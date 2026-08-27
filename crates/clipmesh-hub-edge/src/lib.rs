//! Tailnet-only HTTP and WebSocket edge for the transport-neutral hub core.
//!
//! This crate deliberately has no executable and never starts a listener by
//! itself. A deployment-owned process may bind the validated address and pass
//! each accepted socket's observed remote address to [`HubEdge::admit_socket`]
//! before it reads one HTTP byte.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Mutex,
};

use clipmesh_hub_core::{
    ClearAccepted, ClipContentV1, CoreError, FailureCode as CoreFailureCode, HubCore, PublishInput,
    RetainedClip, SessionEvent, StablePeerId,
};
use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;
use uuid::{Uuid, Variant, Version};

const PROTOCOL: &str = "clipmesh.v1";
const MAX_HEADERS: usize = 16_384;

/// The only LocalAPI operations the edge can use.
///
/// The production adapter owns the operating-system daemon connection. It
/// must not implement this trait with a shell command, proxy header, or
/// network-reachable identity service.
pub trait LocalApi {
    fn self_addresses(&self) -> Result<Vec<IpAddr>, LocalApiError>;
    fn who_is(&self, remote: SocketAddr) -> Result<String, LocalApiError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalApiError {
    Unavailable,
    PermissionDenied,
    PeerNotFound,
    MalformedResponse,
    TimedOut,
}

/// Closed, generic version-1 hub configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EdgeConfig {
    pub listen_address: SocketAddr,
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
    MessageTooLarge,
    MessageRateLimited,
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
            Self::MessageTooLarge => "message_too_large",
            Self::MessageRateLimited => "message_rate_limited",
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
                | Self::MessageRateLimited
                | Self::SessionContextStale
                | Self::ClearGenerationAhead
                | Self::CreatedAtInFuture
                | Self::StorageUnavailable
                | Self::DatabaseIntegrityFailed
        )
    }

    pub fn websocket_close_code(self) -> u16 {
        match self {
            Self::MessageRateLimited => 4429,
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

#[derive(Default)]
struct Sessions {
    peers: HashMap<Uuid, StablePeerId>,
}

/// Output is synchronous by contract: the complete WebSocket text frame is
/// handed off while the core's event lease holds the mutation seam.
pub trait WebSocketOutput {
    type Error;

    fn write_complete_text(&mut self, frame: &str) -> Result<(), Self::Error>;
}

pub struct HubEdge<A> {
    config: EdgeConfig,
    local_api: A,
    core: HubCore,
    sessions: Mutex<Sessions>,
}

impl<A: LocalApi> HubEdge<A> {
    /// Validates config and current LocalAPI status before opening hub state.
    /// It does not bind or activate a listener.
    pub fn prepare(
        config: EdgeConfig,
        local_api: A,
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
        })
    }

    pub fn config(&self) -> &EdgeConfig {
        &self.config
    }

    /// Calls WhoIs before any HTTP parser is invoked by the caller.
    pub fn admit_socket(&self, observed_remote: SocketAddr) -> Result<AdmittedSocket, EdgeFailure> {
        let stable_id = self
            .local_api
            .who_is(observed_remote)
            .map_err(|_| EdgeFailure(EdgeError::TailnetPeerUnverified))?;
        let peer_id = StablePeerId::from_boundary(stable_id)
            .map_err(|_| EdgeFailure(EdgeError::TailnetPeerUnverified))?;
        Ok(AdmittedSocket { peer_id })
    }

    /// Validates an admitted HTTP request in the specified precedence order.
    pub fn upgrade(&self, admitted: AdmittedSocket, request: &HttpRequest) -> UpgradeResult {
        if let Some(response) = validate_http(request) {
            return UpgradeResult::Response(response);
        }
        let mut sessions = self.sessions.lock().expect("edge sessions lock poisoned");
        let peer_count = sessions
            .peers
            .values()
            .filter(|peer| *peer == &admitted.peer_id)
            .count();
        if sessions.peers.len() >= self.config.max_connections
            || peer_count >= self.config.max_connections_per_peer
        {
            return UpgradeResult::Response(HttpResponse::error(
                429,
                EdgeError::ConnectionLimitReached,
            ));
        }
        let hello = self.core.open_session(admitted.peer_id.clone());
        sessions.peers.insert(hello.session_id, admitted.peer_id);
        UpgradeResult::Upgraded(SessionHandle {
            id: hello.session_id,
        })
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
        if text.len() > self.config.maximum_message_bytes() {
            return Err(EdgeFailure(EdgeError::MessageTooLarge));
        }
        let value: Value = serde_json::from_str(text)
            .map_err(|_| EdgeFailure(EdgeError::ProtocolSchemaInvalid))?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(EdgeFailure(EdgeError::ProtocolSchemaInvalid))?;
        match kind {
            "resume" => self.handle_resume(session.id, text, now_ms),
            "publish" => self.handle_publish(session.id, text, now_ms),
            "ack" => self.handle_ack(session.id, text),
            "clear_history" => self.handle_clear(session.id, text),
            _ => Err(EdgeFailure(EdgeError::ProtocolSchemaInvalid)),
        }
    }

    /// Holds the core lease through JSON serialization and the complete output
    /// operation. A clear cannot commit between its generation check and write.
    pub fn write_next_event(
        &self,
        session: SessionHandle,
        output: &mut impl WebSocketOutput,
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

    pub fn close_session(&self, session: SessionHandle) -> Result<(), EdgeFailure> {
        self.sessions
            .lock()
            .expect("edge sessions lock poisoned")
            .peers
            .remove(&session.id);
        self.core
            .close_session(session.id)
            .map_err(|error| EdgeFailure(core_error(error)))
    }

    pub fn health(&self) -> HttpResponse {
        HttpResponse {
            status: 200,
            body: "{\"status\":\"ok\"}".to_owned(),
        }
    }
    pub fn readiness(&self) -> HttpResponse {
        HttpResponse {
            status: 200,
            body: "{\"status\":\"ready\",\"protocol_version\":1}".to_owned(),
        }
    }

    fn session_peer(&self, session_id: Uuid) -> Result<StablePeerId, EdgeFailure> {
        self.sessions
            .lock()
            .expect("edge sessions lock poisoned")
            .peers
            .get(&session_id)
            .cloned()
            .ok_or(EdgeFailure(EdgeError::SessionContextStale))
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
                    clear_generation: parse_u64(event.clear_generation)?,
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
    if request.target != "/v1/stream" {
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
    use std::{net::Ipv4Addr, sync::Mutex};

    use super::*;
    use tempfile::tempdir;

    const NOW: i64 = 1_700_000_000_000;

    struct FakeLocalApi {
        whois: Mutex<Result<String, LocalApiError>>,
    }

    impl FakeLocalApi {
        fn admitted() -> Self {
            Self {
                whois: Mutex::new(Ok("peer-reserved-example".to_owned())),
            }
        }
    }

    impl LocalApi for FakeLocalApi {
        fn self_addresses(&self) -> Result<Vec<IpAddr>, LocalApiError> {
            Ok(vec![IpAddr::V4(Ipv4Addr::new(100, 64, 0, 7))])
        }

        fn who_is(&self, _remote: SocketAddr) -> Result<String, LocalApiError> {
            self.whois.lock().unwrap().clone()
        }
    }

    #[derive(Default)]
    struct Frames(Vec<String>);
    impl WebSocketOutput for Frames {
        type Error = ();

        fn write_complete_text(&mut self, frame: &str) -> Result<(), ()> {
            self.0.push(frame.to_owned());
            Ok(())
        }
    }

    fn config() -> EdgeConfig {
        EdgeConfig {
            listen_address: "100.64.0.7:8123".parse().unwrap(),
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

    fn edge() -> (tempfile::TempDir, HubEdge<FakeLocalApi>) {
        let directory = tempdir().unwrap();
        let database = directory.path().join("hub.sqlite");
        let edge = HubEdge::prepare(config(), FakeLocalApi::admitted(), database).unwrap();
        (directory, edge)
    }

    fn request() -> HttpRequest {
        HttpRequest {
            method: "GET".to_owned(),
            target: "/v1/stream".to_owned(),
            headers: vec![("Sec-WebSocket-Protocol".to_owned(), PROTOCOL.to_owned())],
            header_bytes: 64,
        }
    }

    fn session(edge: &HubEdge<FakeLocalApi>) -> SessionHandle {
        let admitted = edge
            .admit_socket("100.64.0.9:12345".parse().unwrap())
            .unwrap();
        match edge.upgrade(admitted, &request()) {
            UpgradeResult::Upgraded(session) => session,
            UpgradeResult::Response(response) => panic!("unexpected response: {response:?}"),
        }
    }

    fn drain(edge: &HubEdge<FakeLocalApi>, session: SessionHandle) -> Frames {
        let mut frames = Frames::default();
        while edge.write_next_event(session, &mut frames).unwrap() {}
        frames
    }

    fn live(edge: &HubEdge<FakeLocalApi>) -> SessionHandle {
        let session = session(edge);
        edge.handle_text(session, r#"{"protocol_version":1,"type":"resume","known_history_epoch":null,"known_clear_generation":null,"after_cursor":null}"#, NOW).unwrap();
        let frames = drain(edge, session);
        assert_eq!(frames.0.len(), 2);
        session
    }

    #[test]
    fn rejects_unverified_peer_before_http_and_tailnet_bind_is_exact() {
        let directory = tempdir().unwrap();
        let failing = FakeLocalApi {
            whois: Mutex::new(Err(LocalApiError::PeerNotFound)),
        };
        let edge =
            HubEdge::prepare(config(), failing, directory.path().join("hub.sqlite")).unwrap();
        assert!(matches!(
            edge.admit_socket("100.64.0.8:1".parse().unwrap()),
            Err(EdgeFailure(EdgeError::TailnetPeerUnverified))
        ));

        let mut invalid = config();
        invalid.listen_address = "192.0.2.1:8123".parse().unwrap();
        assert!(matches!(
            HubEdge::prepare(
                invalid,
                FakeLocalApi::admitted(),
                directory.path().join("other.sqlite")
            ),
            Err(EdgeFailure(EdgeError::TailnetBindUnverified))
        ));
    }

    #[test]
    fn http_precedence_is_content_free_and_exact() {
        let (_directory, edge) = edge();
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
    }

    #[test]
    fn clear_retracts_unfinished_old_generation_output_before_a_later_write() {
        let (_directory, edge) = edge();
        let source = live(&edge);
        let target = live(&edge);
        edge.handle_text(source, r#"{"protocol_version":1,"type":"publish","event":{"message_id":"00000000-0000-4000-8000-000000000001","clear_generation":"1","created_at_ms":1700000000000,"content_type":"text/plain","payload_bytes":12,"content_sha256":"5cb72f90e968922d30557d0af8f719d21f61792becaa87eb32477767d739dc0b","payload_b64":"Zml4dHVyZSB0ZXh0"}}"#, NOW).unwrap();
        edge.handle_text(source, r#"{"protocol_version":1,"type":"clear_history","request_id":"00000000-0000-4000-8000-000000000002","expected_clear_generation":"1"}"#, NOW).unwrap();
        let target_frames = drain(&edge, target);
        assert!(target_frames
            .0
            .iter()
            .any(|frame| frame.contains("clear_notice")));
        assert!(target_frames
            .0
            .iter()
            .all(|frame| !frame.contains("Zml4dHVyZSB0ZXh0")));
    }

    #[test]
    fn duplicate_json_field_and_oversize_text_close_before_state_mutation() {
        let (_directory, edge) = edge();
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
}
