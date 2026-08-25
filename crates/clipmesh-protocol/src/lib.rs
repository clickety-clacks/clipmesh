//! Canonical, closed ClipMesh protocol-v1 domain and wire types.
//!
//! The only APIs that expose clipboard bytes or credential bytes are the
//! explicitly named wire methods below. Diagnostic formatting is redacted.

use std::{fmt, str::FromStr};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::{Uuid, Variant, Version};

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_CLOCK_SKEW_MS: i64 = 120_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct U64Decimal(u64);

impl U64Decimal {
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
        let number = value
            .parse::<u64>()
            .map_err(|_| "decimal is outside unsigned 64-bit range")?;
        if number == 0 {
            return Err("decimal must be positive");
        }
        Ok(Self(number))
    }
}

impl Serialize for U64Decimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for U64Decimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct UuidV4(Uuid);

impl UuidV4 {
    pub fn get(&self) -> Uuid {
        self.0
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

#[derive(Clone, Eq, PartialEq)]
pub struct ClipboardPayload(String);

impl ClipboardPayload {
    pub fn from_wire(value: &str) -> Result<Self, &'static str> {
        let payload = Self(value.to_owned());
        payload.decode_wire_bytes()?;
        Ok(payload)
    }
    pub fn wire_value(&self) -> &str {
        &self.0
    }
    /// This explicit method is the protocol's permitted payload-byte exposure.
    pub fn decode_wire_bytes(&self) -> Result<Vec<u8>, &'static str> {
        let bytes = URL_SAFE_NO_PAD
            .decode(&self.0)
            .map_err(|_| "expected unpadded base64url payload")?;
        std::str::from_utf8(&bytes).map_err(|_| "payload must be valid UTF-8")?;
        Ok(bytes)
    }
}
impl fmt::Debug for ClipboardPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED_CLIPBOARD_PAYLOAD]")
    }
}
impl fmt::Display for ClipboardPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED_CLIPBOARD_PAYLOAD]")
    }
}
impl Serialize for ClipboardPayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_value())
    }
}
impl<'de> Deserialize<'de> for ClipboardPayload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        // Payload encoding is validated after timestamp and content-type
        // precedence, where it can return payload_encoding_invalid.
        Ok(Self(value))
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ContentHash(String);
impl ContentHash {
    pub fn from_payload(payload: &ClipboardPayload) -> Self {
        let bytes = payload
            .decode_wire_bytes()
            .expect("payload was already validated");
        Self(hex(&Sha256::digest(bytes)))
    }
}
impl fmt::Debug for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED_CONTENT_HASH]")
    }
}
impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED_CONTENT_HASH]")
    }
}
impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Hash canonicality is part of publish-field validation and maps to
        // payload_hash_mismatch, rather than the JSON-schema failure code.
        Ok(Self(String::deserialize(deserializer)?))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

macro_rules! credential {
    ($name:ident, $prefix:literal, $marker:literal) => {
        #[derive(Clone, Eq, PartialEq)]
        pub struct $name([u8; 32]);
        impl $name {
            pub fn from_wire(value: &str) -> Result<Self, &'static str> {
                let encoded = value
                    .strip_prefix($prefix)
                    .ok_or("credential has wrong class")?;
                if encoded.len() != 43 {
                    return Err("credential has wrong length");
                }
                let decoded = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .map_err(|_| "credential is not base64url")?;
                let bytes: [u8; 32] = decoded
                    .try_into()
                    .map_err(|_| "credential has wrong byte length")?;
                Ok(Self(bytes))
            }
            /// This explicit method is the protocol's permitted credential exposure.
            pub fn wire_value(&self) -> String {
                format!("{}{}", $prefix, URL_SAFE_NO_PAD.encode(self.0))
            }
        }
        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str($marker)
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str($marker)
            }
        }
    };
}
credential!(
    DeviceCredential,
    "cm_dev_v1_",
    "[REDACTED_DEVICE_CREDENTIAL]"
);
credential!(
    AdministratorCredential,
    "cm_admin_v1_",
    "[REDACTED_ADMINISTRATOR_CREDENTIAL]"
);
credential!(
    EnrollmentArtifact,
    "cm_enroll_v1_",
    "[REDACTED_ENROLLMENT_ARTIFACT]"
);

#[derive(Clone, Eq, PartialEq)]
pub struct DeviceDisplayName(String);
impl fmt::Debug for DeviceDisplayName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED_DEVICE_DISPLAY_NAME]")
    }
}
impl fmt::Display for DeviceDisplayName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED_DEVICE_DISPLAY_NAME]")
    }
}
impl Serialize for DeviceDisplayName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for DeviceDisplayName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let scalar_count = value.chars().count();
        if !(1..=64).contains(&scalar_count) || value.chars().any(|character| matches!(character as u32, 0x00..=0x1f | 0x7f | 0x202a..=0x202e | 0x2066..=0x2069)) {
            return Err(de::Error::custom("invalid device display name"));
        }
        Ok(Self(value))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    LinuxWayland,
    Macos,
    Ios,
    Ipados,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    ConfigParseFailed,
    ConfigUnknownField,
    ConfigMissingRequired,
    ConfigValueInvalid,
    BindAddressDisallowed,
    BindFailed,
    TlsMaterialInvalid,
    TlsCertificateNotCurrent,
    SecretFileInsecure,
    StatePathInsecure,
    LocalStateUnavailable,
    DatabaseSchemaUnsupported,
    DatabaseIntegrityFailed,
    HttpPathNotFound,
    HttpMethodNotAllowed,
    HttpContentTypeUnsupported,
    Unauthorized,
    AdministrativelyPaused,
    ConnectionLimitReached,
    RequestRateLimited,
    RequestTooLarge,
    MessageTooLarge,
    MessageRateLimited,
    ProtocolVersionUnsupported,
    ProtocolSchemaInvalid,
    ResumeRequired,
    ResumeDeadlineExceeded,
    CursorAhead,
    ResumeCursorWithoutEpoch,
    SessionEpochStale,
    SourceDeviceMismatch,
    MessageIdConflict,
    MessageIdReplay,
    SourceSequenceReplay,
    CreatedAtInFuture,
    EventExpired,
    ExpiryExceedsRetention,
    ContentTypeUnsupported,
    PayloadEmpty,
    PayloadTooLarge,
    PayloadEncodingInvalid,
    PayloadLengthMismatch,
    PayloadHashMismatch,
    PublishRateLimited,
    HubCursorExhausted,
    DeviceSequenceExhausted,
    AckInvalid,
    SlowConsumer,
    HeartbeatTimeout,
    CredentialRotated,
    DeviceRevoked,
    HistoryPurged,
    EnrollmentArtifactInvalid,
    SecretResultAlreadyCommitted,
    RequestIdConflict,
    CredentialStorageFailed,
    StorageUnavailable,
    AdapterUnavailable,
    LockStateUnknown,
    OutboxFull,
    TlsValidationFailed,
}

impl FailureCode {
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::BindFailed
                | Self::TlsCertificateNotCurrent
                | Self::StorageUnavailable
                | Self::ConnectionLimitReached
                | Self::RequestRateLimited
                | Self::MessageRateLimited
                | Self::ResumeDeadlineExceeded
                | Self::SessionEpochStale
                | Self::AdministrativelyPaused
                | Self::PublishRateLimited
                | Self::SlowConsumer
                | Self::HeartbeatTimeout
                | Self::HistoryPurged
                | Self::OutboxFull
                | Self::CreatedAtInFuture
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClipboardEventV1 {
    pub message_id: UuidV4,
    pub source_device_id: UuidV4,
    pub source_seq: U64Decimal,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub content_type: String,
    pub payload_bytes: u32,
    pub content_sha256: ContentHash,
    pub payload_b64: ClipboardPayload,
}

impl ClipboardEventV1 {
    pub fn validate(
        &self,
        hub_time_ms: i64,
        max_payload_bytes: u32,
        retention_seconds: u64,
    ) -> Result<(), FailureCode> {
        if self.created_at_ms > hub_time_ms.saturating_add(MAX_CLOCK_SKEW_MS) {
            return Err(FailureCode::CreatedAtInFuture);
        }
        if self.expires_at_ms <= hub_time_ms {
            return Err(FailureCode::EventExpired);
        }
        let latest_expiry = self
            .created_at_ms
            .saturating_add((retention_seconds.saturating_mul(1000)).min(i64::MAX as u64) as i64);
        if self.expires_at_ms > latest_expiry {
            return Err(FailureCode::ExpiryExceedsRetention);
        }
        if self.content_type != "text/plain" {
            return Err(FailureCode::ContentTypeUnsupported);
        }
        let payload = self
            .payload_b64
            .decode_wire_bytes()
            .map_err(|_| FailureCode::PayloadEncodingInvalid)?;
        let payload_length = payload.len();
        if payload_length == 0 {
            return Err(FailureCode::PayloadEmpty);
        }
        if payload_length > max_payload_bytes as usize {
            return Err(FailureCode::PayloadTooLarge);
        }
        if self.payload_bytes as usize != payload_length {
            return Err(FailureCode::PayloadLengthMismatch);
        }
        if self.content_sha256.0.len() != 64
            || !self
                .content_sha256
                .0
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.content_sha256 != ContentHash::from_payload(&self.payload_b64)
        {
            return Err(FailureCode::PayloadHashMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsV1 {
    pub max_payload_bytes: u32,
    pub retention_seconds: u64,
    pub history_max_entries: u32,
    pub max_clock_skew_ms: u32,
    pub max_websocket_message_bytes: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeStatus {
    Fresh,
    Complete,
    Gap,
    EpochChanged,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Resume,
    Live,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessageV1 {
    Resume {
        protocol_version: u8,
        known_history_epoch: Option<UuidV4>,
        after_cursor: Option<U64Decimal>,
    },
    Publish {
        protocol_version: u8,
        event: ClipboardEventV1,
    },
    Ack {
        protocol_version: u8,
        history_epoch: UuidV4,
        cursor: U64Decimal,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessageV1 {
    ServerHello {
        protocol_version: u8,
        session_id: UuidV4,
        device_id: UuidV4,
        device_display_name: DeviceDisplayName,
        server_time_ms: i64,
        history_epoch: UuidV4,
        newest_cursor: Option<U64Decimal>,
        limits: LimitsV1,
    },
    ResumeStarted {
        protocol_version: u8,
        history_epoch: UuidV4,
        status: ResumeStatus,
        requested_after_cursor: Option<U64Decimal>,
        boundary_cursor: Option<U64Decimal>,
        lost_through_cursor: Option<U64Decimal>,
    },
    Event {
        protocol_version: u8,
        history_epoch: UuidV4,
        cursor: U64Decimal,
        delivery: Delivery,
        accepted_at_ms: i64,
        source_display_name: DeviceDisplayName,
        event: ClipboardEventV1,
    },
    ResumeComplete {
        protocol_version: u8,
        history_epoch: UuidV4,
        boundary_cursor: Option<U64Decimal>,
    },
    PublishAccepted {
        protocol_version: u8,
        message_id: UuidV4,
        cursor: U64Decimal,
        expires_at_ms: i64,
        duplicate: bool,
    },
    PublishRejected {
        protocol_version: u8,
        message_id: Option<UuidV4>,
        code: FailureCode,
        retryable: bool,
    },
    PauseNotice {
        protocol_version: u8,
        scope: String,
        reason_code: FailureCode,
    },
    PurgeNotice {
        protocol_version: u8,
        purge_id: UuidV4,
        history_epoch: UuidV4,
        purged_through_cursor: Option<U64Decimal>,
    },
    Error {
        protocol_version: u8,
        code: FailureCode,
        retryable: bool,
    },
}

#[derive(Deserialize)]
struct VersionEnvelope {
    protocol_version: u8,
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
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err(DecodeError::ProtocolVersionUnsupported);
    }
    Ok(())
}

/// Decodes a closed client-to-hub version-1 JSON object.
pub fn decode_client_message(text: &str) -> Result<ClientMessageV1, DecodeError> {
    ensure_v1(text)?;
    serde_json::from_str(text).map_err(|_| DecodeError::ProtocolSchemaInvalid)
}
/// Decodes a closed hub-to-client version-1 JSON object.
pub fn decode_server_message(text: &str) -> Result<ServerMessageV1, DecodeError> {
    ensure_v1(text)?;
    serde_json::from_str(text).map_err(|_| DecodeError::ProtocolSchemaInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    const FIXTURE: &str = include_str!("../../../fixtures/protocol/publish-v1.json");

    #[test]
    fn canonical_publish_fixture_round_trips_and_validates() {
        let message = decode_client_message(FIXTURE).unwrap();
        let ClientMessageV1::Publish {
            protocol_version,
            event,
        } = &message
        else {
            panic!("fixture must publish");
        };
        assert_eq!(*protocol_version, 1);
        event.validate(1_700_000_000_000, 262_144, 14_400).unwrap();
        let fixture_value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(serde_json::to_value(message).unwrap(), fixture_value);
    }

    #[test]
    fn rejects_version_before_message_fields() {
        assert_matches!(
            decode_client_message(r#"{"protocol_version":2,"type":"publish","event":"wrong"}"#),
            Err(DecodeError::ProtocolVersionUnsupported)
        );
    }

    #[test]
    fn schemas_are_closed_and_scalars_are_canonical() {
        for input in [
            r#"{"protocol_version":1,"type":"resume","known_history_epoch":null,"after_cursor":null,"extra":true}"#,
            r#"{"protocol_version":1,"type":"resume","known_history_epoch":null,"after_cursor":"01"}"#,
            r#"{"protocol_version":1,"type":"resume","known_history_epoch":null,"after_cursor":"1","after_cursor":"2"}"#,
        ] {
            assert_matches!(
                decode_client_message(input),
                Err(DecodeError::ProtocolSchemaInvalid)
            );
        }
    }

    #[test]
    fn payload_and_secret_diagnostics_are_redacted() {
        let payload = ClipboardPayload::from_wire("Zml4dHVyZSB0ZXh0").unwrap();
        let hash = ContentHash::from_payload(&payload);
        assert_eq!(format!("{payload:?}"), "[REDACTED_CLIPBOARD_PAYLOAD]");
        assert_eq!(format!("{hash}"), "[REDACTED_CONTENT_HASH]");
        let credential =
            DeviceCredential::from_wire(&format!("{}{}", "cm_dev_v1_", "A".repeat(43))).unwrap();
        assert_eq!(format!("{credential}"), "[REDACTED_DEVICE_CREDENTIAL]");
    }

    #[test]
    fn event_validation_returns_stable_content_free_codes() {
        let ClientMessageV1::Publish { event, .. } = decode_client_message(FIXTURE).unwrap() else {
            unreachable!()
        };
        let mut wrong_length = event.clone();
        wrong_length.payload_bytes += 1;
        assert_eq!(
            wrong_length.validate(1_700_000_000_000, 262_144, 14_400),
            Err(FailureCode::PayloadLengthMismatch)
        );
        let mut wrong_hash = event;
        wrong_hash.content_sha256 = serde_json::from_str(
            "\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"",
        )
        .unwrap();
        assert_eq!(
            wrong_hash.validate(1_700_000_000_000, 262_144, 14_400),
            Err(FailureCode::PayloadHashMismatch)
        );
    }

    #[test]
    fn publish_payload_encoding_is_not_a_schema_error() {
        let invalid_encoding = FIXTURE.replace("Zml4dHVyZSB0ZXh0", "not+base64");
        let ClientMessageV1::Publish { event, .. } =
            decode_client_message(&invalid_encoding).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            event.validate(1_700_000_000_000, 262_144, 14_400),
            Err(FailureCode::PayloadEncodingInvalid)
        );
    }
}
