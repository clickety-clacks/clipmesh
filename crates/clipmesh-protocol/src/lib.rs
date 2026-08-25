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

/// The only representable outbound protocol version is the reviewed v1 value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProtocolVersion;
impl Serialize for ProtocolVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(PROTOCOL_VERSION)
    }
}
impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let version = u8::deserialize(deserializer)?;
        if version == PROTOCOL_VERSION {
            Ok(Self)
        } else {
            Err(de::Error::custom("unsupported protocol version"))
        }
    }
}

/// The v1 wire limit is fixed; a peer cannot select a weaker clock-skew rule.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FixedClockSkewMs;
impl Serialize for FixedClockSkewMs {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(MAX_CLOCK_SKEW_MS)
    }
}
impl<'de> Deserialize<'de> for FixedClockSkewMs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if i64::deserialize(deserializer)? == MAX_CLOCK_SKEW_MS {
            Ok(Self)
        } else {
            Err(de::Error::custom("max_clock_skew_ms must be 120000"))
        }
    }
}

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
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.wire_value())
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::from_wire(&String::deserialize(deserializer)?).map_err(de::Error::custom)
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
    pub max_clock_skew_ms: FixedClockSkewMs,
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
#[serde(rename_all = "snake_case")]
pub enum PauseScope {
    Global,
    Device,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PauseReasonCode;
impl Serialize for PauseReasonCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("administratively_paused")
    }
}
impl<'de> Deserialize<'de> for PauseReasonCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if String::deserialize(deserializer)? == "administratively_paused" {
            Ok(Self)
        } else {
            Err(de::Error::custom(
                "pause notice reason must be administratively_paused",
            ))
        }
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
    pub fn code(&self) -> &FailureCode {
        &self.code
    }
}
impl Serialize for FailureResponse {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            code: &'a FailureCode,
            retryable: bool,
        }
        Wire {
            code: &self.code,
            retryable: self.code.retryable(),
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessageV1 {
    Resume {
        protocol_version: ProtocolVersion,
        known_history_epoch: Option<UuidV4>,
        after_cursor: Option<U64Decimal>,
    },
    Publish {
        protocol_version: ProtocolVersion,
        event: ClipboardEventV1,
    },
    Ack {
        protocol_version: ProtocolVersion,
        history_epoch: UuidV4,
        cursor: U64Decimal,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessageV1 {
    ServerHello {
        protocol_version: ProtocolVersion,
        session_id: UuidV4,
        device_id: UuidV4,
        device_display_name: DeviceDisplayName,
        server_time_ms: i64,
        history_epoch: UuidV4,
        newest_cursor: Option<U64Decimal>,
        limits: LimitsV1,
    },
    ResumeStarted {
        protocol_version: ProtocolVersion,
        history_epoch: UuidV4,
        status: ResumeStatus,
        requested_after_cursor: Option<U64Decimal>,
        boundary_cursor: Option<U64Decimal>,
        lost_through_cursor: Option<U64Decimal>,
    },
    Event {
        protocol_version: ProtocolVersion,
        history_epoch: UuidV4,
        cursor: U64Decimal,
        delivery: Delivery,
        accepted_at_ms: i64,
        source_display_name: DeviceDisplayName,
        event: ClipboardEventV1,
    },
    ResumeComplete {
        protocol_version: ProtocolVersion,
        history_epoch: UuidV4,
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
    PauseNotice {
        protocol_version: ProtocolVersion,
        scope: PauseScope,
        reason_code: PauseReasonCode,
    },
    PurgeNotice {
        protocol_version: ProtocolVersion,
        purge_id: UuidV4,
        history_epoch: UuidV4,
        purged_through_cursor: Option<U64Decimal>,
    },
    Error {
        protocol_version: ProtocolVersion,
        #[serde(flatten)]
        failure: FailureResponse,
    },
}

/// Control-plane schemas are path-selected: each named request maps to exactly
/// one HTTPS endpoint in Architecture 6.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedPlatform {
    LinuxWayland,
    Macos,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MobilePlatform {
    Ios,
    Ipados,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceState {
    Active,
    Pending,
    Revoked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateManagedDeviceRequestV1 {
    pub protocol_version: ProtocolVersion,
    pub request_id: UuidV4,
    pub display_name: DeviceDisplayName,
    pub platform: ManagedPlatform,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateEnrollmentArtifactRequestV1 {
    pub protocol_version: ProtocolVersion,
    pub request_id: UuidV4,
    pub display_name: DeviceDisplayName,
    pub platform: MobilePlatform,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollRequestV1 {
    pub protocol_version: ProtocolVersion,
    pub request_id: UuidV4,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RotateCredentialRequestV1 {
    pub protocol_version: ProtocolVersion,
    pub request_id: UuidV4,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeDeviceRequestV1 {
    pub protocol_version: ProtocolVersion,
    pub request_id: UuidV4,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeHistoryRequestV1 {
    pub protocol_version: ProtocolVersion,
    pub request_id: UuidV4,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetPauseStateRequestV1 {
    pub protocol_version: ProtocolVersion,
    pub request_id: UuidV4,
    pub scope: PauseScope,
    pub device_id: Option<UuidV4>,
    pub paused: bool,
}
impl SetPauseStateRequestV1 {
    pub fn validate(&self) -> Result<(), DecodeError> {
        match (&self.scope, &self.device_id) {
            (PauseScope::Global, None) | (PauseScope::Device, Some(_)) => Ok(()),
            _ => Err(DecodeError::ProtocolSchemaInvalid),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlSuccessV1 {
    DeviceCreated {
        protocol_version: ProtocolVersion,
        request_id: UuidV4,
        device_id: UuidV4,
        credential: DeviceCredential,
        credential_generation: u64,
        device_state: DeviceState,
        created_at_ms: i64,
    },
    EnrollmentArtifactCreated {
        protocol_version: ProtocolVersion,
        request_id: UuidV4,
        device_id: UuidV4,
        enrollment_artifact: EnrollmentArtifact,
        expires_at_ms: i64,
        device_state: DeviceState,
    },
    DeviceEnrolled {
        protocol_version: ProtocolVersion,
        request_id: UuidV4,
        device_id: UuidV4,
        credential: DeviceCredential,
        credential_generation: u64,
        device_state: DeviceState,
        enrolled_at_ms: i64,
    },
    CredentialRotated {
        protocol_version: ProtocolVersion,
        request_id: UuidV4,
        device_id: UuidV4,
        credential: DeviceCredential,
        credential_generation: u64,
        rotated_at_ms: i64,
    },
    DeviceRevoked {
        protocol_version: ProtocolVersion,
        request_id: UuidV4,
        device_id: UuidV4,
        device_state: DeviceState,
        revoked_at_ms: i64,
    },
    PauseStateSet {
        protocol_version: ProtocolVersion,
        request_id: UuidV4,
        scope: PauseScope,
        device_id: Option<UuidV4>,
        paused: bool,
        changed_at_ms: i64,
    },
    HistoryPurged {
        protocol_version: ProtocolVersion,
        request_id: UuidV4,
        purge_id: UuidV4,
        history_epoch: UuidV4,
        purged_through_cursor: Option<U64Decimal>,
        purged_at_ms: i64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlError {
    Failure(FailureResponse),
    SecretResultAlreadyCommitted { resource_id: UuidV4 },
}
impl Serialize for ControlError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            code: &'a FailureCode,
            retryable: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            resource_id: Option<&'a UuidV4>,
        }
        match self {
            Self::Failure(failure) => Wire {
                code: failure.code(),
                retryable: failure.code().retryable(),
                resource_id: None,
            }
            .serialize(serializer),
            Self::SecretResultAlreadyCommitted { resource_id } => Wire {
                code: &FailureCode::SecretResultAlreadyCommitted,
                retryable: false,
                resource_id: Some(resource_id),
            }
            .serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for ControlError {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            code: FailureCode,
            retryable: bool,
            resource_id: Option<UuidV4>,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.retryable != wire.code.retryable() {
            return Err(de::Error::custom(
                "retryable must match the stable failure code",
            ));
        }
        match (wire.code, wire.resource_id) {
            (FailureCode::SecretResultAlreadyCommitted, Some(resource_id)) => {
                Ok(Self::SecretResultAlreadyCommitted { resource_id })
            }
            (FailureCode::SecretResultAlreadyCommitted, None) => Err(de::Error::custom(
                "secret_result_already_committed requires resource_id",
            )),
            (_, Some(_)) => Err(de::Error::custom(
                "resource_id is only allowed for secret_result_already_committed",
            )),
            (code, None) => Ok(Self::Failure(FailureResponse::new(code))),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlErrorResponseV1 {
    pub protocol_version: ProtocolVersion,
    pub error: ControlError,
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
    let is_integer = envelope.protocol_version.as_i64().is_some()
        || envelope.protocol_version.as_u64().is_some();
    if !is_integer {
        return Err(DecodeError::ProtocolSchemaInvalid);
    }
    if envelope.protocol_version.as_u64() != Some(PROTOCOL_VERSION.into()) {
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
        assert_eq!(*protocol_version, ProtocolVersion);
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
        for version in ["256", "-1", "18446744073709551615"] {
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

    #[test]
    fn fixed_protocol_fields_reject_invalid_degrees_of_freedom() {
        assert_matches!(serde_json::from_str::<FixedClockSkewMs>("0"), Err(_));
        assert_matches!(
            decode_server_message(
                r#"{"protocol_version":1,"type":"pause_notice","scope":"bananas","reason_code":"payload_empty"}"#
            ),
            Err(DecodeError::ProtocolSchemaInvalid)
        );
        assert_matches!(
            decode_server_message(
                r#"{"protocol_version":1,"type":"error","code":"payload_empty","retryable":true}"#
            ),
            Err(DecodeError::ProtocolSchemaInvalid)
        );
    }

    #[test]
    fn control_plane_schemas_are_closed_and_platform_specific() {
        let request = r#"{"protocol_version":1,"request_id":"00000000-0000-4000-8000-000000000003","display_name":"Synthetic desktop","platform":"linux_wayland"}"#;
        assert!(serde_json::from_str::<CreateManagedDeviceRequestV1>(request).is_ok());
        assert!(serde_json::from_str::<CreateEnrollmentArtifactRequestV1>(request).is_err());
        assert!(
            serde_json::from_str::<CreateManagedDeviceRequestV1>(&format!("{request} ")).is_ok()
        );
        let invalid_pause = r#"{"protocol_version":1,"request_id":"00000000-0000-4000-8000-000000000003","scope":"global","device_id":"00000000-0000-4000-8000-000000000004","paused":true}"#;
        assert_matches!(
            serde_json::from_str::<SetPauseStateRequestV1>(invalid_pause)
                .unwrap()
                .validate(),
            Err(DecodeError::ProtocolSchemaInvalid)
        );
    }
}
