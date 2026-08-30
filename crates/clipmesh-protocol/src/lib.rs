//! Canonical, closed ClipMesh protocol-v1 schemas and content seam.
//!
//! Clipboard content and stable peer values use redacted diagnostics. Wire
//! decoding applies protocol-version precedence before closed-schema parsing.

use std::{fmt, str::FromStr};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{
    de::{self, DeserializeOwned},
    Deserialize, Deserializer, Serialize, Serializer,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::{Uuid, Variant, Version};

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_CLOCK_SKEW_MS: i64 = 120_000;
pub const HARD_MAX_PAYLOAD_BYTES: usize = 1_048_576;

/// The only representable outbound protocol version is version 1.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProtocolVersion;

impl Serialize for ProtocolVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(PROTOCOL_VERSION)
    }
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u8::deserialize(deserializer)? {
            PROTOCOL_VERSION => Ok(Self),
            _ => Err(de::Error::custom("unsupported protocol version")),
        }
    }
}

/// The fixed version-1 clock-skew limit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FixedClockSkewMs;

impl Serialize for FixedClockSkewMs {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(MAX_CLOCK_SKEW_MS)
    }
}

impl<'de> Deserialize<'de> for FixedClockSkewMs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match i64::deserialize(deserializer)? {
            MAX_CLOCK_SKEW_MS => Ok(Self),
            _ => Err(de::Error::custom("max_clock_skew_ms must be 120000")),
        }
    }
}

/// A positive unsigned-64 value in canonical decimal wire form.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct U64Decimal(u64);

impl U64Decimal {
    pub fn new(value: u64) -> Result<Self, &'static str> {
        (value != 0)
            .then_some(Self(value))
            .ok_or("decimal must be positive")
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl FromStr for U64Decimal {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("expected a positive canonical decimal");
        }
        Self::new(
            value
                .parse::<u64>()
                .map_err(|_| "decimal is outside unsigned 64-bit range")?,
        )
    }
}

impl Serialize for U64Decimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for U64Decimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

/// A lowercase canonical RFC-4122 UUIDv4.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct UuidV4(Uuid);

impl UuidV4 {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(value: Uuid) -> Result<Self, &'static str> {
        if value.get_version() == Some(Version::Random) && value.get_variant() == Variant::RFC4122 {
            Ok(Self(value))
        } else {
            Err("expected UUIDv4")
        }
    }

    pub fn get(&self) -> Uuid {
        self.0
    }
}

impl Default for UuidV4 {
    fn default() -> Self {
        Self::new()
    }
}

impl Serialize for UuidV4 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for UuidV4 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let uuid = Uuid::parse_str(&value).map_err(de::Error::custom)?;
        if value != uuid.to_string()
            || uuid.get_version() != Some(Version::Random)
            || uuid.get_variant() != Variant::RFC4122
        {
            return Err(de::Error::custom("expected lowercase canonical UUIDv4"));
        }
        Ok(Self(uuid))
    }
}

/// The nonempty stable peer value supplied by the trusted hub boundary.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct StablePeerId(String);

impl StablePeerId {
    pub fn from_boundary(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty() {
            return Err("stable peer ID must be nonempty");
        }
        Ok(Self(value))
    }

    pub fn as_boundary_value(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StablePeerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StablePeerId([redacted])")
    }
}

impl fmt::Display for StablePeerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl Serialize for StablePeerId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for StablePeerId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_boundary(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// A validated exact UTF-8 clipboard payload.
#[derive(Clone, Eq, PartialEq)]
pub struct ClipContentV1(Vec<u8>);

/// The canonical wire projection of [`ClipContentV1`].
#[derive(Clone, Eq, PartialEq)]
pub struct WireContentV1 {
    pub content_type: &'static str,
    pub payload_b64: String,
    pub payload_bytes: usize,
    pub content_sha256: String,
}

impl fmt::Debug for WireContentV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WireContentV1([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ContentError {
    #[error("content_type_unsupported")]
    ContentTypeUnsupported,
    #[error("payload_encoding_invalid")]
    PayloadEncodingInvalid,
    #[error("payload_empty")]
    PayloadEmpty,
    #[error("payload_too_large")]
    PayloadTooLarge,
    #[error("payload_length_mismatch")]
    PayloadLengthMismatch,
    #[error("payload_hash_mismatch")]
    PayloadHashMismatch,
    #[error("database_integrity_failed")]
    StorageIntegrity,
}

impl ClipContentV1 {
    pub fn from_wire(
        content_type: &str,
        payload_b64: &str,
        payload_bytes: usize,
        content_sha256: &str,
        max_payload_bytes: usize,
    ) -> Result<Self, ContentError> {
        if content_type != "text/plain" {
            return Err(ContentError::ContentTypeUnsupported);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| ContentError::PayloadEncodingInvalid)?;
        Self::validate(
            bytes,
            max_payload_bytes,
            Some((payload_bytes, content_sha256)),
        )
    }

    pub fn from_platform(bytes: &[u8], max_payload_bytes: usize) -> Result<Self, ContentError> {
        Self::validate(bytes.to_vec(), max_payload_bytes, None)
    }

    pub fn from_storage_blob(bytes: &[u8]) -> Result<Self, ContentError> {
        Self::validate(bytes.to_vec(), HARD_MAX_PAYLOAD_BYTES, None)
            .map_err(|_| ContentError::StorageIntegrity)
    }

    fn validate(
        bytes: Vec<u8>,
        max_payload_bytes: usize,
        wire_metadata: Option<(usize, &str)>,
    ) -> Result<Self, ContentError> {
        if bytes.is_empty() {
            return Err(ContentError::PayloadEmpty);
        }
        if std::str::from_utf8(&bytes).is_err() {
            return Err(ContentError::PayloadEncodingInvalid);
        }
        if bytes.len() > max_payload_bytes {
            return Err(ContentError::PayloadTooLarge);
        }
        if let Some((declared_length, declared_hash)) = wire_metadata {
            if declared_length != bytes.len() {
                return Err(ContentError::PayloadLengthMismatch);
            }
            if declared_hash != sha256_hex(&bytes) {
                return Err(ContentError::PayloadHashMismatch);
            }
        }
        Ok(Self(bytes))
    }

    pub fn as_storage_blob(&self) -> &[u8] {
        &self.0
    }

    pub fn to_wire(&self) -> WireContentV1 {
        WireContentV1 {
            content_type: "text/plain",
            payload_b64: URL_SAFE_NO_PAD.encode(&self.0),
            payload_bytes: self.0.len(),
            content_sha256: sha256_hex(&self.0),
        }
    }

    pub fn to_platform(&self) -> &[u8] {
        &self.0
    }

    pub fn same_content(&self, other: &Self) -> bool {
        self.0 == other.0
    }

    pub fn to_preview(&self, scalar_limit: usize) -> String {
        let text = std::str::from_utf8(&self.0).expect("ClipContentV1 is valid UTF-8");
        let mut preview = String::new();
        let mut whitespace = false;
        for character in text.chars() {
            let character = if character.is_control()
                && character != '\t'
                && character != '\r'
                && character != '\n'
            {
                '\u{fffd}'
            } else {
                character
            };
            if character.is_whitespace() {
                if whitespace {
                    continue;
                }
                whitespace = true;
                if preview.chars().count() == scalar_limit {
                    break;
                }
                preview.push(' ');
            } else {
                whitespace = false;
                if preview.chars().count() == scalar_limit {
                    break;
                }
                preview.push(character);
            }
        }
        preview
    }
}

impl fmt::Debug for ClipContentV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ClipContentV1([redacted])")
    }
}

impl fmt::Display for ClipContentV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    ConfigParseFailed,
    ConfigUnknownField,
    ConfigMissingRequired,
    ConfigValueInvalid,
    TailscaleLocalapiUnavailable,
    TailnetBindUnverified,
    TailnetPeerUnverified,
    BindFailed,
    StatePathInsecure,
    LocalStateUnavailable,
    DatabaseSchemaUnsupported,
    DatabaseIntegrityFailed,
    StorageUnavailable,
    HttpPathNotFound,
    HttpMethodNotAllowed,
    RequestHeadersTooLarge,
    ClientIdentityClaimForbidden,
    ConnectionLimitReached,
    RequestRateLimited,
    MessageTooLarge,
    MessageRateLimited,
    ProtocolVersionUnsupported,
    ProtocolSchemaInvalid,
    ResumeRequired,
    ResumeDeadlineExceeded,
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
    PublishRateLimited,
    HubCursorExhausted,
    HistoryCleared,
    AckInvalid,
    SlowConsumer,
    HeartbeatTimeout,
    AdapterUnavailable,
    LockStateUnknown,
    OutboxFull,
}

impl FailureCode {
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::TailscaleLocalapiUnavailable
                | Self::TailnetPeerUnverified
                | Self::BindFailed
                | Self::LocalStateUnavailable
                | Self::DatabaseIntegrityFailed
                | Self::StorageUnavailable
                | Self::ConnectionLimitReached
                | Self::RequestRateLimited
                | Self::MessageRateLimited
                | Self::ResumeDeadlineExceeded
                | Self::SessionContextStale
                | Self::ClearGenerationAhead
                | Self::CreatedAtInFuture
                | Self::PublishRateLimited
                | Self::HistoryCleared
                | Self::SlowConsumer
                | Self::HeartbeatTimeout
                | Self::AdapterUnavailable
                | Self::LockStateUnknown
                | Self::OutboxFull
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureResponse {
    code: FailureCode,
}

impl FailureResponse {
    pub fn new(code: FailureCode) -> Self {
        Self { code }
    }

    pub fn code(&self) -> FailureCode {
        self.code
    }

    pub fn retryable(&self) -> bool {
        self.code.retryable()
    }
}

impl Serialize for FailureResponse {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire {
            code: FailureCode,
            retryable: bool,
        }
        Wire {
            code: self.code,
            retryable: self.retryable(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FailureResponse {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            code: FailureCode,
            retryable: bool,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.retryable != wire.code.retryable() {
            return Err(de::Error::custom(
                "retryable must match the stable failure code",
            ));
        }
        Ok(Self::new(wire.code))
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublishEventV1 {
    pub message_id: UuidV4,
    pub clear_generation: U64Decimal,
    pub created_at_ms: i64,
    pub content_type: String,
    pub payload_bytes: u32,
    pub content_sha256: String,
    pub payload_b64: String,
}

impl fmt::Debug for PublishEventV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublishEventV1([redacted])")
    }
}

impl PublishEventV1 {
    pub fn content(&self, max_payload_bytes: usize) -> Result<ClipContentV1, ContentError> {
        ClipContentV1::from_wire(
            &self.content_type,
            &self.payload_b64,
            self.payload_bytes as usize,
            &self.content_sha256,
            max_payload_bytes,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LimitsV1 {
    pub max_payload_bytes: u32,
    pub retention_seconds: u64,
    pub history_max_entries: u32,
    pub max_clock_skew_ms: FixedClockSkewMs,
    pub max_websocket_message_bytes: u32,
}

impl LimitsV1 {
    pub fn new(
        max_payload_bytes: u32,
        retention_seconds: u64,
        history_max_entries: u32,
        max_websocket_message_bytes: u32,
    ) -> Result<Self, &'static str> {
        if !(1..=1_048_576).contains(&max_payload_bytes)
            || !(60..=31_536_000).contains(&retention_seconds)
            || !(1..=10_000).contains(&history_max_entries)
        {
            return Err("limits value is outside the version-1 range");
        }
        let expected = 4 * max_payload_bytes.div_ceil(3) + 4096;
        if max_websocket_message_bytes != expected {
            return Err("max_websocket_message_bytes does not match max_payload_bytes");
        }
        Ok(Self {
            max_payload_bytes,
            retention_seconds,
            history_max_entries,
            max_clock_skew_ms: FixedClockSkewMs,
            max_websocket_message_bytes,
        })
    }
}

impl<'de> Deserialize<'de> for LimitsV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            max_payload_bytes: u32,
            retention_seconds: u64,
            history_max_entries: u32,
            max_clock_skew_ms: FixedClockSkewMs,
            max_websocket_message_bytes: u32,
        }
        let wire = Wire::deserialize(deserializer)?;
        let _ = wire.max_clock_skew_ms;
        Self::new(
            wire.max_payload_bytes,
            wire.retention_seconds,
            wire.history_max_entries,
            wire.max_websocket_message_bytes,
        )
        .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeStatus {
    Fresh,
    Complete,
    Gap,
    EpochChanged,
    GenerationChanged,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Resume,
    Live,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessageV1 {
    Resume {
        protocol_version: ProtocolVersion,
        known_history_epoch: Option<UuidV4>,
        known_clear_generation: Option<U64Decimal>,
        after_cursor: Option<U64Decimal>,
    },
    Publish {
        protocol_version: ProtocolVersion,
        event: PublishEventV1,
    },
    Ack {
        protocol_version: ProtocolVersion,
        history_epoch: UuidV4,
        clear_generation: U64Decimal,
        cursor: U64Decimal,
    },
    ClearHistory {
        protocol_version: ProtocolVersion,
        request_id: UuidV4,
        expected_clear_generation: U64Decimal,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessageV1 {
    ServerHello {
        protocol_version: ProtocolVersion,
        session_id: UuidV4,
        self_peer_id: StablePeerId,
        history_epoch: UuidV4,
        clear_generation: U64Decimal,
        newest_cursor: Option<U64Decimal>,
        server_time_ms: i64,
        limits: LimitsV1,
    },
    ResumeStarted {
        protocol_version: ProtocolVersion,
        history_epoch: UuidV4,
        clear_generation: U64Decimal,
        status: ResumeStatus,
        requested_after_cursor: Option<U64Decimal>,
        boundary_cursor: Option<U64Decimal>,
        lost_through_cursor: Option<U64Decimal>,
    },
    Event {
        protocol_version: ProtocolVersion,
        history_epoch: UuidV4,
        clear_generation: U64Decimal,
        cursor: U64Decimal,
        delivery: Delivery,
        accepted_at_ms: i64,
        expires_at_ms: i64,
        source_peer_id: StablePeerId,
        event: PublishEventV1,
    },
    ResumeComplete {
        protocol_version: ProtocolVersion,
        history_epoch: UuidV4,
        clear_generation: U64Decimal,
        boundary_cursor: Option<U64Decimal>,
    },
    PublishAccepted {
        protocol_version: ProtocolVersion,
        message_id: UuidV4,
        cursor: U64Decimal,
        expires_at_ms: i64,
        duplicate: bool,
    },
    PublishRejected {
        protocol_version: ProtocolVersion,
        message_id: Option<UuidV4>,
        #[serde(flatten)]
        failure: FailureResponse,
    },
    ClearAccepted {
        protocol_version: ProtocolVersion,
        request_id: UuidV4,
        clear_generation: U64Decimal,
        cleared_through_cursor: Option<U64Decimal>,
        duplicate: bool,
    },
    ClearRejected {
        protocol_version: ProtocolVersion,
        request_id: Option<UuidV4>,
        #[serde(flatten)]
        failure: FailureResponse,
    },
    ClearNotice {
        protocol_version: ProtocolVersion,
        request_id: UuidV4,
        clear_generation: U64Decimal,
        cleared_through_cursor: Option<U64Decimal>,
    },
    Error {
        protocol_version: ProtocolVersion,
        #[serde(flatten)]
        failure: FailureResponse,
    },
}

#[derive(Deserialize)]
struct VersionEnvelope {
    protocol_version: serde_json::Value,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DecodeError {
    #[error("protocol version is unsupported")]
    ProtocolVersionUnsupported,
    #[error("protocol schema is invalid")]
    ProtocolSchemaInvalid,
}

fn ensure_v1(text: &str) -> Result<(), DecodeError> {
    let envelope: VersionEnvelope =
        serde_json::from_str(text).map_err(|_| DecodeError::ProtocolSchemaInvalid)?;
    let serde_json::Value::Number(number) = envelope.protocol_version else {
        return Err(DecodeError::ProtocolSchemaInvalid);
    };
    let integer = number.to_string();
    let digits = integer.strip_prefix('-').unwrap_or(&integer);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(DecodeError::ProtocolSchemaInvalid);
    }
    if integer != PROTOCOL_VERSION.to_string() {
        return Err(DecodeError::ProtocolVersionUnsupported);
    }
    Ok(())
}

mod inbound_v1 {
    pub trait Sealed {}
}

/// A closed protocol schema decoded through the version-1 preflight.
pub trait InboundV1: inbound_v1::Sealed + DeserializeOwned {}

macro_rules! inbound_v1 {
    ($($type:ty),+ $(,)?) => {
        $(
            impl inbound_v1::Sealed for $type {}
            impl InboundV1 for $type {}
        )+
    };
}

inbound_v1!(ClientMessageV1, ServerMessageV1);

pub fn decode_inbound_v1<T: InboundV1>(text: &str) -> Result<T, DecodeError> {
    ensure_v1(text)?;
    serde_json::from_str(text).map_err(|_| DecodeError::ProtocolSchemaInvalid)
}

pub fn decode_client_message(text: &str) -> Result<ClientMessageV1, DecodeError> {
    decode_inbound_v1(text)
}

pub fn decode_server_message(text: &str) -> Result<ServerMessageV1, DecodeError> {
    decode_inbound_v1(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    const FIXTURE: &str = include_str!("../../../fixtures/protocol/publish-v1.json");

    #[test]
    fn canonical_publish_fixture_round_trips_through_content_seam() {
        let message = decode_client_message(FIXTURE).unwrap();
        let ClientMessageV1::Publish {
            protocol_version,
            event,
        } = &message
        else {
            panic!("fixture must publish");
        };
        assert_eq!(*protocol_version, ProtocolVersion);
        let content = event.content(262_144).unwrap();
        assert_eq!(content.to_platform(), b"fixture text");
        let fixture_value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(serde_json::to_value(message).unwrap(), fixture_value);
    }

    #[test]
    fn rejects_version_before_message_fields() {
        assert_matches!(
            decode_client_message(r#"{"protocol_version":2,"type":"publish","event":"wrong"}"#),
            Err(DecodeError::ProtocolVersionUnsupported)
        );
        for version in ["256", "-1", "18446744073709551615", "18446744073709551616"] {
            assert_matches!(
                decode_client_message(&format!(
                    r#"{{"protocol_version":{version},"type":"publish","event":"wrong"}}"#
                )),
                Err(DecodeError::ProtocolVersionUnsupported)
            );
        }
    }

    #[test]
    fn schemas_are_closed_and_scalars_are_canonical() {
        for input in [
            r#"{"protocol_version":1,"type":"resume","known_history_epoch":null,"known_clear_generation":null,"after_cursor":null,"extra":true}"#,
            r#"{"protocol_version":1,"type":"resume","known_history_epoch":null,"known_clear_generation":null,"after_cursor":"01"}"#,
            r#"{"protocol_version":1,"type":"resume","known_history_epoch":null,"known_clear_generation":null,"after_cursor":"1","after_cursor":"2"}"#,
        ] {
            assert_matches!(
                decode_client_message(input),
                Err(DecodeError::ProtocolSchemaInvalid)
            );
        }
    }

    #[test]
    fn content_diagnostics_are_redacted_and_validation_is_ordered() {
        let ClientMessageV1::Publish { mut event, .. } = decode_client_message(FIXTURE).unwrap()
        else {
            unreachable!()
        };
        let content = event.content(262_144).unwrap();
        assert_eq!(format!("{content:?}"), "ClipContentV1([redacted])");
        assert_eq!(format!("{}", content), "[redacted]");
        assert_eq!(format!("{event:?}"), "PublishEventV1([redacted])");
        assert_eq!(
            format!("{:?}", content.to_wire()),
            "WireContentV1([redacted])"
        );

        event.payload_bytes += 1;
        assert_eq!(
            event.content(262_144),
            Err(ContentError::PayloadLengthMismatch)
        );
        event.payload_bytes -= 1;
        event.content_sha256 = "a".repeat(64);
        assert_eq!(
            event.content(262_144),
            Err(ContentError::PayloadHashMismatch)
        );
        event.payload_b64 = "not+base64".to_owned();
        assert_eq!(
            event.content(262_144),
            Err(ContentError::PayloadEncodingInvalid)
        );
    }

    #[test]
    fn all_client_message_shapes_are_closed() {
        for input in [
            r#"{"protocol_version":1,"type":"resume","known_history_epoch":null,"known_clear_generation":null,"after_cursor":null}"#,
            FIXTURE,
            r#"{"protocol_version":1,"type":"ack","history_epoch":"00000000-0000-4000-8000-000000000002","clear_generation":"1","cursor":"1"}"#,
            r#"{"protocol_version":1,"type":"clear_history","request_id":"00000000-0000-4000-8000-000000000003","expected_clear_generation":"1"}"#,
        ] {
            assert!(decode_client_message(input).is_ok(), "{input}");
        }
    }

    #[test]
    fn server_failure_retryability_is_not_caller_selectable() {
        assert_matches!(
            decode_server_message(
                r#"{"protocol_version":1,"type":"error","code":"payload_empty","retryable":true}"#
            ),
            Err(DecodeError::ProtocolSchemaInvalid)
        );
        let message = decode_server_message(
            r#"{"protocol_version":1,"type":"error","code":"payload_empty","retryable":false}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(message).unwrap(),
            serde_json::json!({
                "protocol_version": 1,
                "type": "error",
                "code": "payload_empty",
                "retryable": false
            })
        );
    }

    #[test]
    fn limits_validate_the_computed_message_bound() {
        let valid = LimitsV1::new(262_144, 604_800, 500, 353_624).unwrap();
        assert_eq!(valid.max_clock_skew_ms, FixedClockSkewMs);
        assert!(LimitsV1::new(262_144, 604_800, 500, 353_623).is_err());
    }

    #[test]
    fn storage_revalidation_collapses_to_integrity_failure() {
        assert_eq!(
            ClipContentV1::from_storage_blob(b""),
            Err(ContentError::StorageIntegrity)
        );
        let bytes = [0xff];
        assert_eq!(
            ClipContentV1::from_storage_blob(&bytes),
            Err(ContentError::StorageIntegrity)
        );
    }
}
