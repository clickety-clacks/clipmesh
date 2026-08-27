//! Tailnet-only HTTP and WebSocket edge for the transport-neutral hub core.
//!
//! This crate deliberately has no executable and never starts a listener by
//! itself. A deployment-owned process may explicitly create the dormant
//! listener through [`HubEdge::bind`]. That listener owns both accepted socket
//! addresses and obtains WhoIs before it reads one HTTP byte.

use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use clipmesh_hub_core::{
    ClearAccepted, ClipContentV1, CoreError, FailureCode as CoreFailureCode, HubCore, PublishInput,
    RetainedClip, SessionEvent, StablePeerId,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use thiserror::Error;
use uuid::{Uuid, Variant, Version};

const PROTOCOL: &str = "clipmesh.v1";
const MAX_HEADERS: usize = 16_384;

const SYSTEM_LOCALAPI_SOCKET: &str = "/var/run/tailscale/tailscaled.sock";
const LOCALAPI_RESPONSE_LIMIT: usize = 131_072;

/// Concrete owner of the host-local Tailscale daemon socket.
///
/// It has no TCP fallback and deliberately returns only the two values the
/// edge is authorized to consume: local Tailnet addresses and a peer StableID.
#[derive(Clone, Debug)]
pub struct SystemLocalApi {
    socket_path: PathBuf,
}

impl SystemLocalApi {
    pub fn system() -> Self {
        Self::from_socket_path(SYSTEM_LOCALAPI_SOCKET)
    }

    /// This constructor exists for process-local daemon simulation only. It
    /// does not create, bind, or activate a listener.
    pub fn from_socket_path(path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: path.into(),
        }
    }

    fn self_addresses(&self) -> Result<Vec<IpAddr>, LocalApiError> {
        let value = self.request("/localapi/v0/status")?;
        let addresses = value
            .get("TailscaleIPs")
            .and_then(Value::as_array)
            .ok_or(LocalApiError::MalformedResponse)?;
        let mut parsed = Vec::with_capacity(addresses.len());
        for address in addresses {
            parsed.push(
                address
                    .as_str()
                    .and_then(|address| address.parse().ok())
                    .ok_or(LocalApiError::MalformedResponse)?,
            );
        }
        Ok(parsed)
    }

    fn who_is(&self, remote: SocketAddr) -> Result<String, LocalApiError> {
        let value = self.request(&format!("/localapi/v0/whois?addr={remote}"))?;
        value
            .get("Node")
            .and_then(Value::as_object)
            .and_then(|node| node.get("StableID"))
            .and_then(Value::as_str)
            .filter(|stable_id| !stable_id.is_empty())
            .map(ToOwned::to_owned)
            .ok_or(LocalApiError::PeerNotFound)
    }

    fn request(&self, target: &str) -> Result<Value, LocalApiError> {
        let mut stream = UnixStream::connect(&self.socket_path).map_err(LocalApiError::from_io)?;
        let timeout = Some(Duration::from_secs(2));
        stream
            .set_read_timeout(timeout)
            .map_err(LocalApiError::from_io)?;
        stream
            .set_write_timeout(timeout)
            .map_err(LocalApiError::from_io)?;
        stream
            .write_all(
                format!(
                    "GET {target} HTTP/1.1\\r\\nHost: local-tailscaled\\r\\nConnection: close\\r\\n\\r\\n"
                )
                .replace(r"\\r\\n", "\r\n")
                .as_bytes(),
            )
            .map_err(LocalApiError::from_io)?;
        let mut response = Vec::new();
        stream
            .take((LOCALAPI_RESPONSE_LIMIT + 1) as u64)
            .read_to_end(&mut response)
            .map_err(LocalApiError::from_io)?;
        if response.len() > LOCALAPI_RESPONSE_LIMIT {
            return Err(LocalApiError::MalformedResponse);
        }
        let response =
            std::str::from_utf8(&response).map_err(|_| LocalApiError::MalformedResponse)?;
        let (head, body) = response
            .split_once("\r\n\r\n")
            .ok_or(LocalApiError::MalformedResponse)?;
        if !head.starts_with("HTTP/1.1 200 ") && !head.starts_with("HTTP/1.0 200 ") {
            return Err(LocalApiError::PeerNotFound);
        }
        serde_json::from_str(body).map_err(|_| LocalApiError::MalformedResponse)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalApiError {
    Unavailable,
    PermissionDenied,
    PeerNotFound,
    MalformedResponse,
    TimedOut,
}

impl LocalApiError {
    fn from_io(error: std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => Self::TimedOut,
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            _ => Self::Unavailable,
        }
    }
}

/// Closed, generic version-1 hub configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EdgeConfig {
    pub listen_address: SocketAddr,
    pub state_directory: PathBuf,
    pub retention_seconds: u64,
    pub history_max_entries: usize,
    pub max_payload_bytes: usize,
    pub max_connections: usize,
    pub max_connections_per_peer: usize,
    pub publish_tokens_per_minute: u32,
    pub publish_burst: u32,
    pub outbound_queue_messages: usize,
    pub outbound_queue_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    config_version: u8,
    listen_address: String,
    #[serde(default = "system_localapi")]
    tailscale_localapi: String,
    state_directory: String,
    #[serde(default = "default_retention")]
    retention_seconds: u64,
    #[serde(default = "default_history")]
    history_max_entries: usize,
    #[serde(default = "default_payload")]
    max_payload_bytes: usize,
    #[serde(default = "default_connections")]
    max_connections: usize,
    #[serde(default = "default_peer_connections")]
    max_connections_per_peer: usize,
    #[serde(default = "default_publish_tokens")]
    publish_tokens_per_minute: u32,
    #[serde(default = "default_publish_burst")]
    publish_burst: u32,
    #[serde(default = "default_queue_messages")]
    outbound_queue_messages: usize,
    #[serde(default = "default_queue_bytes")]
    outbound_queue_bytes: usize,
}

fn system_localapi() -> String {
    "system".to_owned()
}
fn default_retention() -> u64 {
    604_800
}
fn default_history() -> usize {
    500
}
fn default_payload() -> usize {
    262_144
}
fn default_connections() -> usize {
    64
}
fn default_peer_connections() -> usize {
    2
}
fn default_publish_tokens() -> u32 {
    60
}
fn default_publish_burst() -> u32 {
    10
}
fn default_queue_messages() -> usize {
    64
}
fn default_queue_bytes() -> usize {
    2_097_152
}

impl EdgeConfig {
    pub fn parse_toml(input: &str) -> Result<Self, EdgeError> {
        let raw: RawConfig = toml::from_str(input).map_err(|_| EdgeError::ConfigParseFailed)?;
        if raw.config_version != 1
            || raw.tailscale_localapi != "system"
            || raw.state_directory.is_empty()
        {
            return Err(EdgeError::ConfigValueInvalid);
        }
        let listen_address = raw
            .listen_address
            .parse()
            .map_err(|_| EdgeError::ConfigValueInvalid)?;
        let config = Self {
            listen_address,
            state_directory: PathBuf::from(raw.state_directory),
            retention_seconds: raw.retention_seconds,
            history_max_entries: raw.history_max_entries,
            max_payload_bytes: raw.max_payload_bytes,
            max_connections: raw.max_connections,
            max_connections_per_peer: raw.max_connections_per_peer,
            publish_tokens_per_minute: raw.publish_tokens_per_minute,
            publish_burst: raw.publish_burst,
            outbound_queue_messages: raw.outbound_queue_messages,
            outbound_queue_bytes: raw.outbound_queue_bytes,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), EdgeError> {
        if self.listen_address.port() == 0
            || self.state_directory.as_os_str().is_empty()
            || !(60..=31_536_000).contains(&self.retention_seconds)
            || !(1..=10_000).contains(&self.history_max_entries)
            || !(1..=1_048_576).contains(&self.max_payload_bytes)
            || !(1..=1024).contains(&self.max_connections)
            || !(1..=8).contains(&self.max_connections_per_peer)
            || self.max_connections_per_peer > self.max_connections
            || !(1..=600).contains(&self.publish_tokens_per_minute)
            || !(1..=100).contains(&self.publish_burst)
            || !(1..=256).contains(&self.outbound_queue_messages)
            || !(self.maximum_message_bytes()..=16_777_216).contains(&self.outbound_queue_bytes)
        {
            return Err(EdgeError::ConfigValueInvalid);
        }
        Ok(())
    }

    pub fn maximum_message_bytes(&self) -> usize {
        4 * self.max_payload_bytes.div_ceil(3) + 4096
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EdgeError {
    ConfigParseFailed,
    ConfigValueInvalid,
    TailnetBindUnverified,
    TailscaleLocalapiUnavailable,
    TailnetPeerUnverified,
    ConnectionLimitReached,
    RequestRateLimited,
    PublishRateLimited,
    MessageTooLarge,
    MessageRateLimited,
    SlowConsumer,
    ResumeDeadlineExceeded,
    HeartbeatTimedOut,
    BindFailed,
    ProtocolVersionUnsupported,
    ProtocolSchemaInvalid,
    ResumeRequired,
    ResumeContextIncomplete,
    ResumeCursorWithoutContext,
    CursorAhead,
    SessionContextStale,
    ClearGenerationStale,
    ClearGenerationAhead,
    ClearGenerationExhausted,
    RequestIdConflict,
    MessageIdConflict,
    MessageIdReplay,
    CreatedAtInFuture,
    EventTooOld,
    ContentTypeUnsupported,
    PayloadEmpty,
    PayloadTooLarge,
    PayloadEncodingInvalid,
    PayloadLengthMismatch,
    PayloadHashMismatch,
    HubCursorExhausted,
    AckInvalid,
    StorageUnavailable,
    DatabaseSchemaUnsupported,
    DatabaseIntegrityFailed,
    OutputFailed,
}

impl EdgeError {
    pub fn code(self) -> &'static str {
        match self {
            Self::ConfigParseFailed => "config_parse_failed",
            Self::ConfigValueInvalid => "config_value_invalid",
            Self::TailnetBindUnverified => "tailnet_bind_unverified",
            Self::TailscaleLocalapiUnavailable => "tailscale_localapi_unavailable",
            Self::TailnetPeerUnverified => "tailnet_peer_unverified",
            Self::ConnectionLimitReached => "connection_limit_reached",
            Self::RequestRateLimited => "request_rate_limited",
            Self::PublishRateLimited => "publish_rate_limited",
            Self::MessageTooLarge => "message_too_large",
            Self::MessageRateLimited => "message_rate_limited",
            Self::SlowConsumer => "slow_consumer",
            Self::ResumeDeadlineExceeded => "resume_deadline_exceeded",
            Self::HeartbeatTimedOut => "heartbeat_timeout",
            Self::BindFailed => "bind_failed",
            Self::ProtocolVersionUnsupported => "protocol_version_unsupported",
            Self::ProtocolSchemaInvalid => "protocol_schema_invalid",
            Self::ResumeRequired => "resume_required",
            Self::ResumeContextIncomplete => "resume_context_incomplete",
            Self::ResumeCursorWithoutContext => "resume_cursor_without_context",
            Self::CursorAhead => "cursor_ahead",
            Self::SessionContextStale => "session_context_stale",
            Self::ClearGenerationStale => "clear_generation_stale",
            Self::ClearGenerationAhead => "clear_generation_ahead",
            Self::ClearGenerationExhausted => "clear_generation_exhausted",
            Self::RequestIdConflict => "request_id_conflict",
            Self::MessageIdConflict => "message_id_conflict",
            Self::MessageIdReplay => "message_id_replay",
            Self::CreatedAtInFuture => "created_at_in_future",
            Self::EventTooOld => "event_too_old",
            Self::ContentTypeUnsupported => "content_type_unsupported",
            Self::PayloadEmpty => "payload_empty",
            Self::PayloadTooLarge => "payload_too_large",
            Self::PayloadEncodingInvalid => "payload_encoding_invalid",
            Self::PayloadLengthMismatch => "payload_length_mismatch",
            Self::PayloadHashMismatch => "payload_hash_mismatch",
            Self::HubCursorExhausted => "hub_cursor_exhausted",
            Self::AckInvalid => "ack_invalid",
            Self::StorageUnavailable => "storage_unavailable",
            Self::DatabaseSchemaUnsupported => "database_schema_unsupported",
            Self::DatabaseIntegrityFailed => "database_integrity_failed",
            Self::OutputFailed => "storage_unavailable",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::TailscaleLocalapiUnavailable
                | Self::TailnetPeerUnverified
                | Self::ConnectionLimitReached
                | Self::RequestRateLimited
                | Self::PublishRateLimited
                | Self::MessageRateLimited
                | Self::SlowConsumer
                | Self::SessionContextStale
                | Self::ClearGenerationAhead
                | Self::CreatedAtInFuture
                | Self::StorageUnavailable
                | Self::DatabaseIntegrityFailed
        )
    }

    pub fn websocket_close_code(self) -> u16 {
        match self {
            Self::MessageRateLimited | Self::SlowConsumer => 4429,
            Self::ResumeDeadlineExceeded | Self::HeartbeatTimedOut => 4408,
            Self::SessionContextStale | Self::ClearGenerationStale | Self::ClearGenerationAhead => {
                4409
            }
            Self::TailscaleLocalapiUnavailable
            | Self::StorageUnavailable
            | Self::HubCursorExhausted
            | Self::ClearGenerationExhausted => 4500,
            _ => 4400,
        }
    }
}

#[derive(Debug, Error)]
#[error("clipmesh hub edge failure: {0:?}")]
pub struct EdgeFailure(pub EdgeError);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub header_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    fn error(status: u16, error: EdgeError) -> Self {
        Self {
            status,
            body: format!(
                "{{\"protocol_version\":1,\"error\":{{\"code\":\"{}\",\"retryable\":{}}}}}",
                error.code(),
                error.retryable()
            ),
        }
    }
}

/// Proof that WhoIs has completed for this exact accepted socket.
pub struct AdmittedSocket {
    peer_id: StablePeerId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionHandle {
    pub id: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpgradeResult {
    Response(HttpResponse),
    Upgraded(SessionHandle),
}

/// A completed decision for one edge-owned accepted socket.
pub enum ServedConnection {
    Rejected(HttpResponse),
    Upgraded {
        session: SessionHandle,
        websocket: WebSocketConnection,
    },
}

#[derive(Default)]
struct Sessions {
    entries: HashMap<Uuid, EdgeSession>,
}

struct EdgeSession {
    peer_id: StablePeerId,
    opened_at_ms: i64,
    last_activity_ms: i64,
    last_pong_ms: i64,
    awaiting_resume: bool,
    message_window_started_ms: i64,
    message_tokens: u32,
    publish_window_started_ms: i64,
    publish_tokens: u32,
}

#[derive(Default)]
struct Runtime {
    not_ready: Option<EdgeError>,
    connection_attempts: HashMap<StablePeerId, RateBucket>,
    http_requests: HashMap<StablePeerId, RateBucket>,
}

struct RateBucket {
    tokens: u32,
    updated_at_ms: i64,
}

impl RateBucket {
    fn take(&mut self, now_ms: i64, per_minute: u32, burst: u32) -> bool {
        let elapsed_ms = now_ms.saturating_sub(self.updated_at_ms);
        let refill = (elapsed_ms.saturating_mul(i64::from(per_minute)) / 60_000) as u32;
        if refill > 0 {
            self.updated_at_ms = now_ms;
            self.tokens = self.tokens.saturating_add(refill).min(burst);
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

/// Owns the one configured application listener. It is inert until a caller
/// explicitly invokes [`HubEdge::bind`]; no executable activates it by default.
pub struct HubListener {
    edge: HubEdge,
    listener: TcpListener,
}

/// Concrete WebSocket owner. It writes a complete server text frame before a
/// leased core event can be completed, so no caller can retain a payload after
/// the generation seam releases it.
pub struct WebSocketConnection {
    stream: TcpStream,
}

enum InboundFrame {
    Text(String),
    Pong,
    Close,
}

impl WebSocketConnection {
    fn from_upgraded(stream: TcpStream) -> Self {
        Self { stream }
    }

    fn write_complete_text(&mut self, frame: &str) -> std::io::Result<()> {
        let bytes = frame.as_bytes();
        let mut header = Vec::with_capacity(10);
        header.push(0x81);
        match bytes.len() {
            0..=125 => header.push(bytes.len() as u8),
            126..=65_535 => {
                header.push(126);
                header.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            }
            _ => {
                header.push(127);
                header.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
            }
        }
        self.stream.write_all(&header)?;
        self.stream.write_all(bytes)?;
        self.stream.flush()
    }

    fn write_ping(&mut self) -> std::io::Result<()> {
        self.stream.write_all(&[0x89, 0])?;
        self.stream.flush()
    }

    fn write_close(&mut self, error: EdgeError) -> std::io::Result<()> {
        let mut payload = error.websocket_close_code().to_be_bytes().to_vec();
        payload.extend_from_slice(error.code().as_bytes());
        self.stream.write_all(&[0x88, payload.len() as u8])?;
        self.stream.write_all(&payload)?;
        self.stream.flush()
    }

    fn read_complete_frame(&mut self, maximum_bytes: usize) -> Result<InboundFrame, EdgeError> {
        let mut header = [0_u8; 2];
        self.stream
            .read_exact(&mut header)
            .map_err(|_| EdgeError::OutputFailed)?;
        if !matches!(header[0], 0x81 | 0x8a | 0x88) || header[1] & 0x80 == 0 {
            return Err(EdgeError::ProtocolSchemaInvalid);
        }
        let length = match header[1] & 0x7f {
            value @ 0..=125 => value as usize,
            126 => {
                let mut bytes = [0_u8; 2];
                self.stream
                    .read_exact(&mut bytes)
                    .map_err(|_| EdgeError::OutputFailed)?;
                u16::from_be_bytes(bytes) as usize
            }
            127 => {
                let mut bytes = [0_u8; 8];
                self.stream
                    .read_exact(&mut bytes)
                    .map_err(|_| EdgeError::OutputFailed)?;
                usize::try_from(u64::from_be_bytes(bytes))
                    .map_err(|_| EdgeError::MessageTooLarge)?
            }
            _ => unreachable!("WebSocket length uses seven bits"),
        };
        if length > maximum_bytes {
            return Err(EdgeError::MessageTooLarge);
        }
        let mut mask = [0_u8; 4];
        self.stream
            .read_exact(&mut mask)
            .map_err(|_| EdgeError::OutputFailed)?;
        let mut payload = vec![0_u8; length];
        self.stream
            .read_exact(&mut payload)
            .map_err(|_| EdgeError::OutputFailed)?;
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % mask.len()];
        }
        match header[0] {
            0x81 => String::from_utf8(payload)
                .map(InboundFrame::Text)
                .map_err(|_| EdgeError::ProtocolSchemaInvalid),
            0x8a if payload.is_empty() => Ok(InboundFrame::Pong),
            0x88 => Ok(InboundFrame::Close),
            _ => Err(EdgeError::ProtocolSchemaInvalid),
        }
    }
}

pub struct HubEdge {
    config: EdgeConfig,
    local_api: SystemLocalApi,
    core: HubCore,
    sessions: Mutex<Sessions>,
    runtime: Mutex<Runtime>,
}

impl HubEdge {
    /// Validates config and current LocalAPI status before opening hub state.
    /// It does not bind or activate a listener.
    fn prepare(
        config: EdgeConfig,
        local_api: SystemLocalApi,
        database: impl AsRef<Path>,
    ) -> Result<Self, EdgeFailure> {
        config.validate().map_err(EdgeFailure)?;
        let self_addresses = local_api
            .self_addresses()
            .map_err(|_| EdgeFailure(EdgeError::TailscaleLocalapiUnavailable))?;
        if !self_addresses
            .iter()
            .any(|address| *address == config.listen_address.ip())
            || disallowed_bind_ip(config.listen_address.ip())
        {
            return Err(EdgeFailure(EdgeError::TailnetBindUnverified));
        }
        let limits = clipmesh_hub_core::RetentionLimits {
            retention_seconds: config.retention_seconds,
            history_max_entries: config.history_max_entries,
            max_payload_bytes: config.max_payload_bytes,
        };
        let core =
            HubCore::open(database, limits).map_err(|error| EdgeFailure(core_error(error)))?;
        Ok(Self {
            config,
            local_api,
            core,
            sessions: Mutex::new(Sessions::default()),
            runtime: Mutex::new(Runtime::default()),
        })
    }

    /// Binds only the LocalAPI-verified configured address and derives both
    /// socket identities internally on every accept.
    pub fn bind(config: EdgeConfig, local_api: SystemLocalApi) -> Result<HubListener, EdgeFailure> {
        let database = config.state_directory.join("hub.sqlite");
        let edge = Self::prepare(config, local_api, database)?;
        let listener = TcpListener::bind(edge.config.listen_address)
            .map_err(|_| EdgeFailure(EdgeError::BindFailed))?;
        if listener
            .local_addr()
            .map_err(|_| EdgeFailure(EdgeError::BindFailed))?
            != edge.config.listen_address
        {
            return Err(EdgeFailure(EdgeError::BindFailed));
        }
        Ok(HubListener { edge, listener })
    }

    pub fn config(&self) -> &EdgeConfig {
        &self.config
    }

    /// Calls WhoIs on the peer address observed from an accepted socket.
    fn admit_socket(&self, observed_remote: SocketAddr) -> Result<AdmittedSocket, EdgeFailure> {
        let stable_id = self
            .local_api
            .who_is(observed_remote)
            .map_err(|_| EdgeFailure(EdgeError::TailnetPeerUnverified))?;
        let peer_id = StablePeerId::from_boundary(stable_id)
            .map_err(|_| EdgeFailure(EdgeError::TailnetPeerUnverified))?;
        Ok(AdmittedSocket { peer_id })
    }

    /// Validates an admitted HTTP request in the specified precedence order.
    fn upgrade(&self, admitted: AdmittedSocket, request: &HttpRequest) -> UpgradeResult {
        if let Some(response) = validate_http(request) {
            return UpgradeResult::Response(response);
        }
        let mut sessions = self.sessions.lock().expect("edge sessions lock poisoned");
        let peer_count = sessions
            .entries
            .values()
            .filter(|session| session.peer_id == admitted.peer_id)
            .count();
        if sessions.entries.len() >= self.config.max_connections
            || peer_count >= self.config.max_connections_per_peer
        {
            return UpgradeResult::Response(HttpResponse::error(
                429,
                EdgeError::ConnectionLimitReached,
            ));
        }
        let hello = self.core.open_session(admitted.peer_id.clone());
        sessions.entries.insert(
            hello.session_id,
            EdgeSession {
                peer_id: admitted.peer_id,
                opened_at_ms: 0,
                last_activity_ms: 0,
                last_pong_ms: 0,
                awaiting_resume: true,
                message_window_started_ms: 0,
                message_tokens: 20,
                publish_window_started_ms: 0,
                publish_tokens: self.config.publish_burst,
            },
        );
        UpgradeResult::Upgraded(SessionHandle {
            id: hello.session_id,
        })
    }

    /// Runs the production WhoIs, HTTP, and WebSocket handshake path for an
    /// already-accepted socket. It calls WhoIs before consuming HTTP bytes and
    /// emits `server_hello` as the first WebSocket text frame.
    fn serve_accepted(
        &self,
        mut stream: TcpStream,
        now_ms: i64,
    ) -> Result<ServedConnection, EdgeFailure> {
        if stream
            .local_addr()
            .map_err(|_| EdgeFailure(EdgeError::TailnetBindUnverified))?
            != self.config.listen_address
        {
            return Err(EdgeFailure(EdgeError::TailnetBindUnverified));
        }
        self.require_ready()?;
        let observed_remote = stream
            .peer_addr()
            .map_err(|_| EdgeFailure(EdgeError::TailnetPeerUnverified))?;
        let admitted = self.admit_socket(observed_remote)?;
        if !self.consume_connection_attempt(&admitted.peer_id, now_ms) {
            return Err(EdgeFailure(EdgeError::RequestRateLimited));
        }
        let request = read_http_request(&mut stream)
            .map_err(|_| EdgeFailure(EdgeError::ProtocolSchemaInvalid))?;
        if !self.consume_http_request(&admitted.peer_id, now_ms) {
            let response = HttpResponse::error(429, EdgeError::RequestRateLimited);
            write_http_response(&mut stream, &response)
                .map_err(|_| EdgeFailure(EdgeError::OutputFailed))?;
            return Ok(ServedConnection::Rejected(response));
        }
        if request.method == "GET" && request.target == "/healthz" {
            let response = self.health();
            write_http_response(&mut stream, &response)
                .map_err(|_| EdgeFailure(EdgeError::OutputFailed))?;
            return Ok(ServedConnection::Rejected(response));
        }
        if request.method == "GET" && request.target == "/readyz" {
            let response = self.readiness();
            write_http_response(&mut stream, &response)
                .map_err(|_| EdgeFailure(EdgeError::OutputFailed))?;
            return Ok(ServedConnection::Rejected(response));
        }
        if let Some(response) = validate_http(&request) {
            write_http_response(&mut stream, &response)
                .map_err(|_| EdgeFailure(EdgeError::OutputFailed))?;
            return Ok(ServedConnection::Rejected(response));
        }
        let session = match self.upgrade(admitted, &request) {
            UpgradeResult::Upgraded(session) => session,
            UpgradeResult::Response(response) => {
                write_http_response(&mut stream, &response)
                    .map_err(|_| EdgeFailure(EdgeError::OutputFailed))?;
                return Ok(ServedConnection::Rejected(response));
            }
        };
        let key = request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("sec-websocket-key"))
            .map(|(_, value)| value.as_str())
            .ok_or(EdgeFailure(EdgeError::ProtocolSchemaInvalid))?;
        write_upgrade_response(&mut stream, key)
            .map_err(|_| EdgeFailure(EdgeError::OutputFailed))?;
        let mut websocket = WebSocketConnection::from_upgraded(stream);
        websocket
            .write_complete_text(&self.server_hello(session, now_ms)?)
            .map_err(|_| EdgeFailure(EdgeError::OutputFailed))?;
        let mut sessions = self.sessions.lock().expect("edge sessions lock poisoned");
        let entry = sessions
            .entries
            .get_mut(&session.id)
            .expect("session just inserted");
        entry.opened_at_ms = now_ms;
        entry.last_activity_ms = now_ms;
        entry.last_pong_ms = now_ms;
        Ok(ServedConnection::Upgraded { session, websocket })
    }

    pub fn server_hello(&self, session: SessionHandle, now_ms: i64) -> Result<String, EdgeFailure> {
        let peer = self.session_peer(session.id)?;
        let limits = self.core.limits();
        Ok(json!({"protocol_version": 1, "type": "server_hello", "session_id": session.id.to_string(), "self_peer_id": peer.as_boundary_value(), "history_epoch": self.core.history_epoch().to_string(), "clear_generation": self.core.clear_generation().to_string(), "newest_cursor": self.core.newest_cursor().map(|value| value.to_string()), "server_time_ms": now_ms, "limits": {"max_payload_bytes": limits.max_payload_bytes, "retention_seconds": limits.retention_seconds, "history_max_entries": limits.history_max_entries, "max_clock_skew_ms": 120000, "max_websocket_message_bytes": self.config.maximum_message_bytes()}}).to_string())
    }

    /// Parses and applies one complete WebSocket text message. The caller must
    /// reject binary or invalid UTF-8 frames before this method.
    pub fn handle_text(
        &self,
        session: SessionHandle,
        text: &str,
        now_ms: i64,
    ) -> Result<(), EdgeFailure> {
        self.require_ready()?;
        if text.len() > self.config.maximum_message_bytes() {
            return Err(EdgeFailure(EdgeError::MessageTooLarge));
        }
        let value: Value = serde_json::from_str(text)
            .map_err(|_| EdgeFailure(EdgeError::ProtocolSchemaInvalid))?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(EdgeFailure(EdgeError::ProtocolSchemaInvalid))?;
        self.consume_message_token(session.id, now_ms, kind == "publish")?;
        {
            let mut sessions = self.sessions.lock().expect("edge sessions lock poisoned");
            let entry = sessions
                .entries
                .get_mut(&session.id)
                .ok_or(EdgeFailure(EdgeError::SessionContextStale))?;
            if entry.opened_at_ms == 0 {
                entry.opened_at_ms = now_ms;
                entry.last_pong_ms = now_ms;
            }
            entry.last_activity_ms = now_ms;
        }
        let result = match kind {
            "resume" => self.handle_resume(session.id, text, now_ms),
            "publish" => self.handle_publish(session.id, text, now_ms),
            "ack" => self.handle_ack(session.id, text),
            "clear_history" => self.handle_clear(session.id, text),
            _ => Err(EdgeFailure(EdgeError::ProtocolSchemaInvalid)),
        };
        result?;
        if kind == "resume" {
            self.sessions
                .lock()
                .expect("edge sessions lock poisoned")
                .entries
                .get_mut(&session.id)
                .ok_or(EdgeFailure(EdgeError::SessionContextStale))?
                .awaiting_resume = false;
        }
        self.close_slow_consumers()?;
        Ok(())
    }

    /// Holds the core lease through JSON serialization and the complete output
    /// operation. A clear cannot commit between its generation check and write.
    pub fn write_next_event(
        &self,
        session: SessionHandle,
        output: &mut WebSocketConnection,
    ) -> Result<bool, EdgeFailure> {
        let history_epoch = self.core.history_epoch();
        let Some(lease) = self
            .core
            .lease_next_session_event(session.id)
            .map_err(|error| EdgeFailure(core_error(error)))?
        else {
            return Ok(false);
        };
        let resume_complete = matches!(lease.event(), SessionEvent::ResumeComplete(_));
        let frame = event_frame(lease.event(), history_epoch);
        output
            .write_complete_text(&frame)
            .map_err(|_| EdgeFailure(EdgeError::OutputFailed))?;
        lease.complete();
        if resume_complete {
            self.core
                .complete_resume(session.id)
                .map_err(|error| EdgeFailure(core_error(error)))?;
        }
        Ok(true)
    }

    /// Reads one complete client WebSocket text frame through the concrete
    /// socket owner, then applies protocol admission before any mutation.
    pub fn read_websocket_text(
        &self,
        session: SessionHandle,
        websocket: &mut WebSocketConnection,
        now_ms: i64,
    ) -> Result<(), EdgeFailure> {
        let InboundFrame::Text(text) = websocket
            .read_complete_frame(self.config.maximum_message_bytes())
            .map_err(EdgeFailure)?
        else {
            return Err(EdgeFailure(EdgeError::ProtocolSchemaInvalid));
        };
        self.handle_text(session, &text, now_ms)
    }

    pub fn close_session(&self, session: SessionHandle) -> Result<(), EdgeFailure> {
        self.sessions
            .lock()
            .expect("edge sessions lock poisoned")
            .entries
            .remove(&session.id);
        self.core
            .close_session(session.id)
            .map_err(|error| EdgeFailure(core_error(error)))
    }

    /// Enforces the fixed resume and heartbeat deadlines. The owner must stop
    /// the TCP transport after a returned error; state is closed here first.
    pub fn tick(&self, session: SessionHandle, now_ms: i64) -> Result<(), EdgeFailure> {
        let failure = {
            let sessions = self.sessions.lock().expect("edge sessions lock poisoned");
            let entry = sessions
                .entries
                .get(&session.id)
                .ok_or(EdgeFailure(EdgeError::SessionContextStale))?;
            if entry.opened_at_ms != 0
                && entry.awaiting_resume
                && now_ms.saturating_sub(entry.opened_at_ms) > 5_000
            {
                Some(EdgeError::ResumeDeadlineExceeded)
            } else {
                None
            }
        };
        if let Some(failure) = failure {
            self.close_session(session)?;
            return Err(EdgeFailure(failure));
        }
        Ok(())
    }

    /// Records a received WebSocket pong; bytes themselves are not retained.
    pub fn note_pong(&self, session: SessionHandle, now_ms: i64) -> Result<(), EdgeFailure> {
        let mut sessions = self.sessions.lock().expect("edge sessions lock poisoned");
        let entry = sessions
            .entries
            .get_mut(&session.id)
            .ok_or(EdgeFailure(EdgeError::SessionContextStale))?;
        entry.last_pong_ms = now_ms;
        entry.last_activity_ms = now_ms;
        Ok(())
    }

    pub fn health(&self) -> HttpResponse {
        HttpResponse {
            status: 200,
            body: "{\"status\":\"ok\"}".to_owned(),
        }
    }
    pub fn poll_localapi(&self) {
        let loss = self.local_api.self_addresses().err();
        self.runtime
            .lock()
            .expect("edge runtime lock poisoned")
            .not_ready = loss.map(|_| EdgeError::TailscaleLocalapiUnavailable);
    }

    pub fn readiness(&self) -> HttpResponse {
        match self
            .runtime
            .lock()
            .expect("edge runtime lock poisoned")
            .not_ready
        {
            None => HttpResponse {
                status: 200,
                body: "{\"status\":\"ready\",\"protocol_version\":1}".to_owned(),
            },
            Some(reason) => HttpResponse {
                status: 503,
                body: format!(
                    "{{\"status\":\"not_ready\",\"reason_code\":\"{}\"}}",
                    reason.code()
                ),
            },
        }
    }

    fn session_peer(&self, session_id: Uuid) -> Result<StablePeerId, EdgeFailure> {
        self.sessions
            .lock()
            .expect("edge sessions lock poisoned")
            .entries
            .get(&session_id)
            .map(|session| session.peer_id.clone())
            .ok_or(EdgeFailure(EdgeError::SessionContextStale))
    }

    fn require_ready(&self) -> Result<(), EdgeFailure> {
        self.runtime
            .lock()
            .expect("edge runtime lock poisoned")
            .not_ready
            .map_or(Ok(()), |reason| Err(EdgeFailure(reason)))
    }

    fn consume_message_token(
        &self,
        session_id: Uuid,
        now_ms: i64,
        publish: bool,
    ) -> Result<(), EdgeFailure> {
        let mut sessions = self.sessions.lock().expect("edge sessions lock poisoned");
        let session = sessions
            .entries
            .get_mut(&session_id)
            .ok_or(EdgeFailure(EdgeError::SessionContextStale))?;
        let elapsed_ms = now_ms.saturating_sub(session.message_window_started_ms);
        let message_refill = (elapsed_ms.saturating_mul(120) / 60_000) as u32;
        if message_refill > 0 {
            session.message_window_started_ms = now_ms;
            session.message_tokens = session
                .message_tokens
                .saturating_add(message_refill)
                .min(20);
        }
        if session.message_tokens == 0 {
            return Err(EdgeFailure(EdgeError::MessageRateLimited));
        }
        session.message_tokens -= 1;
        if publish {
            let elapsed_ms = now_ms.saturating_sub(session.publish_window_started_ms);
            let refill = (elapsed_ms.saturating_mul(self.config.publish_tokens_per_minute as i64)
                / 60_000) as u32;
            if refill > 0 {
                session.publish_window_started_ms = now_ms;
                session.publish_tokens = session
                    .publish_tokens
                    .saturating_add(refill)
                    .min(self.config.publish_burst);
            }
            if session.publish_tokens == 0 {
                return Err(EdgeFailure(EdgeError::PublishRateLimited));
            }
            session.publish_tokens -= 1;
        }
        Ok(())
    }

    fn consume_connection_attempt(&self, peer_id: &StablePeerId, now_ms: i64) -> bool {
        let mut runtime = self.runtime.lock().expect("edge runtime lock poisoned");
        runtime
            .connection_attempts
            .entry(peer_id.clone())
            .or_insert(RateBucket {
                tokens: 10,
                updated_at_ms: now_ms,
            })
            .take(now_ms, 30, 10)
    }

    fn consume_http_request(&self, peer_id: &StablePeerId, now_ms: i64) -> bool {
        let mut runtime = self.runtime.lock().expect("edge runtime lock poisoned");
        runtime
            .http_requests
            .entry(peer_id.clone())
            .or_insert(RateBucket {
                tokens: 20,
                updated_at_ms: now_ms,
            })
            .take(now_ms, 120, 20)
    }

    fn close_slow_consumers(&self) -> Result<(), EdgeFailure> {
        let session_ids: Vec<_> = self
            .sessions
            .lock()
            .expect("edge sessions lock poisoned")
            .entries
            .keys()
            .copied()
            .collect();
        let over_limit: Vec<_> = session_ids
            .into_iter()
            .filter_map(|id| {
                let metrics = self.core.session_queue_metrics(id).ok()?;
                (metrics.events > self.config.outbound_queue_messages
                    || metrics.wire_upper_bound_bytes > self.config.outbound_queue_bytes)
                    .then_some(id)
            })
            .collect();
        for id in over_limit {
            self.close_session(SessionHandle { id })?;
        }
        // A recipient is closed after its queue crosses a bound. A completed
        // publisher never receives that recipient's failure: its transaction
        // and acknowledgement are already committed at the core seam.
        Ok(())
    }

    fn handle_resume(&self, session_id: Uuid, text: &str, now_ms: i64) -> Result<(), EdgeFailure> {
        let input: ResumeInput = serde_json::from_str(text)
            .map_err(|_| EdgeFailure(EdgeError::ProtocolSchemaInvalid))?;
        require_version(input.protocol_version)?;
        let epoch = input.known_history_epoch.map(parse_uuid).transpose()?;
        let generation = input.known_clear_generation.map(parse_u64).transpose()?;
        let cursor = input.after_cursor.map(parse_u64).transpose()?;
        self.core
            .begin_resume(session_id, epoch, generation, cursor, now_ms)
            .map(|_| ())
            .map_err(|error| EdgeFailure(core_error(error)))
    }

    fn handle_publish(&self, session_id: Uuid, text: &str, now_ms: i64) -> Result<(), EdgeFailure> {
        let input: PublishMessage = serde_json::from_str(text)
            .map_err(|_| EdgeFailure(EdgeError::ProtocolSchemaInvalid))?;
        require_version(input.protocol_version)?;
        let event = input.event;
        let clear_generation = parse_u64(event.clear_generation)?;
        self.core
            .validate_publish_context(session_id, clear_generation)
            .map_err(|error| EdgeFailure(core_error(error)))?;
        let content = ClipContentV1::from_wire(
            &event.content_type,
            &event.payload_b64,
            event.payload_bytes,
            &event.content_sha256,
            self.config.max_payload_bytes,
        )
        .map_err(|error| EdgeFailure(core_error(error)))?;
        self.core
            .publish(
                session_id,
                PublishInput {
                    message_id: parse_uuid(event.message_id)?,
                    clear_generation,
                    created_at_ms: event.created_at_ms,
                    content,
                },
                now_ms,
            )
            .map(|_| ())
            .map_err(|error| EdgeFailure(core_error(error)))
    }

    fn handle_ack(&self, session_id: Uuid, text: &str) -> Result<(), EdgeFailure> {
        let input: AckInput = serde_json::from_str(text)
            .map_err(|_| EdgeFailure(EdgeError::ProtocolSchemaInvalid))?;
        require_version(input.protocol_version)?;
        self.core
            .acknowledge(
                session_id,
                parse_uuid(input.history_epoch)?,
                parse_u64(input.clear_generation)?,
                parse_u64(input.cursor)?,
            )
            .map_err(|error| EdgeFailure(core_error(error)))
    }

    fn handle_clear(&self, session_id: Uuid, text: &str) -> Result<(), EdgeFailure> {
        let input: ClearInput = serde_json::from_str(text)
            .map_err(|_| EdgeFailure(EdgeError::ProtocolSchemaInvalid))?;
        require_version(input.protocol_version)?;
        self.core
            .clear_history(
                session_id,
                parse_uuid(input.request_id)?,
                parse_u64(input.expected_clear_generation)?,
            )
            .map(|_: ClearAccepted| ())
            .map_err(|error| EdgeFailure(core_error(error)))
    }
}

impl HubListener {
    /// Accepts one socket from the listener this value owns. Neither the local
    /// address nor the WhoIs input can be supplied by a caller.
    pub fn accept_once(&self, now_ms: i64) -> Result<ServedConnection, EdgeFailure> {
        let (stream, _) = self
            .listener
            .accept()
            .map_err(|_| EdgeFailure(EdgeError::BindFailed))?;
        self.edge.serve_accepted(stream, now_ms)
    }

    /// Owns the post-upgrade session lifecycle: output drain, ping interval,
    /// LocalAPI probe, text admission, failure frame, and close frame.
    pub fn accept_and_serve(&self) -> Result<(), EdgeFailure> {
        let started = unix_ms()?;
        let ServedConnection::Upgraded {
            session,
            mut websocket,
        } = self.accept_once(started)?
        else {
            return Ok(());
        };
        websocket
            .stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .map_err(|_| EdgeFailure(EdgeError::OutputFailed))?;
        let mut last_outbound = Instant::now();
        let mut last_probe = Instant::now();
        let mut ping_sent = None;
        loop {
            let now_ms = unix_ms()?;
            if let Err(EdgeFailure(error)) = self.edge.tick(session, now_ms) {
                return self.close_with(session, &mut websocket, error);
            }
            if last_probe.elapsed() >= Duration::from_secs(60) {
                self.edge.poll_localapi();
                last_probe = Instant::now();
                if let Err(EdgeFailure(error)) = self.edge.require_ready() {
                    return self.close_with(session, &mut websocket, error);
                }
            }
            loop {
                match self.edge.write_next_event(session, &mut websocket) {
                    Ok(true) => {
                        last_outbound = Instant::now();
                        ping_sent = None;
                    }
                    Ok(false) => break,
                    Err(EdgeFailure(error)) => {
                        return self.close_with(session, &mut websocket, error)
                    }
                }
            }
            if last_outbound.elapsed() >= Duration::from_secs(30) && ping_sent.is_none() {
                websocket
                    .write_ping()
                    .map_err(|_| EdgeFailure(EdgeError::OutputFailed))?;
                ping_sent = Some(Instant::now());
            }
            if ping_sent.is_some_and(|at| at.elapsed() >= Duration::from_secs(10)) {
                return self.close_with(session, &mut websocket, EdgeError::HeartbeatTimedOut);
            }
            match websocket.read_complete_frame(self.edge.config.maximum_message_bytes()) {
                Ok(InboundFrame::Text(text)) => {
                    if let Err(EdgeFailure(error)) = self.edge.handle_text(session, &text, now_ms) {
                        return self.close_with(session, &mut websocket, error);
                    }
                }
                Ok(InboundFrame::Pong) => {
                    self.edge.note_pong(session, now_ms)?;
                    ping_sent = None;
                }
                Ok(InboundFrame::Close) => {
                    self.edge.close_session(session)?;
                    return Ok(());
                }
                Err(EdgeError::OutputFailed) => continue,
                Err(error) => return self.close_with(session, &mut websocket, error),
            }
        }
    }

    fn close_with(
        &self,
        session: SessionHandle,
        websocket: &mut WebSocketConnection,
        error: EdgeError,
    ) -> Result<(), EdgeFailure> {
        let _ = websocket.write_complete_text(&HttpResponse::error(400, error).body);
        let _ = websocket.write_close(error);
        let _ = self.edge.close_session(session);
        Err(EdgeFailure(error))
    }
}

fn unix_ms() -> Result<i64, EdgeFailure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| EdgeFailure(EdgeError::StorageUnavailable))
        .and_then(|duration| {
            i64::try_from(duration.as_millis())
                .map_err(|_| EdgeFailure(EdgeError::StorageUnavailable))
        })
}

fn disallowed_bind_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(address) => {
            let octets = address.octets();
            !(octets[0] == 100 && (64..=127).contains(&octets[1]))
        }
        IpAddr::V6(address) => {
            let segments = address.segments();
            segments[..3] != [0xfd7a, 0x115c, 0xa1e0]
        }
    }
}

fn validate_http(request: &HttpRequest) -> Option<HttpResponse> {
    if request.header_bytes > MAX_HEADERS {
        return Some(
            HttpResponse::error(431, EdgeError::ConfigValueInvalid)
                .with_code("request_headers_too_large", false),
        );
    }
    let path = request
        .target
        .split_once('?')
        .map_or(request.target.as_str(), |(path, _)| path);
    if path != "/v1/stream" {
        return Some(
            HttpResponse::error(404, EdgeError::ConfigValueInvalid)
                .with_code("http_path_not_found", false),
        );
    }
    if request.method != "GET" {
        return Some(
            HttpResponse::error(405, EdgeError::ConfigValueInvalid)
                .with_code("http_method_not_allowed", false),
        );
    }
    if request.target.contains('?') || request.target.contains('@') {
        return Some(
            HttpResponse::error(403, EdgeError::ConfigValueInvalid)
                .with_code("client_identity_claim_forbidden", false),
        );
    }
    if request.headers.iter().any(|(name, _)| {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "authorization" | "x-forwarded-for" | "forwarded"
        ) || name.to_ascii_lowercase().starts_with("x-clipmesh-")
    }) {
        return Some(
            HttpResponse::error(403, EdgeError::ConfigValueInvalid)
                .with_code("client_identity_claim_forbidden", false),
        );
    }
    let protocols: Vec<_> = request
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("sec-websocket-protocol"))
        .collect();
    if protocols.len() != 1 || protocols[0].1 != PROTOCOL {
        return Some(HttpResponse::error(
            400,
            EdgeError::ProtocolVersionUnsupported,
        ));
    }
    None
}

fn read_http_request(stream: &mut TcpStream) -> std::io::Result<HttpRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut raw = Vec::new();
    let mut byte = [0_u8; 1];
    while raw.len() <= MAX_HEADERS {
        stream.read_exact(&mut byte)?;
        raw.push(byte[0]);
        if raw.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let header_bytes = raw.len();
    let text = std::str::from_utf8(&raw)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
    let mut lines = text.trim_end_matches("\r\n").split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
    let mut parts = request_line.split_ascii_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
    let target = parts
        .next()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
    if parts.next().is_none() {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
    }
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
        headers.push((name.trim().to_owned(), value.trim().to_owned()));
    }
    Ok(HttpRequest {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
        header_bytes,
    })
}

fn write_http_response(stream: &mut TcpStream, response: &HttpResponse) -> std::io::Result<()> {
    let status = match response.status {
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status,
        status,
        response.body.len(),
        response.body
    )?;
    stream.flush()
}

fn write_upgrade_response(stream: &mut TcpStream, key: &str) -> std::io::Result<()> {
    let mut digest = Sha1::new();
    digest.update(key.as_bytes());
    digest.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let accept = STANDARD.encode(digest.finalize());
    write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Protocol: {PROTOCOL}\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    )?;
    stream.flush()
}

trait HttpCode {
    fn with_code(self, code: &'static str, retryable: bool) -> Self;
}
impl HttpCode for HttpResponse {
    fn with_code(mut self, code: &'static str, retryable: bool) -> Self {
        self.body = format!("{{\"protocol_version\":1,\"error\":{{\"code\":\"{code}\",\"retryable\":{retryable}}}}}");
        self
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeInput {
    protocol_version: u64,
    #[serde(rename = "type")]
    _kind: String,
    known_history_epoch: Option<String>,
    known_clear_generation: Option<String>,
    after_cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishMessage {
    protocol_version: u64,
    #[serde(rename = "type")]
    _kind: String,
    event: PublishEvent,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishEvent {
    message_id: String,
    clear_generation: String,
    created_at_ms: i64,
    content_type: String,
    payload_bytes: usize,
    content_sha256: String,
    payload_b64: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AckInput {
    protocol_version: u64,
    #[serde(rename = "type")]
    _kind: String,
    history_epoch: String,
    clear_generation: String,
    cursor: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClearInput {
    protocol_version: u64,
    #[serde(rename = "type")]
    _kind: String,
    request_id: String,
    expected_clear_generation: String,
}

fn require_version(version: u64) -> Result<(), EdgeFailure> {
    if version == 1 {
        Ok(())
    } else {
        Err(EdgeFailure(EdgeError::ProtocolVersionUnsupported))
    }
}
fn parse_uuid(value: String) -> Result<Uuid, EdgeFailure> {
    let parsed =
        Uuid::parse_str(&value).map_err(|_| EdgeFailure(EdgeError::ProtocolSchemaInvalid))?;
    if parsed.to_string() != value
        || parsed.get_version() != Some(Version::Random)
        || parsed.get_variant() != Variant::RFC4122
    {
        Err(EdgeFailure(EdgeError::ProtocolSchemaInvalid))
    } else {
        Ok(parsed)
    }
}
fn parse_u64(value: String) -> Result<u64, EdgeFailure> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(EdgeFailure(EdgeError::ProtocolSchemaInvalid));
    }
    value
        .parse()
        .ok()
        .filter(|value: &u64| *value > 0)
        .ok_or(EdgeFailure(EdgeError::ProtocolSchemaInvalid))
}

fn core_error(error: CoreError) -> EdgeError {
    match error {
        CoreError::Failure(code) => match code {
            CoreFailureCode::ConfigValueInvalid => EdgeError::ConfigValueInvalid,
            CoreFailureCode::TailnetPeerUnverified => EdgeError::TailnetPeerUnverified,
            CoreFailureCode::DatabaseSchemaUnsupported => EdgeError::DatabaseSchemaUnsupported,
            CoreFailureCode::DatabaseIntegrityFailed => EdgeError::DatabaseIntegrityFailed,
            CoreFailureCode::StorageUnavailable => EdgeError::StorageUnavailable,
            CoreFailureCode::SessionContextStale => EdgeError::SessionContextStale,
            CoreFailureCode::ResumeContextIncomplete => EdgeError::ResumeContextIncomplete,
            CoreFailureCode::ResumeCursorWithoutContext => EdgeError::ResumeCursorWithoutContext,
            CoreFailureCode::CursorAhead => EdgeError::CursorAhead,
            CoreFailureCode::ClearGenerationStale => EdgeError::ClearGenerationStale,
            CoreFailureCode::ClearGenerationAhead => EdgeError::ClearGenerationAhead,
            CoreFailureCode::ClearGenerationExhausted => EdgeError::ClearGenerationExhausted,
            CoreFailureCode::RequestIdConflict => EdgeError::RequestIdConflict,
            CoreFailureCode::MessageIdConflict => EdgeError::MessageIdConflict,
            CoreFailureCode::MessageIdReplay => EdgeError::MessageIdReplay,
            CoreFailureCode::CreatedAtInFuture => EdgeError::CreatedAtInFuture,
            CoreFailureCode::EventTooOld => EdgeError::EventTooOld,
            CoreFailureCode::ContentTypeUnsupported => EdgeError::ContentTypeUnsupported,
            CoreFailureCode::PayloadEmpty => EdgeError::PayloadEmpty,
            CoreFailureCode::PayloadTooLarge => EdgeError::PayloadTooLarge,
            CoreFailureCode::PayloadEncodingInvalid => EdgeError::PayloadEncodingInvalid,
            CoreFailureCode::PayloadLengthMismatch => EdgeError::PayloadLengthMismatch,
            CoreFailureCode::PayloadHashMismatch => EdgeError::PayloadHashMismatch,
            CoreFailureCode::HubCursorExhausted => EdgeError::HubCursorExhausted,
            CoreFailureCode::AckInvalid => EdgeError::AckInvalid,
        },
    }
}

fn event_frame(event: &SessionEvent, history_epoch: Uuid) -> String {
    let content = |clip: &RetainedClip| {
        let wire = clip.content.to_wire();
        json!({"message_id":clip.message_id.to_string(),"clear_generation":clip.clear_generation.to_string(),"created_at_ms":clip.created_at_ms,"content_type":wire.content_type,"payload_bytes":wire.payload_bytes,"content_sha256":wire.content_sha256,"payload_b64":wire.payload_b64})
    };
    match event {
        SessionEvent::ResumeStarted(value) => json!({"protocol_version":1,"type":"resume_started","history_epoch":value.history_epoch.to_string(),"clear_generation":value.clear_generation.to_string(),"status":resume_status(value.status),"requested_after_cursor":value.requested_after_cursor.map(|v|v.to_string()),"boundary_cursor":value.boundary_cursor.map(|v|v.to_string()),"lost_through_cursor":value.lost_through_cursor.map(|v|v.to_string())}).to_string(),
        SessionEvent::ResumeClip(clip) | SessionEvent::Live(clip) => json!({"protocol_version":1,"type":"event","history_epoch":history_epoch.to_string(),"clear_generation":clip.clear_generation.to_string(),"cursor":clip.cursor.to_string(),"delivery":if matches!(event, SessionEvent::ResumeClip(_)) {"resume"} else {"live"},"accepted_at_ms":clip.accepted_at_ms,"expires_at_ms":clip.expires_at_ms,"source_peer_id":clip.source_peer_id.as_boundary_value(),"event":content(clip)}).to_string(),
        SessionEvent::ResumeComplete(value) => json!({"protocol_version":1,"type":"resume_complete","history_epoch":value.history_epoch.to_string(),"clear_generation":value.clear_generation.to_string(),"boundary_cursor":value.boundary_cursor.map(|v|v.to_string())}).to_string(),
        SessionEvent::PublishAccepted(value) => json!({"protocol_version":1,"type":"publish_accepted","message_id":value.message_id.to_string(),"cursor":value.cursor.to_string(),"expires_at_ms":value.expires_at_ms,"duplicate":value.duplicate}).to_string(),
        SessionEvent::ClearAccepted(value) => json!({"protocol_version":1,"type":"clear_accepted","request_id":value.request_id.to_string(),"clear_generation":value.clear_generation.to_string(),"cleared_through_cursor":value.cleared_through_cursor.map(|v|v.to_string()),"duplicate":value.duplicate}).to_string(),
        SessionEvent::ClearNotice(value) => json!({"protocol_version":1,"type":"clear_notice","request_id":value.request_id.to_string(),"clear_generation":value.clear_generation.to_string(),"cleared_through_cursor":value.cleared_through_cursor.map(|v|v.to_string())}).to_string(),
    }
}

fn resume_status(status: clipmesh_hub_core::ResumeStatus) -> &'static str {
    match status {
        clipmesh_hub_core::ResumeStatus::Fresh => "fresh",
        clipmesh_hub_core::ResumeStatus::Complete => "complete",
        clipmesh_hub_core::ResumeStatus::Gap => "gap",
        clipmesh_hub_core::ResumeStatus::EpochChanged => "epoch_changed",
        clipmesh_hub_core::ResumeStatus::GenerationChanged => "generation_changed",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        os::unix::net::UnixListener,
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    use super::*;
    use tempfile::tempdir;

    const NOW: i64 = 1_700_000_000_000;

    #[derive(Clone)]
    struct LocalApiSimulator {
        state: Arc<Mutex<SimState>>,
        _directory: Arc<tempfile::TempDir>,
    }

    struct SimState {
        available: bool,
        peer: Option<&'static str>,
        requests: Vec<String>,
    }

    impl LocalApiSimulator {
        fn admitted() -> Self {
            let directory = Arc::new(tempdir().unwrap());
            let path = directory.path().join("tailscaled.sock");
            let listener = UnixListener::bind(&path).unwrap();
            listener.set_nonblocking(true).unwrap();
            let state = Arc::new(Mutex::new(SimState {
                available: true,
                peer: Some("peer-reserved-example"),
                requests: Vec::new(),
            }));
            let server_state = Arc::clone(&state);
            let server_directory = Arc::clone(&directory);
            thread::spawn(move || loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0_u8; 1024];
                        let size = stream.read(&mut request).unwrap();
                        let request = String::from_utf8_lossy(&request[..size]);
                        let target = request.split_whitespace().nth(1).unwrap_or("").to_owned();
                        let (status, body) = {
                            let mut state = server_state.lock().unwrap();
                            state.requests.push(target.clone());
                            if !state.available {
                                (503, "{}".to_owned())
                            } else if target.starts_with("/localapi/v0/status") {
                                (200, r#"{"TailscaleIPs":["100.64.0.7"]}"#.to_owned())
                            } else if let Some(peer) = state.peer {
                                (200, format!(r#"{{"Node":{{"StableID":"{peer}"}}}}"#))
                            } else {
                                (404, "{}".to_owned())
                            }
                        };
                        let response = format!(
                            "HTTP/1.1 {status} ok\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        if stream.flush().is_ok() {
                            continue;
                        }
                        let _ = write!(stream, "HTTP/1.1 {status} ok\\r\\nContent-Length: {}\\r\\nConnection: close\\r\\n\\r\\n{body}", body.len());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Arc::strong_count(&server_directory) == 1 {
                            break;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => break,
                }
            });
            Self {
                state,
                _directory: directory,
            }
        }

        fn client(&self) -> SystemLocalApi {
            SystemLocalApi::from_socket_path(self._directory.path().join("tailscaled.sock"))
        }
    }

    fn config() -> EdgeConfig {
        EdgeConfig {
            listen_address: "100.64.0.7:8123".parse().unwrap(),
            state_directory: PathBuf::from("/tmp/clipmesh-r3-test-state"),
            retention_seconds: 60,
            history_max_entries: 500,
            max_payload_bytes: 262_144,
            max_connections: 64,
            max_connections_per_peer: 2,
            publish_tokens_per_minute: 60,
            publish_burst: 10,
            outbound_queue_messages: 64,
            outbound_queue_bytes: 2_097_152,
        }
    }

    fn edge() -> (tempfile::TempDir, LocalApiSimulator, HubEdge) {
        let directory = tempdir().unwrap();
        let database = directory.path().join("hub.sqlite");
        let daemon = LocalApiSimulator::admitted();
        let edge = HubEdge::prepare(config(), daemon.client(), database).unwrap();
        (directory, daemon, edge)
    }

    fn request() -> HttpRequest {
        HttpRequest {
            method: "GET".to_owned(),
            target: "/v1/stream".to_owned(),
            headers: vec![("Sec-WebSocket-Protocol".to_owned(), PROTOCOL.to_owned())],
            header_bytes: 64,
        }
    }

    fn session(edge: &HubEdge) -> SessionHandle {
        let admitted = edge
            .admit_socket("100.64.0.9:12345".parse().unwrap())
            .unwrap();
        match edge.upgrade(admitted, &request()) {
            UpgradeResult::Upgraded(session) => session,
            UpgradeResult::Response(response) => panic!("unexpected response: {response:?}"),
        }
    }

    fn drain(edge: &HubEdge, session: SessionHandle) -> Vec<String> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut reader = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        let mut writer = WebSocketConnection::from_upgraded(server);
        let mut count = 0;
        while edge.write_next_event(session, &mut writer).unwrap() {
            count += 1;
        }
        let mut frames = Vec::new();
        for _ in 0..count {
            let mut header = [0_u8; 2];
            reader.read_exact(&mut header).unwrap();
            assert_eq!(header[0], 0x81);
            let length = match header[1] & 0x7f {
                size @ 0..=125 => size as usize,
                126 => {
                    let mut value = [0_u8; 2];
                    reader.read_exact(&mut value).unwrap();
                    u16::from_be_bytes(value) as usize
                }
                _ => panic!("fixture does not emit enormous frames"),
            };
            let mut frame = vec![0_u8; length];
            reader.read_exact(&mut frame).unwrap();
            frames.push(String::from_utf8(frame).unwrap());
        }
        frames
    }

    fn masked_text_frame(text: &str) -> Vec<u8> {
        assert!(text.len() <= 125, "fixture uses a compact WebSocket frame");
        let mask = [0x13, 0x37, 0xca, 0xfe];
        let mut frame = vec![0x81, 0x80 | text.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(
            text.bytes()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        frame
    }

    fn live(edge: &HubEdge) -> SessionHandle {
        let session = session(edge);
        edge.handle_text(session, r#"{"protocol_version":1,"type":"resume","known_history_epoch":null,"known_clear_generation":null,"after_cursor":null}"#, NOW).unwrap();
        let frames = drain(edge, session);
        assert_eq!(frames.len(), 2);
        session
    }

    #[test]
    fn rejects_unverified_peer_before_http_and_tailnet_bind_is_exact() {
        let directory = tempdir().unwrap();
        let failing = LocalApiSimulator::admitted();
        failing.state.lock().unwrap().peer = None;
        let edge = HubEdge::prepare(
            config(),
            failing.client(),
            directory.path().join("hub.sqlite"),
        )
        .unwrap();
        assert!(matches!(
            edge.admit_socket("100.64.0.8:1".parse().unwrap()),
            Err(EdgeFailure(EdgeError::TailnetPeerUnverified))
        ));

        let mut invalid = config();
        invalid.listen_address = "192.0.2.1:8123".parse().unwrap();
        assert!(matches!(
            HubEdge::prepare(
                invalid,
                LocalApiSimulator::admitted().client(),
                directory.path().join("other.sqlite")
            ),
            Err(EdgeFailure(EdgeError::TailnetBindUnverified))
        ));
    }

    #[test]
    fn http_precedence_is_content_free_and_exact() {
        let (_directory, _daemon, edge) = edge();
        let admitted = edge.admit_socket("100.64.0.9:1".parse().unwrap()).unwrap();
        let oversized = HttpRequest {
            header_bytes: MAX_HEADERS + 1,
            ..request()
        };
        let UpgradeResult::Response(response) = edge.upgrade(admitted, &oversized) else {
            panic!("must reject")
        };
        assert_eq!(response.status, 431);
        assert_eq!(response.body, "{\"protocol_version\":1,\"error\":{\"code\":\"request_headers_too_large\",\"retryable\":false}}");

        let admitted = edge.admit_socket("100.64.0.9:2".parse().unwrap()).unwrap();
        let forbidden = HttpRequest {
            headers: vec![("X-ClipMesh-Peer".to_owned(), "ignored".to_owned())],
            ..request()
        };
        let UpgradeResult::Response(response) = edge.upgrade(admitted, &forbidden) else {
            panic!("must reject")
        };
        assert_eq!(response.status, 403);
        assert!(!response.body.contains("ignored"));

        let admitted = edge.admit_socket("100.64.0.9:3".parse().unwrap()).unwrap();
        let query = HttpRequest {
            target: "/v1/stream?peer=ignored".to_owned(),
            ..request()
        };
        let UpgradeResult::Response(response) = edge.upgrade(admitted, &query) else {
            panic!("must reject")
        };
        assert_eq!(response.status, 403);
        assert_eq!(response.body, "{\"protocol_version\":1,\"error\":{\"code\":\"client_identity_claim_forbidden\",\"retryable\":false}}");
    }

    #[test]
    fn clear_retracts_unfinished_old_generation_output_before_a_later_write() {
        let (_directory, _daemon, edge) = edge();
        let source = live(&edge);
        let target = live(&edge);
        edge.handle_text(source, r#"{"protocol_version":1,"type":"publish","event":{"message_id":"00000000-0000-4000-8000-000000000001","clear_generation":"1","created_at_ms":1700000000000,"content_type":"text/plain","payload_bytes":12,"content_sha256":"5cb72f90e968922d30557d0af8f719d21f61792becaa87eb32477767d739dc0b","payload_b64":"Zml4dHVyZSB0ZXh0"}}"#, NOW).unwrap();
        edge.handle_text(source, r#"{"protocol_version":1,"type":"clear_history","request_id":"00000000-0000-4000-8000-000000000002","expected_clear_generation":"1"}"#, NOW).unwrap();
        let target_frames = drain(&edge, target);
        assert!(target_frames
            .iter()
            .any(|frame| frame.contains("clear_notice")));
        assert!(target_frames
            .iter()
            .all(|frame| !frame.contains("Zml4dHVyZSB0ZXh0")));
    }

    #[test]
    fn duplicate_json_field_and_oversize_text_close_before_state_mutation() {
        let (_directory, _daemon, edge) = edge();
        let session = session(&edge);
        let duplicate = r#"{"protocol_version":1,"type":"resume","type":"resume","known_history_epoch":null,"known_clear_generation":null,"after_cursor":null}"#;
        assert_eq!(
            edge.handle_text(session, duplicate, NOW).unwrap_err().0,
            EdgeError::ProtocolSchemaInvalid
        );
        assert_eq!(
            edge.handle_text(
                session,
                &"x".repeat(edge.config().maximum_message_bytes() + 1),
                NOW
            )
            .unwrap_err()
            .0,
            EdgeError::MessageTooLarge
        );
    }

    #[test]
    fn stale_generation_precedes_payload_decoding() {
        let (_directory, _daemon, edge) = edge();
        let session = live(&edge);
        edge.handle_text(session, r#"{"protocol_version":1,"type":"clear_history","request_id":"00000000-0000-4000-8000-000000000003","expected_clear_generation":"1"}"#, NOW).unwrap();
        let stale_bad_payload = r#"{"protocol_version":1,"type":"publish","event":{"message_id":"00000000-0000-4000-8000-000000000004","clear_generation":"1","created_at_ms":1700000000000,"content_type":"text/plain","payload_bytes":1,"content_sha256":"0000000000000000000000000000000000000000000000000000000000000000","payload_b64":"!"}}"#;
        assert_eq!(
            edge.handle_text(session, stale_bad_payload, NOW)
                .unwrap_err()
                .0,
            EdgeError::ClearGenerationStale
        );
    }

    #[test]
    fn rate_bucket_and_localapi_loss_stop_mutation_and_readiness() {
        let directory = tempdir().unwrap();
        let daemon = LocalApiSimulator::admitted();
        let mut limits = config();
        limits.publish_burst = 1;
        let edge =
            HubEdge::prepare(limits, daemon.client(), directory.path().join("hub.sqlite")).unwrap();
        let session = live(&edge);
        let message = r#"{"protocol_version":1,"type":"publish","event":{"message_id":"00000000-0000-4000-8000-000000000005","clear_generation":"1","created_at_ms":1700000000000,"content_type":"text/plain","payload_bytes":12,"content_sha256":"5cb72f90e968922d30557d0af8f719d21f61792becaa87eb32477767d739dc0b","payload_b64":"Zml4dHVyZSB0ZXh0"}}"#;
        edge.handle_text(session, message, NOW).unwrap();
        assert_eq!(
            edge.handle_text(session, message, NOW).unwrap_err().0,
            EdgeError::PublishRateLimited
        );

        daemon.state.lock().unwrap().available = false;
        edge.poll_localapi();
        assert_eq!(edge.readiness().status, 503);
        assert_eq!(
            edge.handle_text(session, message, NOW).unwrap_err().0,
            EdgeError::TailscaleLocalapiUnavailable
        );
    }

    #[test]
    fn peer_connection_and_http_buckets_refill_at_the_specified_rates() {
        let (_directory, _daemon, edge) = edge();
        let peer = StablePeerId::from_boundary("peer-reserved-example".to_owned()).unwrap();
        for _ in 0..10 {
            assert!(edge.consume_connection_attempt(&peer, NOW));
        }
        assert!(!edge.consume_connection_attempt(&peer, NOW));
        assert!(edge.consume_connection_attempt(&peer, NOW + 2_000));

        for _ in 0..20 {
            assert!(edge.consume_http_request(&peer, NOW));
        }
        assert!(!edge.consume_http_request(&peer, NOW));
        assert!(edge.consume_http_request(&peer, NOW + 500));
    }

    #[test]
    fn mismatched_accepted_socket_is_rejected_before_http_or_whois() {
        let directory = tempdir().unwrap();
        let daemon = LocalApiSimulator::admitted();
        let edge = Arc::new(
            HubEdge::prepare(
                config(),
                daemon.client(),
                directory.path().join("hub.sqlite"),
            )
            .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_edge = Arc::clone(&edge);
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            server_edge.serve_accepted(stream, NOW)
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .write_all(b"GET /v1/stream HTTP/1.1\r\nHost: simulator\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: clipmesh.v1\r\n\r\n")
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        let _ = client.read_to_end(&mut response);
        assert!(matches!(
            server.join().unwrap(),
            Err(EdgeFailure(EdgeError::TailnetBindUnverified))
        ));
        let requests = &daemon.state.lock().unwrap().requests;
        assert!(requests
            .iter()
            .all(|target| target == "/localapi/v0/status"));
    }

    #[test]
    fn concrete_websocket_reader_accepts_only_masked_complete_text() {
        let (_directory, _daemon, edge) = edge();
        let session = session(&edge);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        let mut websocket = WebSocketConnection::from_upgraded(server);
        let resume = r#"{"protocol_version":1,"type":"resume","known_history_epoch":null,"known_clear_generation":null,"after_cursor":null}"#;
        client.write_all(&masked_text_frame(resume)).unwrap();
        edge.read_websocket_text(session, &mut websocket, NOW)
            .unwrap();
        assert_eq!(drain(&edge, session).len(), 2);
        client.write_all(&[0x8a, 0x80, 0, 0, 0, 0]).unwrap();
        assert!(matches!(
            websocket
                .read_complete_frame(edge.config().maximum_message_bytes())
                .unwrap(),
            InboundFrame::Pong
        ));
        client.write_all(&[0x82, 0x80, 0, 0, 0, 0]).unwrap();
        assert_eq!(
            edge.read_websocket_text(session, &mut websocket, NOW)
                .unwrap_err()
                .0,
            EdgeError::ProtocolSchemaInvalid
        );
    }

    #[test]
    fn negative_schema_and_lifecycle_boundaries_close_without_payload_capture() {
        let (_directory, _daemon, edge) = edge();
        let session = session(&edge);
        assert_eq!(
            edge.handle_text(
                session,
                r#"{"protocol_version":1,"type":"resume","unknown":true}"#,
                NOW
            )
            .unwrap_err()
            .0,
            EdgeError::ProtocolSchemaInvalid
        );

        {
            let mut sessions = edge.sessions.lock().unwrap();
            let entry = sessions.entries.get_mut(&session.id).unwrap();
            entry.opened_at_ms = NOW;
            entry.last_pong_ms = NOW;
        }
        assert_eq!(
            edge.tick(session, NOW + 5_001).unwrap_err().0,
            EdgeError::ResumeDeadlineExceeded
        );
        assert_eq!(
            edge.session_peer(session.id).unwrap_err().0,
            EdgeError::SessionContextStale
        );

        let heartbeat = super::tests::session(&edge);
        {
            let mut sessions = edge.sessions.lock().unwrap();
            let entry = sessions.entries.get_mut(&heartbeat.id).unwrap();
            entry.opened_at_ms = NOW;
            entry.awaiting_resume = false;
            entry.last_pong_ms = NOW;
        }
        edge.note_pong(heartbeat, NOW + 44_000).unwrap();
        edge.tick(heartbeat, NOW + 45_000).unwrap();
    }

    #[test]
    fn outbound_queue_limit_closes_the_slow_consumer() {
        let directory = tempdir().unwrap();
        let daemon = LocalApiSimulator::admitted();
        let mut limits = config();
        limits.outbound_queue_messages = 2;
        let edge =
            HubEdge::prepare(limits, daemon.client(), directory.path().join("hub.sqlite")).unwrap();
        let source = live(&edge);
        let target = live(&edge);
        let first = r#"{"protocol_version":1,"type":"publish","event":{"message_id":"00000000-0000-4000-8000-000000000011","clear_generation":"1","created_at_ms":1700000000000,"content_type":"text/plain","payload_bytes":12,"content_sha256":"5cb72f90e968922d30557d0af8f719d21f61792becaa87eb32477767d739dc0b","payload_b64":"Zml4dHVyZSB0ZXh0"}}"#;
        edge.handle_text(source, first, NOW).unwrap();
        drain(&edge, source);
        let second = first.replace("000000000011", "000000000012");
        edge.handle_text(source, &second, NOW).unwrap();
        drain(&edge, source);
        let third = first.replace("000000000011", "000000000013");
        edge.handle_text(source, &third, NOW).unwrap();
        assert_eq!(
            edge.session_peer(target.id).unwrap_err().0,
            EdgeError::SessionContextStale
        );
    }
}
