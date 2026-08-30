//! Generic desktop executable seam for ClipMesh protocol version 1.
//!
//! Configuration is admitted before local state, platform adapters, or the
//! network are opened. The transport sends only the accepted protocol schema;
//! it has no application identity, enrollment, or credential field.

use std::{
    io,
    net::{IpAddr, SocketAddr, TcpStream},
    path::PathBuf,
    time::Duration,
};

use clipmesh_agent_core::{
    AckCursor, AgentCore, ClipboardAdapter, CoreError, Delivery as CoreDelivery, ObservationResult,
    OutboxItem, PermanentPublishFailure, ReceivedEvent, SessionParameters,
};
use clipmesh_protocol::{
    decode_server_message, ClientMessageV1, Delivery, FailureCode, ProtocolVersion, PublishEventV1,
    ServerMessageV1, U64Decimal, UuidV4,
};
use serde::Deserialize;
use thiserror::Error;
use tungstenite::{
    client::{client, IntoClientRequest},
    http::{header::SEC_WEBSOCKET_PROTOCOL, HeaderValue},
    Message, WebSocket,
};

const WEBSOCKET_PROTOCOL: &str = "clipmesh.v1";
const STREAM_PATH: &str = "/v1/stream";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    LinuxWayland,
    Macos,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentConfig {
    pub hub_url: String,
    pub endpoint: SocketAddr,
    pub platform: Platform,
    pub state_path: PathBuf,
    pub control_socket: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    config_version: u8,
    hub_url: String,
    platform: String,
    state_path: String,
    control_socket: String,
}

impl AgentConfig {
    pub fn parse_toml(input: &str) -> Result<Self, AgentError> {
        let raw: RawConfig = toml::from_str(input).map_err(|error| {
            let text = error.to_string();
            if text.contains("unknown field") {
                AgentError::ConfigUnknownField
            } else if text.contains("missing field") {
                AgentError::ConfigMissingRequired
            } else if text.contains("invalid type") {
                AgentError::ConfigValueInvalid
            } else {
                AgentError::ConfigParseFailed
            }
        })?;
        let endpoint = validate_url(&raw.hub_url)?;
        let platform = match raw.platform.as_str() {
            "linux-wayland" => Platform::LinuxWayland,
            "macos" => Platform::Macos,
            _ => return Err(AgentError::ConfigValueInvalid),
        };
        let state_path = PathBuf::from(raw.state_path);
        let control_socket = PathBuf::from(raw.control_socket);
        if raw.config_version != 1
            || !state_path.is_absolute()
            || !control_socket.is_absolute()
            || state_path == control_socket
        {
            return Err(AgentError::ConfigValueInvalid);
        }
        Ok(Self {
            hub_url: raw.hub_url,
            endpoint,
            platform,
            state_path,
            control_socket,
        })
    }
}

fn validate_url(value: &str) -> Result<SocketAddr, AgentError> {
    let remainder = value
        .strip_prefix("ws://")
        .ok_or(AgentError::ConfigValueInvalid)?;
    let (authority, path) = remainder
        .split_once('/')
        .ok_or(AgentError::ConfigValueInvalid)?;
    if path != &STREAM_PATH[1..]
        || authority.is_empty()
        || authority
            .bytes()
            .any(|byte| matches!(byte, b'@' | b'?' | b'#'))
    {
        return Err(AgentError::ConfigValueInvalid);
    }
    let (host, port) = split_host_port(authority)?;
    let address = match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) if is_tailscale_v4(address) => IpAddr::V4(address),
        Ok(IpAddr::V6(address)) if is_tailscale_v6(address) => IpAddr::V6(address),
        _ => return Err(AgentError::ConfigValueInvalid),
    };
    Ok(SocketAddr::new(address, port))
}

fn split_host_port(authority: &str) -> Result<(&str, u16), AgentError> {
    let (host, port) = if let Some(authority) = authority.strip_prefix('[') {
        let (host, port) = authority
            .split_once("]:")
            .ok_or(AgentError::ConfigValueInvalid)?;
        (host, port)
    } else {
        authority
            .rsplit_once(':')
            .ok_or(AgentError::ConfigValueInvalid)?
    };
    if host.is_empty()
        || port.is_empty()
        || !port.bytes().all(|byte| byte.is_ascii_digit())
        || (!authority.starts_with('[') && host.contains(':'))
    {
        return Err(AgentError::ConfigValueInvalid);
    }
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or(AgentError::ConfigValueInvalid)?;
    Ok((host, port))
}

fn is_tailscale_v4(address: std::net::Ipv4Addr) -> bool {
    let octets = address.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

fn is_tailscale_v6(address: std::net::Ipv6Addr) -> bool {
    let segments = address.segments();
    segments[..3] == [0xfd7a, 0x115c, 0xa1e0]
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum AgentError {
    #[error("config_parse_failed")]
    ConfigParseFailed,
    #[error("config_unknown_field")]
    ConfigUnknownField,
    #[error("config_missing_required")]
    ConfigMissingRequired,
    #[error("config_value_invalid")]
    ConfigValueInvalid,
    #[error("protocol_schema_invalid")]
    ProtocolSchemaInvalid,
    #[error("protocol_version_unsupported")]
    ProtocolVersionUnsupported,
    #[error("state_path_insecure")]
    StatePathInsecure,
    #[error("local_state_unavailable")]
    LocalStateUnavailable,
    #[error("adapter_unavailable")]
    AdapterUnavailable,
    #[error("transport_unavailable")]
    TransportUnavailable,
    #[error("transport_closed")]
    TransportClosed,
    #[error("remote_failure")]
    RemoteFailure,
}

impl From<CoreError> for AgentError {
    fn from(error: CoreError) -> Self {
        match error {
            CoreError::StatePathInsecure => Self::StatePathInsecure,
            CoreError::LocalStateUnavailable => Self::LocalStateUnavailable,
            CoreError::AdapterUnavailable => Self::AdapterUnavailable,
            _ => Self::ProtocolSchemaInvalid,
        }
    }
}

pub trait Transport {
    fn send(&mut self, message: ClientMessageV1) -> Result<(), AgentError>;
    fn receive(&mut self) -> Result<Option<ServerMessageV1>, AgentError>;
}

pub struct WebSocketTransport {
    socket: WebSocket<TcpStream>,
}

impl WebSocketTransport {
    pub fn connect(config: &AgentConfig) -> Result<Self, AgentError> {
        let stream = TcpStream::connect_timeout(&config.endpoint, Duration::from_secs(5))
            .map_err(|_| AgentError::TransportUnavailable)?;
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|_| AgentError::TransportUnavailable)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|_| AgentError::TransportUnavailable)?;
        let request = websocket_request(config)?;
        let (socket, response) =
            client(request, stream).map_err(|_| AgentError::TransportUnavailable)?;
        if response.headers().get(SEC_WEBSOCKET_PROTOCOL)
            != Some(&HeaderValue::from_static(WEBSOCKET_PROTOCOL))
        {
            return Err(AgentError::ProtocolSchemaInvalid);
        }
        Ok(Self { socket })
    }
}

fn websocket_request(config: &AgentConfig) -> Result<tungstenite::http::Request<()>, AgentError> {
    let mut request = config
        .hub_url
        .as_str()
        .into_client_request()
        .map_err(|_| AgentError::ConfigValueInvalid)?;
    request.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static(WEBSOCKET_PROTOCOL),
    );
    Ok(request)
}

impl Transport for WebSocketTransport {
    fn send(&mut self, message: ClientMessageV1) -> Result<(), AgentError> {
        let text =
            serde_json::to_string(&message).map_err(|_| AgentError::ProtocolSchemaInvalid)?;
        self.socket
            .send(Message::Text(text.into()))
            .map_err(|_| AgentError::TransportUnavailable)
    }

    fn receive(&mut self) -> Result<Option<ServerMessageV1>, AgentError> {
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    return decode_server_message(text.as_ref())
                        .map(Some)
                        .map_err(|error| match error {
                            clipmesh_protocol::DecodeError::ProtocolVersionUnsupported => {
                                AgentError::ProtocolVersionUnsupported
                            }
                            clipmesh_protocol::DecodeError::ProtocolSchemaInvalid => {
                                AgentError::ProtocolSchemaInvalid
                            }
                        });
                }
                Ok(Message::Ping(bytes)) => {
                    self.socket
                        .send(Message::Pong(bytes))
                        .map_err(|_| AgentError::TransportUnavailable)?;
                }
                Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) => return Err(AgentError::TransportClosed),
                Ok(_) => return Err(AgentError::ProtocolSchemaInvalid),
                Err(tungstenite::Error::Io(error)) if is_idle(&error) => return Ok(None),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Err(AgentError::TransportClosed)
                }
                Err(_) => return Err(AgentError::TransportUnavailable),
            }
        }
    }
}

fn is_idle(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

pub fn establish_live<T: Transport, A: ClipboardAdapter>(
    core: &mut AgentCore,
    adapter: &mut A,
    transport: &mut T,
    local_time_ms: i64,
) -> Result<(), AgentError> {
    let prior = core.snapshot()?;
    let hello = transport
        .receive()?
        .ok_or(AgentError::TransportUnavailable)?;
    let ServerMessageV1::ServerHello {
        self_peer_id,
        history_epoch,
        clear_generation,
        server_time_ms,
        limits,
        ..
    } = hello
    else {
        return Err(AgentError::ProtocolSchemaInvalid);
    };
    let session_epoch = history_epoch.get();
    let session_generation = clear_generation.get();
    core.set_session(SessionParameters {
        self_peer_id: self_peer_id.as_boundary_value().to_owned(),
        history_epoch: session_epoch,
        clear_generation: session_generation,
        max_payload_bytes: limits.max_payload_bytes as usize,
        retention_seconds: limits.retention_seconds,
        server_time_offset_ms: server_time_ms.saturating_sub(local_time_ms),
    })?;
    transport.send(ClientMessageV1::Resume {
        protocol_version: ProtocolVersion,
        known_history_epoch: prior.history_epoch.map(uuid_v4).transpose()?,
        known_clear_generation: prior.clear_generation.map(decimal).transpose()?,
        after_cursor: prior.last_cursor.map(decimal).transpose()?,
    })?;

    let mut resume_started = false;
    loop {
        let message = transport
            .receive()?
            .ok_or(AgentError::TransportUnavailable)?;
        match message {
            ServerMessageV1::ResumeStarted {
                history_epoch,
                clear_generation,
                ..
            } if !resume_started
                && history_epoch.get() == session_epoch
                && clear_generation.get() == session_generation =>
            {
                resume_started = true
            }
            ServerMessageV1::Event {
                delivery: Delivery::Resume,
                ..
            } if resume_started => {
                receive_event(core, adapter, message, local_time_ms)?;
            }
            ServerMessageV1::ResumeComplete {
                history_epoch,
                clear_generation,
                ..
            } if resume_started
                && history_epoch.get() == session_epoch
                && clear_generation.get() == session_generation =>
            {
                core.finish_resume(local_time_ms);
                for item in core.outbox_for_retry()? {
                    send_publish(transport, item)?;
                }
                return Ok(());
            }
            ServerMessageV1::Error { .. } => return Err(AgentError::RemoteFailure),
            _ => return Err(AgentError::ProtocolSchemaInvalid),
        }
    }
}

pub fn send_observation<T: Transport, A: ClipboardAdapter>(
    core: &mut AgentCore,
    adapter: &mut A,
    observation: clipmesh_agent_core::LocalObservation,
    transport: &mut T,
    local_time_ms: i64,
) -> Result<(), AgentError> {
    if let Some(token) = core.begin_observation(observation) {
        if let ObservationResult::Queued(item) =
            core.commit_observation(token, local_time_ms, adapter)?
        {
            send_publish(transport, item)?;
        }
    }
    Ok(())
}

pub fn send_shared_clear<T: Transport>(
    core: &AgentCore,
    transport: &mut T,
) -> Result<(), AgentError> {
    let expected_clear_generation = core
        .snapshot()?
        .clear_generation
        .ok_or(AgentError::ProtocolSchemaInvalid)?;
    transport.send(ClientMessageV1::ClearHistory {
        protocol_version: ProtocolVersion,
        request_id: uuid_v4(uuid::Uuid::new_v4())?,
        expected_clear_generation: decimal(expected_clear_generation)?,
    })
}

pub fn drive_server_once<T: Transport, A: ClipboardAdapter>(
    core: &mut AgentCore,
    adapter: &mut A,
    transport: &mut T,
    local_time_ms: i64,
) -> Result<(), AgentError> {
    if let Some(message) = transport.receive()? {
        match message {
            ServerMessageV1::Event {
                delivery: Delivery::Live,
                ..
            } => {
                receive_event(core, adapter, message, local_time_ms)?;
            }
            ServerMessageV1::PublishAccepted { message_id, .. } => {
                core.publish_accepted(message_id.get())?;
            }
            ServerMessageV1::PublishRejected {
                message_id: Some(message_id),
                failure,
                ..
            } => {
                core.publish_rejected(
                    message_id.get(),
                    permanent_failure(failure.code()),
                    failure.retryable(),
                )?;
            }
            ServerMessageV1::ClearNotice {
                clear_generation,
                cleared_through_cursor,
                ..
            } => {
                let history_epoch = core
                    .snapshot()?
                    .history_epoch
                    .ok_or(AgentError::ProtocolSchemaInvalid)?;
                core.clear_notice(
                    history_epoch,
                    clear_generation.get(),
                    cleared_through_cursor.map(U64Decimal::get),
                )?;
            }
            ServerMessageV1::ClearAccepted { .. } => {}
            ServerMessageV1::ClearRejected { .. } => return Err(AgentError::RemoteFailure),
            ServerMessageV1::Error { .. } => return Err(AgentError::RemoteFailure),
            _ => return Err(AgentError::ProtocolSchemaInvalid),
        }
    }
    if let Some(ack) = core.poll_ack(local_time_ms) {
        send_ack(transport, ack)?;
    }
    Ok(())
}

fn receive_event<A: ClipboardAdapter>(
    core: &mut AgentCore,
    adapter: &mut A,
    message: ServerMessageV1,
    local_time_ms: i64,
) -> Result<(), AgentError> {
    let ServerMessageV1::Event {
        history_epoch,
        clear_generation,
        cursor,
        delivery,
        accepted_at_ms,
        expires_at_ms,
        source_peer_id,
        event,
        ..
    } = message
    else {
        return Err(AgentError::ProtocolSchemaInvalid);
    };
    core.receive_event(
        ReceivedEvent {
            history_epoch: history_epoch.get(),
            clear_generation: clear_generation.get(),
            cursor: cursor.get(),
            delivery: match delivery {
                Delivery::Resume => CoreDelivery::Resume,
                Delivery::Live => CoreDelivery::Live,
            },
            accepted_at_ms,
            expires_at_ms,
            source_peer_id: source_peer_id.as_boundary_value().to_owned(),
            message_id: event.message_id.get(),
            created_at_ms: event.created_at_ms,
            content_type: event.content_type,
            payload_b64: event.payload_b64,
            payload_bytes: event.payload_bytes as usize,
            content_sha256: event.content_sha256,
        },
        local_time_ms,
        adapter,
    )?;
    Ok(())
}

fn send_publish<T: Transport>(transport: &mut T, item: OutboxItem) -> Result<(), AgentError> {
    let wire = item.event.content.to_wire();
    transport.send(ClientMessageV1::Publish {
        protocol_version: ProtocolVersion,
        event: PublishEventV1 {
            message_id: uuid_v4(item.event.message_id)?,
            clear_generation: decimal(item.event.clear_generation)?,
            created_at_ms: item.event.created_at_ms,
            content_type: wire.content_type.to_owned(),
            payload_bytes: wire
                .payload_bytes
                .try_into()
                .map_err(|_| AgentError::ProtocolSchemaInvalid)?,
            content_sha256: wire.content_sha256,
            payload_b64: wire.payload_b64,
        },
    })
}

fn send_ack<T: Transport>(transport: &mut T, ack: AckCursor) -> Result<(), AgentError> {
    transport.send(ClientMessageV1::Ack {
        protocol_version: ProtocolVersion,
        history_epoch: uuid_v4(ack.history_epoch)?,
        clear_generation: decimal(ack.clear_generation)?,
        cursor: decimal(ack.cursor)?,
    })
}

fn decimal(value: u64) -> Result<U64Decimal, AgentError> {
    U64Decimal::new(value).map_err(|_| AgentError::ProtocolSchemaInvalid)
}

fn uuid_v4(value: uuid::Uuid) -> Result<UuidV4, AgentError> {
    UuidV4::from_uuid(value).map_err(|_| AgentError::ProtocolSchemaInvalid)
}

fn permanent_failure(code: FailureCode) -> PermanentPublishFailure {
    match code {
        FailureCode::MessageIdReplay | FailureCode::MessageIdConflict => {
            PermanentPublishFailure::Replay
        }
        FailureCode::ClearGenerationStale => PermanentPublishFailure::StaleGeneration,
        _ => PermanentPublishFailure::Validation,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, fs, os::unix::fs::PermissionsExt};

    use clipmesh_agent_core::{AdapterError, AgentState, PlatformRevision};
    use clipmesh_protocol::{LimitsV1, ResumeStatus, StablePeerId};
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;

    const VALID: &str = "config_version = 1\nhub_url = 'ws://100.64.0.7:4357/v1/stream'\nplatform = 'linux-wayland'\nstate_path = '/var/lib/clipmesh/agent.sqlite3'\ncontrol_socket = '/var/run/user/1000/clipmesh.sock'\n";

    #[test]
    fn desktop_config_is_closed_and_rejected_before_transport() {
        let cases = [
            ("", AgentError::ConfigMissingRequired),
            (
                &VALID.replace("config_version = 1\n", ""),
                AgentError::ConfigMissingRequired,
            ),
            (
                &format!("{VALID}unexpected = true\n"),
                AgentError::ConfigUnknownField,
            ),
            (
                &VALID.replace("ws://", "wss://"),
                AgentError::ConfigValueInvalid,
            ),
            (
                &VALID.replace("100.64.0.7", "hub.example.invalid"),
                AgentError::ConfigValueInvalid,
            ),
            (
                &VALID.replace("100.64.0.7", "192.0.2.7"),
                AgentError::ConfigValueInvalid,
            ),
            (&VALID.replace(":4357", ""), AgentError::ConfigValueInvalid),
            (
                &VALID.replace("/v1/stream", "/v1/other"),
                AgentError::ConfigValueInvalid,
            ),
            (
                &VALID.replace("100.64.0.7", "user@100.64.0.7"),
                AgentError::ConfigValueInvalid,
            ),
            (
                &VALID.replace(":4357", ":65536"),
                AgentError::ConfigValueInvalid,
            ),
            (
                &VALID.replace("linux-wayland", "windows"),
                AgentError::ConfigValueInvalid,
            ),
        ];
        let mut transport_opens = 0;
        for (input, expected) in cases {
            let result = (|| {
                let config = AgentConfig::parse_toml(input)?;
                transport_opens += 1;
                Ok::<_, AgentError>(config)
            })();
            assert_eq!(result.unwrap_err(), expected);
        }
        assert_eq!(transport_opens, 0);
        assert_eq!(
            AgentConfig::parse_toml(&VALID.replace("100.64.0.7", "[fd7a:115c:a1e0::7]"))
                .unwrap()
                .endpoint
                .port(),
            4357
        );
        assert_eq!(
            AgentConfig::parse_toml(&VALID.replace(":4357", ":80"))
                .unwrap()
                .endpoint
                .port(),
            80
        );
    }

    #[test]
    fn websocket_request_has_only_transport_and_protocol_headers() {
        let config = AgentConfig::parse_toml(VALID).unwrap();
        let request = websocket_request(&config).unwrap();
        let names: Vec<_> = request.headers().keys().map(|name| name.as_str()).collect();
        assert!(names.iter().all(|name| matches!(
            *name,
            "host"
                | "connection"
                | "upgrade"
                | "sec-websocket-version"
                | "sec-websocket-key"
                | "sec-websocket-protocol"
        )));
        assert_eq!(
            request.headers().get(SEC_WEBSOCKET_PROTOCOL).unwrap(),
            WEBSOCKET_PROTOCOL
        );
    }

    #[test]
    fn isolated_generic_delivery_reaches_live_without_identity_fields() {
        let directory = TempDir::new().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let mut core = AgentCore::open(&directory.path().join("agent.sqlite3")).unwrap();
        core.start_unlocked();
        let epoch = Uuid::parse_str("00000000-0000-4000-8000-000000000003").unwrap();
        let mut transport = FakeTransport::new(vec![
            ServerMessageV1::ServerHello {
                protocol_version: ProtocolVersion,
                session_id: uuid_v4(
                    Uuid::parse_str("00000000-0000-4000-8000-000000000004").unwrap(),
                )
                .unwrap(),
                self_peer_id: StablePeerId::from_boundary("peer-from-whois").unwrap(),
                history_epoch: uuid_v4(epoch).unwrap(),
                clear_generation: decimal(1).unwrap(),
                newest_cursor: None,
                server_time_ms: 1_700_000_000_000,
                limits: LimitsV1::new(262_144, 604_800, 500, 353_624).unwrap(),
            },
            ServerMessageV1::ResumeStarted {
                protocol_version: ProtocolVersion,
                history_epoch: uuid_v4(epoch).unwrap(),
                clear_generation: decimal(1).unwrap(),
                status: ResumeStatus::Fresh,
                requested_after_cursor: None,
                boundary_cursor: None,
                lost_through_cursor: None,
            },
            ServerMessageV1::ResumeComplete {
                protocol_version: ProtocolVersion,
                history_epoch: uuid_v4(epoch).unwrap(),
                clear_generation: decimal(1).unwrap(),
                boundary_cursor: None,
            },
        ]);
        establish_live(
            &mut core,
            &mut FakeClipboard,
            &mut transport,
            1_700_000_000_000,
        )
        .unwrap();
        assert_eq!(core.state(), AgentState::ActiveUnlockedLive);
        assert_eq!(transport.sent.len(), 1);
        let serialized = serde_json::to_string(&transport.sent[0]).unwrap();
        assert_eq!(
            serialized,
            r#"{"type":"resume","protocol_version":1,"known_history_epoch":null,"known_clear_generation":null,"after_cursor":null}"#
        );
        assert!(!serialized.contains("identity"));
        assert!(!serialized.contains("credential"));
        assert!(!serialized.contains("account"));

        send_shared_clear(&core, &mut transport).unwrap();
        let ClientMessageV1::ClearHistory {
            request_id,
            expected_clear_generation,
            ..
        } = &transport.sent[1]
        else {
            panic!("owner shared clear emitted the wrong protocol message");
        };
        assert_eq!(expected_clear_generation.get(), 1);
        transport
            .received
            .push_back(ServerMessageV1::ClearAccepted {
                protocol_version: ProtocolVersion,
                request_id: request_id.clone(),
                clear_generation: decimal(2).unwrap(),
                cleared_through_cursor: None,
                duplicate: false,
            });
        drive_server_once(
            &mut core,
            &mut FakeClipboard,
            &mut transport,
            1_700_000_000_001,
        )
        .unwrap();
    }

    struct FakeTransport {
        received: VecDeque<ServerMessageV1>,
        sent: Vec<ClientMessageV1>,
    }

    impl FakeTransport {
        fn new(received: Vec<ServerMessageV1>) -> Self {
            Self {
                received: received.into(),
                sent: Vec::new(),
            }
        }
    }

    impl Transport for FakeTransport {
        fn send(&mut self, message: ClientMessageV1) -> Result<(), AgentError> {
            self.sent.push(message);
            Ok(())
        }

        fn receive(&mut self) -> Result<Option<ServerMessageV1>, AgentError> {
            Ok(self.received.pop_front())
        }
    }

    struct FakeClipboard;

    impl ClipboardAdapter for FakeClipboard {
        fn is_current(&mut self, _: &PlatformRevision) -> Result<bool, AdapterError> {
            Ok(true)
        }

        fn write_text(&mut self, _: &[u8]) -> Result<PlatformRevision, AdapterError> {
            Ok(PlatformRevision::synthetic("written"))
        }
    }
}
