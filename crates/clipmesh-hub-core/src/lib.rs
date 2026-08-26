//! Serialized hub state for ClipMesh protocol v1.
//!
//! This crate deliberately has no listener, HTTP, WebSocket, TLS, deployment,
//! or live-service integration. `HubCore` is the one mutation and recipient
//! seam for state changes, recipient selection, and queue cutoff. The TLS-owning
//! integration slice must extend this seam through its concrete frame handoff.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::Path,
    sync::Mutex,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use clipmesh_protocol::{
    AdministratorCredential, ClipboardEventV1, DeviceCredential, DeviceDisplayName,
    EnrollmentArtifact, FailureCode, Platform, ResumeStatus,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::{Uuid, Variant, Version};

const SCHEMA_VERSION: &str = "1";
const ENROLLMENT_LIFETIME_MS: i64 = 600_000;
const ENROLLMENT_TOMBSTONE_LIFETIME_MS: i64 = 86_400_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryMode {
    Sqlite,
    Memory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionLimits {
    pub retention_seconds: u64,
    pub history_max_entries: usize,
    pub max_payload_bytes: u32,
}

impl Default for RetentionLimits {
    fn default() -> Self {
        Self {
            retention_seconds: 14_400,
            history_max_entries: 20,
            max_payload_bytes: 262_144,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Authority {
    Device,
    Administrator,
    Enrollment,
}

pub enum PresentedCredential<'a> {
    Device(&'a DeviceCredential),
    Administrator(&'a AdministratorCredential),
    Enrollment(&'a EnrollmentArtifact),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevicePrincipal {
    pub device_id: Uuid,
    pub credential_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Principal {
    Device(DevicePrincipal),
    Administrator,
    Enrollment { device_id: Uuid },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceLifecycle {
    Pending,
    Active,
    Revoked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceRecord {
    pub device_id: Uuid,
    pub display_name: DeviceDisplayName,
    pub platform: Platform,
    pub state: DeviceLifecycle,
    pub credential_generation: Option<u64>,
    pub created_at_ms: i64,
    pub enrolled_at_ms: Option<i64>,
    pub revoked_at_ms: Option<i64>,
    pub paused: bool,
}

#[derive(Clone, Eq, PartialEq)]
pub struct RequestIdentity {
    request_id: Uuid,
    body_sha256: [u8; 32],
}

impl std::fmt::Debug for RequestIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestIdentity")
            .field("request_id", &self.request_id)
            .field("body_sha256", &"[REDACTED_REQUEST_BODY_HASH]")
            .finish()
    }
}

impl RequestIdentity {
    pub fn new(request_id: Uuid, body_sha256: [u8; 32]) -> Result<Self, CoreError> {
        if request_id.get_version() != Some(Version::Random)
            || request_id.get_variant() != Variant::RFC4122
        {
            return Err(CoreError::Failure(FailureCode::ProtocolSchemaInvalid));
        }
        Ok(Self {
            request_id,
            body_sha256,
        })
    }

    pub fn request_id(&self) -> Uuid {
        self.request_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedDevice {
    pub record: DeviceRecord,
    pub credential: DeviceCredential,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssuedEnrollment {
    pub record: DeviceRecord,
    pub artifact: EnrollmentArtifact,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnrolledDevice {
    pub device_id: Uuid,
    pub credential: DeviceCredential,
    pub enrolled_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RotatedCredential {
    pub device_id: Uuid,
    pub credential: DeviceCredential,
    pub credential_generation: u64,
    pub rotated_at_ms: i64,
    pub cut_off_sessions: Vec<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationResult {
    pub device_id: Option<Uuid>,
    pub changed_at_ms: i64,
    pub cut_off_sessions: Vec<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PurgeResult {
    pub purge_id: Uuid,
    pub history_epoch: Uuid,
    pub purged_through_cursor: Option<u64>,
    pub purged_at_ms: i64,
    pub cut_off_sessions: Vec<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AcceptedPublish {
    pub cursor: u64,
    pub expires_at_ms: i64,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedEvent {
    pub cursor: u64,
    pub accepted_at_ms: i64,
    pub source_display_name: DeviceDisplayName,
    pub event: ClipboardEventV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumePlan {
    pub history_epoch: Uuid,
    pub status: ResumeStatus,
    pub requested_after_cursor: Option<u64>,
    pub boundary_cursor: Option<u64>,
    pub lost_through_cursor: Option<u64>,
    pub events: Vec<RetainedEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueuedLiveEvent {
    history_epoch: Uuid,
    event: RetainedEvent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SessionOutput {
    PublishAccepted(AcceptedPublish),
    LiveEvent(QueuedLiveEvent),
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CoreError {
    #[error("hub state rejected the operation: {0:?}")]
    Failure(FailureCode),
    #[error("a secret result was already committed for resource {resource_id}")]
    SecretResultAlreadyCommitted { resource_id: Uuid },
    #[error("hub state storage is unavailable")]
    StorageUnavailable,
    #[error("database schema is unsupported")]
    DatabaseSchemaUnsupported,
    #[error("database integrity check failed")]
    DatabaseIntegrityFailed,
}

impl From<rusqlite::Error> for CoreError {
    fn from(_: rusqlite::Error) -> Self {
        Self::StorageUnavailable
    }
}

#[derive(Clone)]
struct StoredEvent {
    cursor: u64,
    accepted_at_ms: i64,
    source_display_name: DeviceDisplayName,
    event: ClipboardEventV1,
}

struct NewDevice<'a> {
    device_id: Uuid,
    display_name: &'a DeviceDisplayName,
    platform: &'a Platform,
    state: &'a str,
    credential_digest: Option<&'a [u8; 32]>,
    credential_generation: Option<u64>,
    created_at_ms: i64,
    enrolled_at_ms: Option<i64>,
}

struct NewReceipt<'a> {
    principal_class: &'a str,
    principal_digest: &'a [u8; 32],
    operation: &'a str,
    request: &'a RequestIdentity,
    result_code: &'a str,
    resource_id: Uuid,
    result_json: Option<&'a str>,
}

impl StoredEvent {
    fn retained(&self) -> RetainedEvent {
        RetainedEvent {
            cursor: self.cursor,
            accepted_at_ms: self.accepted_at_ms,
            source_display_name: self.source_display_name.clone(),
            event: self.event.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionPhase {
    AwaitResume,
    Buffering,
    Live,
}

struct Session {
    device_id: Uuid,
    credential_generation: u64,
    history_epoch: Uuid,
    phase: SessionPhase,
    queue: VecDeque<SessionOutput>,
}

struct State {
    connection: Connection,
    history_mode: HistoryMode,
    limits: RetentionLimits,
    history_epoch: Uuid,
    cursor_high_water: u64,
    lost_through_cursor: Option<u64>,
    ready: bool,
    memory_events: BTreeMap<u64, StoredEvent>,
    sessions: HashMap<Uuid, Session>,
}

pub struct HubCore {
    state: Mutex<State>,
}

impl HubCore {
    pub fn open(
        database_path: impl AsRef<Path>,
        history_mode: HistoryMode,
        limits: RetentionLimits,
        administrator_credential: &AdministratorCredential,
        now_ms: i64,
    ) -> Result<Self, CoreError> {
        if !(60..=604_800).contains(&limits.retention_seconds)
            || !(1..=1_000).contains(&limits.history_max_entries)
            || !(1..=1_048_576).contains(&limits.max_payload_bytes)
        {
            return Err(CoreError::Failure(FailureCode::ConfigValueInvalid));
        }
        let database_path = database_path.as_ref();
        let database_is_new = fs_path_is_new(database_path)?;
        let mut connection = Connection::open(database_path)?;
        initialize_schema(&mut connection, database_is_new)?;
        #[cfg(unix)]
        std::fs::set_permissions(
            database_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .map_err(|_| CoreError::StorageUnavailable)?;
        connection.pragma_update(None, "secure_delete", "ON")?;
        connection.pragma_update(None, "journal_mode", "DELETE")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        verify_integrity(&connection)?;

        let administrator_digest = digest(administrator_credential.wire_value().as_bytes());
        initialize_administrator(&connection, &administrator_digest, database_is_new)?;

        let prior_mode = metadata(&connection, "history_mode")?;
        let mut history_epoch = match metadata(&connection, "history_epoch")? {
            Some(value) => parse_uuid_v4(&value)?,
            None if database_is_new => Uuid::new_v4(),
            None => return Err(CoreError::DatabaseIntegrityFailed),
        };
        let cursor_high_water =
            match parse_optional_u64(metadata(&connection, "cursor_high_water")?)? {
                Some(value) => value,
                None if database_is_new => 0,
                None => return Err(CoreError::DatabaseIntegrityFailed),
            };
        let mut lost_through_cursor =
            parse_optional_u64(metadata(&connection, "lost_through_cursor")?)?;

        if !database_is_new && prior_mode.is_none() {
            return Err(CoreError::DatabaseIntegrityFailed);
        }
        if prior_mode
            .as_deref()
            .is_some_and(|mode| !matches!(mode, "sqlite" | "memory"))
        {
            return Err(CoreError::DatabaseIntegrityFailed);
        }
        if !database_is_new {
            match metadata(&connection, "global_pause")?.as_deref() {
                Some("0" | "1") => {}
                _ => return Err(CoreError::DatabaseIntegrityFailed),
            }
        }
        verify_persisted_state(
            &connection,
            cursor_high_water,
            lost_through_cursor,
            prior_mode.as_deref(),
        )?;
        if !database_is_new
            && (history_mode == HistoryMode::Memory || prior_mode.as_deref() == Some("memory"))
        {
            history_epoch = Uuid::new_v4();
            lost_through_cursor = nonzero(cursor_high_water);
        }

        set_metadata(&connection, "schema_version", SCHEMA_VERSION)?;
        set_metadata(
            &connection,
            "history_mode",
            match history_mode {
                HistoryMode::Sqlite => "sqlite",
                HistoryMode::Memory => "memory",
            },
        )?;
        set_metadata(&connection, "history_epoch", &history_epoch.to_string())?;
        set_metadata(
            &connection,
            "cursor_high_water",
            &cursor_high_water.to_string(),
        )?;
        set_optional_u64_metadata(&connection, "lost_through_cursor", lost_through_cursor)?;
        if database_is_new {
            set_metadata(&connection, "global_pause", "0")?;
        }

        if history_mode == HistoryMode::Memory && prior_mode.as_deref() == Some("sqlite") {
            connection.execute("DELETE FROM events", [])?;
        }

        let core = Self {
            state: Mutex::new(State {
                connection,
                history_mode,
                limits,
                history_epoch,
                cursor_high_water,
                lost_through_cursor,
                ready: true,
                memory_events: BTreeMap::new(),
                sessions: HashMap::new(),
            }),
        };
        core.expire_and_trim(now_ms)?;
        Ok(core)
    }

    pub fn authorize(
        &self,
        presented: PresentedCredential<'_>,
        required: Authority,
        now_ms: i64,
    ) -> Result<Principal, CoreError> {
        let state = self.state.lock().expect("hub state lock poisoned");
        match (presented, required) {
            (PresentedCredential::Administrator(credential), Authority::Administrator) => {
                let value = digest(credential.wire_value().as_bytes());
                let exists = state
                    .connection
                    .query_row(
                        "SELECT 1 FROM administrators WHERE credential_digest = ?1",
                        params![value.as_slice()],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                exists
                    .then_some(Principal::Administrator)
                    .ok_or(CoreError::Failure(FailureCode::Unauthorized))
            }
            (PresentedCredential::Device(credential), Authority::Device) => {
                authenticate_device(&state.connection, credential).map(Principal::Device)
            }
            (PresentedCredential::Enrollment(artifact), Authority::Enrollment) => {
                authenticate_enrollment(&state.connection, artifact, now_ms)
            }
            _ => Err(CoreError::Failure(FailureCode::Unauthorized)),
        }
    }

    pub fn create_managed_device(
        &self,
        administrator: &AdministratorCredential,
        request: RequestIdentity,
        display_name: DeviceDisplayName,
        platform: Platform,
        now_ms: i64,
    ) -> Result<CreatedDevice, CoreError> {
        require_platform(&platform, true)?;
        let mut state = self.state.lock().expect("hub state lock poisoned");
        require_administrator(&state.connection, administrator)?;
        if let Some(resource_id) = check_receipt(
            &state.connection,
            "administrator",
            &digest(administrator.wire_value().as_bytes()),
            "create_managed_device",
            &request,
        )? {
            return Err(CoreError::SecretResultAlreadyCommitted { resource_id });
        }
        let device_id = Uuid::new_v4();
        let credential = generate_device_credential()?;
        let credential_digest = digest(credential.wire_value().as_bytes());
        let tx = state.connection.transaction()?;
        insert_device(
            &tx,
            NewDevice {
                device_id,
                display_name: &display_name,
                platform: &platform,
                state: "active",
                credential_digest: Some(&credential_digest),
                credential_generation: Some(1),
                created_at_ms: now_ms,
                enrolled_at_ms: Some(now_ms),
            },
        )?;
        let receipt_digest = digest(administrator.wire_value().as_bytes());
        insert_receipt(
            &tx,
            NewReceipt {
                principal_class: "administrator",
                principal_digest: &receipt_digest,
                operation: "create_managed_device",
                request: &request,
                result_code: "secret_committed",
                resource_id: device_id,
                result_json: None,
            },
        )?;
        tx.commit()?;
        Ok(CreatedDevice {
            record: load_device(&state.connection, device_id)?,
            credential,
        })
    }

    pub fn issue_enrollment_artifact(
        &self,
        administrator: &AdministratorCredential,
        request: RequestIdentity,
        display_name: DeviceDisplayName,
        platform: Platform,
        now_ms: i64,
    ) -> Result<IssuedEnrollment, CoreError> {
        require_platform(&platform, false)?;
        let mut state = self.state.lock().expect("hub state lock poisoned");
        require_administrator(&state.connection, administrator)?;
        cleanup_enrollments(&mut state.connection, now_ms)?;
        let administrator_digest = digest(administrator.wire_value().as_bytes());
        if let Some(resource_id) = check_receipt(
            &state.connection,
            "administrator",
            &administrator_digest,
            "issue_enrollment_artifact",
            &request,
        )? {
            return Err(CoreError::SecretResultAlreadyCommitted { resource_id });
        }
        let device_id = Uuid::new_v4();
        let artifact = generate_enrollment_artifact()?;
        let artifact_digest = digest(artifact.wire_value().as_bytes());
        let expires_at_ms = now_ms.saturating_add(ENROLLMENT_LIFETIME_MS);
        let tx = state.connection.transaction()?;
        insert_device(
            &tx,
            NewDevice {
                device_id,
                display_name: &display_name,
                platform: &platform,
                state: "pending",
                credential_digest: None,
                credential_generation: None,
                created_at_ms: now_ms,
                enrolled_at_ms: None,
            },
        )?;
        tx.execute(
            "INSERT INTO enrollment_artifacts
             (credential_digest, device_id, expires_at_ms, state, tombstone_expires_at_ms)
             VALUES (?1, ?2, ?3, 'active', NULL)",
            params![
                artifact_digest.as_slice(),
                device_id.to_string(),
                expires_at_ms
            ],
        )?;
        insert_receipt(
            &tx,
            NewReceipt {
                principal_class: "administrator",
                principal_digest: &administrator_digest,
                operation: "issue_enrollment_artifact",
                request: &request,
                result_code: "secret_committed",
                resource_id: device_id,
                result_json: None,
            },
        )?;
        tx.commit()?;
        Ok(IssuedEnrollment {
            record: load_device(&state.connection, device_id)?,
            artifact,
            expires_at_ms,
        })
    }

    pub fn exchange_enrollment(
        &self,
        artifact: &EnrollmentArtifact,
        request: RequestIdentity,
        now_ms: i64,
    ) -> Result<EnrolledDevice, CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        cleanup_enrollments(&mut state.connection, now_ms)?;
        let artifact_digest = digest(artifact.wire_value().as_bytes());
        let artifact_row = state
            .connection
            .query_row(
                "SELECT device_id, state, expires_at_ms FROM enrollment_artifacts
                 WHERE credential_digest = ?1",
                params![artifact_digest.as_slice()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((device_id, artifact_state, expires_at_ms)) = artifact_row else {
            return Err(CoreError::Failure(FailureCode::Unauthorized));
        };
        if artifact_state == "consumed" {
            return match check_receipt(
                &state.connection,
                "enrollment",
                &artifact_digest,
                "exchange_enrollment",
                &request,
            )? {
                Some(resource_id) => Err(CoreError::SecretResultAlreadyCommitted { resource_id }),
                None => Err(CoreError::Failure(FailureCode::EnrollmentArtifactInvalid)),
            };
        }
        if artifact_state != "active" || expires_at_ms <= now_ms {
            return Err(CoreError::Failure(FailureCode::EnrollmentArtifactInvalid));
        }
        let device_id = parse_uuid(&device_id)?;
        let credential = generate_device_credential()?;
        let credential_digest = digest(credential.wire_value().as_bytes());
        let tx = state.connection.transaction()?;
        tx.execute(
            "UPDATE devices SET state = 'active', credential_digest = ?1,
             credential_generation = '1', enrolled_at_ms = ?2 WHERE device_id = ?3 AND state = 'pending'",
            params![credential_digest.as_slice(), now_ms, device_id.to_string()],
        )?;
        tx.execute(
            "UPDATE enrollment_artifacts SET state = 'consumed', tombstone_expires_at_ms = ?1
             WHERE credential_digest = ?2 AND state = 'active'",
            params![
                now_ms.saturating_add(ENROLLMENT_TOMBSTONE_LIFETIME_MS),
                artifact_digest.as_slice()
            ],
        )?;
        insert_receipt(
            &tx,
            NewReceipt {
                principal_class: "enrollment",
                principal_digest: &artifact_digest,
                operation: "exchange_enrollment",
                request: &request,
                result_code: "secret_committed",
                resource_id: device_id,
                result_json: None,
            },
        )?;
        tx.commit()?;
        Ok(EnrolledDevice {
            device_id,
            credential,
            enrolled_at_ms: now_ms,
        })
    }

    pub fn open_session(
        &self,
        credential: &DeviceCredential,
    ) -> Result<(Uuid, DevicePrincipal), CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        let principal = authenticate_device(&state.connection, credential)?;
        if global_pause(&state.connection)? || device_pause(&state.connection, principal.device_id)?
        {
            return Err(CoreError::Failure(FailureCode::AdministrativelyPaused));
        }
        let session_id = Uuid::new_v4();
        let history_epoch = state.history_epoch;
        state.sessions.insert(
            session_id,
            Session {
                device_id: principal.device_id,
                credential_generation: principal.credential_generation,
                history_epoch,
                phase: SessionPhase::AwaitResume,
                queue: VecDeque::new(),
            },
        );
        Ok((session_id, principal))
    }

    pub fn begin_resume(
        &self,
        session_id: Uuid,
        known_history_epoch: Option<Uuid>,
        after_cursor: Option<u64>,
        now_ms: i64,
    ) -> Result<ResumePlan, CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        expire_and_trim_locked(&mut state, now_ms)?;
        let history_epoch = state.history_epoch;
        let boundary_cursor = nonzero(state.cursor_high_water);
        let lost_through_cursor = state.lost_through_cursor;
        let epoch_changed = known_history_epoch.is_some_and(|epoch| epoch != history_epoch);
        if known_history_epoch.is_none() && after_cursor.is_some() {
            return Err(CoreError::Failure(FailureCode::ResumeCursorWithoutEpoch));
        }
        if after_cursor == Some(0) {
            return Err(CoreError::Failure(FailureCode::ProtocolSchemaInvalid));
        }
        if !epoch_changed
            && after_cursor.is_some_and(|cursor| boundary_cursor.is_none_or(|b| cursor > b))
        {
            return Err(CoreError::Failure(FailureCode::CursorAhead));
        }
        let effective_after = if epoch_changed { None } else { after_cursor };
        let status = if epoch_changed {
            ResumeStatus::EpochChanged
        } else if after_cursor.is_none() {
            ResumeStatus::Fresh
        } else if after_cursor
            .zip(lost_through_cursor)
            .is_some_and(|(after, lost)| after < lost)
        {
            ResumeStatus::Gap
        } else {
            ResumeStatus::Complete
        };
        let events = retained_events(&state)?
            .into_iter()
            .filter(|event| {
                effective_after.is_none_or(|after| event.cursor > after)
                    && boundary_cursor.is_some_and(|boundary| event.cursor <= boundary)
                    && event.event.expires_at_ms > now_ms
            })
            .map(|event| event.retained())
            .collect();
        let session = state
            .sessions
            .get_mut(&session_id)
            .ok_or(CoreError::Failure(FailureCode::Unauthorized))?;
        if session.phase != SessionPhase::AwaitResume || session.history_epoch != history_epoch {
            return Err(CoreError::Failure(FailureCode::SessionEpochStale));
        }
        session.phase = SessionPhase::Buffering;
        Ok(ResumePlan {
            history_epoch,
            status,
            requested_after_cursor: after_cursor,
            boundary_cursor,
            lost_through_cursor,
            events,
        })
    }

    pub fn complete_resume(&self, session_id: Uuid) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        let history_epoch = state.history_epoch;
        let session = state
            .sessions
            .get_mut(&session_id)
            .ok_or(CoreError::Failure(FailureCode::Unauthorized))?;
        if session.history_epoch != history_epoch {
            return Err(CoreError::Failure(FailureCode::SessionEpochStale));
        }
        if session.phase != SessionPhase::Buffering {
            return Err(CoreError::Failure(FailureCode::ProtocolSchemaInvalid));
        }
        session.phase = SessionPhase::Live;
        Ok(())
    }

    pub fn publish(
        &self,
        session_id: Uuid,
        event: ClipboardEventV1,
        now_ms: i64,
    ) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        let (device_id, credential_generation, session_epoch) = {
            let session = state
                .sessions
                .get(&session_id)
                .ok_or(CoreError::Failure(FailureCode::Unauthorized))?;
            if session.phase != SessionPhase::Live {
                return Err(CoreError::Failure(FailureCode::ResumeRequired));
            }
            (
                session.device_id,
                session.credential_generation,
                session.history_epoch,
            )
        };
        if event.source_device_id.get() != device_id {
            return Err(CoreError::Failure(FailureCode::SourceDeviceMismatch));
        }
        event
            .validate(
                now_ms,
                state.limits.max_payload_bytes,
                state.limits.retention_seconds,
            )
            .map_err(CoreError::Failure)?;
        recheck_publish_authority(&state, device_id, credential_generation, session_epoch)?;

        let message_id = event.message_id.get();
        if let Some(retained) = find_retained_event(&state, message_id)? {
            return if retained.event == event {
                enqueue_publish_accepted(
                    &mut state,
                    session_id,
                    AcceptedPublish {
                        cursor: retained.cursor,
                        expires_at_ms: retained.event.expires_at_ms,
                        duplicate: true,
                    },
                )?;
                Ok(())
            } else {
                Err(CoreError::Failure(FailureCode::MessageIdConflict))
            };
        }
        if tombstone(&state.connection, message_id)?.is_some() {
            return Err(CoreError::Failure(FailureCode::MessageIdReplay));
        }
        let source_seq = event.source_seq.get();
        if source_high_water(&state.connection, device_id)?.is_some_and(|value| source_seq <= value)
        {
            return Err(CoreError::Failure(FailureCode::SourceSequenceReplay));
        }
        let Some(cursor) = state.cursor_high_water.checked_add(1) else {
            state.ready = false;
            return Err(CoreError::Failure(FailureCode::HubCursorExhausted));
        };
        let source_display_name = load_device(&state.connection, device_id)?.display_name;
        let stored = StoredEvent {
            cursor,
            accepted_at_ms: now_ms,
            source_display_name,
            event,
        };
        persist_publish_with_retention(&mut state, &stored, now_ms)?;
        enqueue_accepted(&mut state, session_id, &stored)?;
        Ok(())
    }

    pub fn rotate_credential(
        &self,
        administrator: &AdministratorCredential,
        request: RequestIdentity,
        device_id: Uuid,
        now_ms: i64,
    ) -> Result<RotatedCredential, CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        require_administrator(&state.connection, administrator)?;
        let administrator_digest = digest(administrator.wire_value().as_bytes());
        if let Some(resource_id) = check_receipt(
            &state.connection,
            "administrator",
            &administrator_digest,
            "rotate_credential",
            &request,
        )? {
            return Err(CoreError::SecretResultAlreadyCommitted { resource_id });
        }
        let current = load_device(&state.connection, device_id)?;
        if current.state != DeviceLifecycle::Active {
            return Err(CoreError::Failure(FailureCode::Unauthorized));
        }
        let generation = current
            .credential_generation
            .and_then(|value| value.checked_add(1))
            .ok_or(CoreError::Failure(FailureCode::StorageUnavailable))?;
        let credential = generate_device_credential()?;
        let credential_digest = digest(credential.wire_value().as_bytes());
        let tx = state.connection.transaction()?;
        tx.execute(
            "UPDATE devices SET credential_digest = ?1, credential_generation = ?2 WHERE device_id = ?3",
            params![credential_digest.as_slice(), generation.to_string(), device_id.to_string()],
        )?;
        insert_receipt(
            &tx,
            NewReceipt {
                principal_class: "administrator",
                principal_digest: &administrator_digest,
                operation: "rotate_credential",
                request: &request,
                result_code: "secret_committed",
                resource_id: device_id,
                result_json: None,
            },
        )?;
        tx.commit()?;
        let cut_off_sessions = cutoff_device_sessions(&mut state, device_id);
        Ok(RotatedCredential {
            device_id,
            credential,
            credential_generation: generation,
            rotated_at_ms: now_ms,
            cut_off_sessions,
        })
    }

    pub fn revoke_device(
        &self,
        administrator: &AdministratorCredential,
        request: RequestIdentity,
        device_id: Uuid,
        now_ms: i64,
    ) -> Result<MutationResult, CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        require_administrator(&state.connection, administrator)?;
        let administrator_digest = digest(administrator.wire_value().as_bytes());
        if let Some(result) = replay_nonsecret_receipt(
            &state.connection,
            &administrator_digest,
            "revoke_device",
            &request,
        )? {
            return Ok(result);
        }
        let record = load_device(&state.connection, device_id)?;
        if record.state != DeviceLifecycle::Active {
            return Err(CoreError::Failure(FailureCode::Unauthorized));
        }
        let tx = state.connection.transaction()?;
        tx.execute(
            "UPDATE devices SET state = 'revoked', credential_digest = NULL, revoked_at_ms = ?1
             WHERE device_id = ?2 AND state = 'active'",
            params![now_ms, device_id.to_string()],
        )?;
        let result = MutationResult {
            device_id: Some(device_id),
            changed_at_ms: now_ms,
            cut_off_sessions: Vec::new(),
        };
        let result_json = serialize_mutation_result(&result)?;
        insert_receipt(
            &tx,
            NewReceipt {
                principal_class: "administrator",
                principal_digest: &administrator_digest,
                operation: "revoke_device",
                request: &request,
                result_code: "device_revoked",
                resource_id: device_id,
                result_json: Some(&result_json),
            },
        )?;
        tx.commit()?;
        let cut_off_sessions = cutoff_device_sessions(&mut state, device_id);
        Ok(MutationResult {
            cut_off_sessions,
            ..result
        })
    }

    pub fn set_pause(
        &self,
        administrator: &AdministratorCredential,
        request: RequestIdentity,
        device_id: Option<Uuid>,
        paused: bool,
        now_ms: i64,
    ) -> Result<MutationResult, CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        require_administrator(&state.connection, administrator)?;
        let administrator_digest = digest(administrator.wire_value().as_bytes());
        if let Some(result) = replay_nonsecret_receipt(
            &state.connection,
            &administrator_digest,
            "set_pause",
            &request,
        )? {
            return Ok(result);
        }
        if let Some(device_id) = device_id {
            load_device(&state.connection, device_id)?;
        }
        let tx = state.connection.transaction()?;
        match device_id {
            Some(device_id) => {
                tx.execute(
                    "UPDATE devices SET paused = ?1 WHERE device_id = ?2",
                    params![paused, device_id.to_string()],
                )?;
            }
            None => set_metadata_tx(&tx, "global_pause", if paused { "1" } else { "0" })?,
        }
        let result = MutationResult {
            device_id,
            changed_at_ms: now_ms,
            cut_off_sessions: Vec::new(),
        };
        let resource_id = device_id.unwrap_or(Uuid::nil());
        let result_json = serialize_mutation_result(&result)?;
        insert_receipt(
            &tx,
            NewReceipt {
                principal_class: "administrator",
                principal_digest: &administrator_digest,
                operation: "set_pause",
                request: &request,
                result_code: "pause_state_set",
                resource_id,
                result_json: Some(&result_json),
            },
        )?;
        tx.commit()?;
        let cut_off_sessions = if paused {
            match device_id {
                Some(device_id) => cutoff_device_sessions(&mut state, device_id),
                None => cutoff_all_sessions(&mut state),
            }
        } else {
            Vec::new()
        };
        Ok(MutationResult {
            cut_off_sessions,
            ..result
        })
    }

    pub fn purge(
        &self,
        administrator: &AdministratorCredential,
        request: RequestIdentity,
        now_ms: i64,
    ) -> Result<PurgeResult, CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        require_administrator(&state.connection, administrator)?;
        let administrator_digest = digest(administrator.wire_value().as_bytes());
        if let Some(result) =
            replay_purge_receipt(&state.connection, &administrator_digest, &request)?
        {
            return Ok(result);
        }
        let purge_id = Uuid::new_v4();
        let history_epoch = Uuid::new_v4();
        let purged_through_cursor = nonzero(state.cursor_high_water);
        let result = PurgeResult {
            purge_id,
            history_epoch,
            purged_through_cursor,
            purged_at_ms: now_ms,
            cut_off_sessions: Vec::new(),
        };
        let result_json = serialize_purge_result(&result)?;
        let tx = state.connection.transaction()?;
        tx.execute("DELETE FROM events", [])?;
        set_metadata_tx(&tx, "history_epoch", &history_epoch.to_string())?;
        set_optional_u64_metadata_tx(&tx, "lost_through_cursor", purged_through_cursor)?;
        insert_receipt(
            &tx,
            NewReceipt {
                principal_class: "administrator",
                principal_digest: &administrator_digest,
                operation: "purge",
                request: &request,
                result_code: "history_purged",
                resource_id: purge_id,
                result_json: Some(&result_json),
            },
        )?;
        tx.commit()?;
        state.memory_events.clear();
        state.history_epoch = history_epoch;
        state.lost_through_cursor = purged_through_cursor;
        let cut_off_sessions = cutoff_all_sessions(&mut state);
        Ok(PurgeResult {
            cut_off_sessions,
            ..result
        })
    }

    pub fn history(&self, now_ms: i64) -> Result<Vec<RetainedEvent>, CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        expire_and_trim_locked(&mut state, now_ms)?;
        Ok(retained_events(&state)?
            .into_iter()
            .filter(|event| event.event.expires_at_ms > now_ms)
            .map(|event| event.retained())
            .collect())
    }

    pub fn device(&self, device_id: Uuid) -> Result<DeviceRecord, CoreError> {
        let state = self.state.lock().expect("hub state lock poisoned");
        load_device(&state.connection, device_id)
    }

    pub fn history_epoch(&self) -> Uuid {
        self.state
            .lock()
            .expect("hub state lock poisoned")
            .history_epoch
    }

    pub fn cursor_high_water(&self) -> u64 {
        self.state
            .lock()
            .expect("hub state lock poisoned")
            .cursor_high_water
    }

    pub fn lost_through_cursor(&self) -> Option<u64> {
        self.state
            .lock()
            .expect("hub state lock poisoned")
            .lost_through_cursor
    }

    pub fn is_ready(&self) -> bool {
        self.state.lock().expect("hub state lock poisoned").ready
    }

    pub fn expire_and_trim(&self, now_ms: i64) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        expire_and_trim_locked(&mut state, now_ms)
    }

    pub fn cleanup_expired_enrollments(&self, now_ms: i64) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        cleanup_enrollments(&mut state.connection, now_ms)
    }

    #[cfg(test)]
    fn set_cursor_high_water_for_test(&self, cursor: u64) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        state.cursor_high_water = cursor;
        set_metadata(&state.connection, "cursor_high_water", &cursor.to_string())
    }
}

fn fs_path_is_new(path: &Path) -> Result<bool, CoreError> {
    match std::fs::metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err(CoreError::StorageUnavailable),
    }
}

fn initialize_schema(connection: &mut Connection, database_is_new: bool) -> Result<(), CoreError> {
    if !database_is_new {
        let version = connection
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| CoreError::DatabaseSchemaUnsupported)?;
        return if version == SCHEMA_VERSION {
            Ok(())
        } else {
            Err(CoreError::DatabaseSchemaUnsupported)
        };
    }
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS metadata (
           key TEXT PRIMARY KEY,
           value TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS administrators (
           credential_digest BLOB PRIMARY KEY CHECK(length(credential_digest) = 32)
         );
         CREATE TABLE IF NOT EXISTS devices (
           device_id TEXT PRIMARY KEY,
           display_name_json TEXT NOT NULL,
           platform_json TEXT NOT NULL,
           state TEXT NOT NULL CHECK(state IN ('pending','active','revoked')),
           credential_digest BLOB UNIQUE,
           credential_generation TEXT,
           created_at_ms INTEGER NOT NULL,
           enrolled_at_ms INTEGER,
           revoked_at_ms INTEGER,
           paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0,1))
         );
         CREATE TABLE IF NOT EXISTS enrollment_artifacts (
           credential_digest BLOB PRIMARY KEY CHECK(length(credential_digest) = 32),
           device_id TEXT NOT NULL,
           expires_at_ms INTEGER NOT NULL,
           state TEXT NOT NULL CHECK(state IN ('active','consumed','expired')),
           tombstone_expires_at_ms INTEGER
         );
         CREATE TABLE IF NOT EXISTS request_receipts (
           principal_class TEXT NOT NULL,
           principal_digest BLOB NOT NULL,
           operation TEXT NOT NULL,
           request_id TEXT NOT NULL,
           body_sha256 BLOB NOT NULL CHECK(length(body_sha256) = 32),
           result_code TEXT NOT NULL,
           resource_id TEXT NOT NULL,
           result_json TEXT,
           PRIMARY KEY(principal_class, principal_digest, operation, request_id)
         );
         CREATE TABLE IF NOT EXISTS source_watermarks (
           device_id TEXT PRIMARY KEY,
           source_seq TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS message_tombstones (
           message_id TEXT PRIMARY KEY,
           source_device_id TEXT NOT NULL,
           source_seq TEXT NOT NULL,
           accepted_cursor TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS events (
           cursor TEXT PRIMARY KEY,
           message_id TEXT NOT NULL UNIQUE,
           accepted_at_ms INTEGER NOT NULL,
           expires_at_ms INTEGER NOT NULL,
           source_display_name_json TEXT NOT NULL,
           event_json TEXT NOT NULL
         );
         INSERT INTO metadata (key, value) VALUES ('schema_version', '1');
         COMMIT;",
    )?;
    Ok(())
}

fn verify_integrity(connection: &Connection) -> Result<(), CoreError> {
    let result: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if result == "ok" {
        Ok(())
    } else {
        Err(CoreError::DatabaseIntegrityFailed)
    }
}

fn verify_persisted_state(
    connection: &Connection,
    cursor_high_water: u64,
    lost_through_cursor: Option<u64>,
    prior_mode: Option<&str>,
) -> Result<(), CoreError> {
    verify_persisted_state_inner(
        connection,
        cursor_high_water,
        lost_through_cursor,
        prior_mode,
    )
    .map_err(|_| CoreError::DatabaseIntegrityFailed)
}

fn verify_persisted_state_inner(
    connection: &Connection,
    cursor_high_water: u64,
    lost_through_cursor: Option<u64>,
    prior_mode: Option<&str>,
) -> Result<(), CoreError> {
    if lost_through_cursor.is_some_and(|cursor| cursor == 0 || cursor > cursor_high_water) {
        return Err(CoreError::DatabaseIntegrityFailed);
    }
    let orphaned_events: u64 = connection.query_row(
        "SELECT count(*) FROM events AS event
         LEFT JOIN message_tombstones AS tombstone
           ON tombstone.message_id = event.message_id
          AND tombstone.accepted_cursor = event.cursor
         WHERE tombstone.message_id IS NULL",
        [],
        |row| row.get(0),
    )?;
    if orphaned_events != 0 {
        return Err(CoreError::DatabaseIntegrityFailed);
    }
    if prior_mode == Some("memory") {
        let persisted_payloads: u64 =
            connection.query_row("SELECT count(*) FROM events", [], |row| row.get(0))?;
        if persisted_payloads != 0 {
            return Err(CoreError::DatabaseIntegrityFailed);
        }
    }
    for query in [
        "SELECT cursor FROM events",
        "SELECT accepted_cursor FROM message_tombstones",
    ] {
        let mut statement = connection.prepare(query)?;
        let values = statement.query_map([], |row| row.get::<_, String>(0))?;
        for value in values {
            let cursor: u64 = value?
                .parse()
                .map_err(|_| CoreError::DatabaseIntegrityFailed)?;
            if cursor == 0 || cursor > cursor_high_water {
                return Err(CoreError::DatabaseIntegrityFailed);
            }
        }
    }
    for query in [
        "SELECT source_seq FROM source_watermarks",
        "SELECT source_seq FROM message_tombstones",
        "SELECT credential_generation FROM devices WHERE credential_generation IS NOT NULL",
    ] {
        let mut statement = connection.prepare(query)?;
        let values = statement.query_map([], |row| row.get::<_, String>(0))?;
        for value in values {
            if value?.parse::<u64>().ok().is_none_or(|value| value == 0) {
                return Err(CoreError::DatabaseIntegrityFailed);
            }
        }
    }
    Ok(())
}

fn initialize_administrator(
    connection: &Connection,
    administrator_digest: &[u8; 32],
    database_is_new: bool,
) -> Result<(), CoreError> {
    let existing: Option<Vec<u8>> = connection
        .query_row(
            "SELECT credential_digest FROM administrators LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    match existing {
        None if database_is_new => {
            connection.execute(
                "INSERT INTO administrators (credential_digest) VALUES (?1)",
                params![administrator_digest.as_slice()],
            )?;
            Ok(())
        }
        Some(existing) if existing == administrator_digest => Ok(()),
        Some(_) | None => Err(CoreError::DatabaseIntegrityFailed),
    }
}

fn metadata(connection: &Connection, key: &str) -> Result<Option<String>, CoreError> {
    Ok(connection
        .query_row(
            "SELECT value FROM metadata WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?)
}

fn set_metadata(connection: &Connection, key: &str, value: &str) -> Result<(), CoreError> {
    connection.execute(
        "INSERT INTO metadata (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn set_metadata_tx(tx: &Transaction<'_>, key: &str, value: &str) -> Result<(), CoreError> {
    tx.execute(
        "INSERT INTO metadata (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn set_optional_u64_metadata(
    connection: &Connection,
    key: &str,
    value: Option<u64>,
) -> Result<(), CoreError> {
    match value {
        Some(value) => set_metadata(connection, key, &value.to_string()),
        None => {
            connection.execute("DELETE FROM metadata WHERE key = ?1", params![key])?;
            Ok(())
        }
    }
}

fn set_optional_u64_metadata_tx(
    tx: &Transaction<'_>,
    key: &str,
    value: Option<u64>,
) -> Result<(), CoreError> {
    match value {
        Some(value) => set_metadata_tx(tx, key, &value.to_string()),
        None => {
            tx.execute("DELETE FROM metadata WHERE key = ?1", params![key])?;
            Ok(())
        }
    }
}

fn parse_optional_u64(value: Option<String>) -> Result<Option<u64>, CoreError> {
    value
        .map(|value| {
            value
                .parse()
                .map_err(|_| CoreError::DatabaseIntegrityFailed)
        })
        .transpose()
}

fn nonzero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn random_wire(prefix: &str) -> Result<String, CoreError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| CoreError::StorageUnavailable)?;
    Ok(format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

fn generate_device_credential() -> Result<DeviceCredential, CoreError> {
    DeviceCredential::from_wire(&random_wire("cm_dev_v1_")?)
        .map_err(|_| CoreError::DatabaseIntegrityFailed)
}

fn generate_enrollment_artifact() -> Result<EnrollmentArtifact, CoreError> {
    EnrollmentArtifact::from_wire(&random_wire("cm_enroll_v1_")?)
        .map_err(|_| CoreError::DatabaseIntegrityFailed)
}

fn require_platform(platform: &Platform, managed: bool) -> Result<(), CoreError> {
    let allowed = if managed {
        matches!(platform, Platform::LinuxWayland | Platform::Macos)
    } else {
        matches!(platform, Platform::Ios | Platform::Ipados)
    };
    allowed
        .then_some(())
        .ok_or(CoreError::Failure(FailureCode::ProtocolSchemaInvalid))
}

fn require_administrator(
    connection: &Connection,
    credential: &AdministratorCredential,
) -> Result<(), CoreError> {
    let credential_digest = digest(credential.wire_value().as_bytes());
    let exists = connection
        .query_row(
            "SELECT 1 FROM administrators WHERE credential_digest = ?1",
            params![credential_digest.as_slice()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    exists
        .then_some(())
        .ok_or(CoreError::Failure(FailureCode::Unauthorized))
}

fn authenticate_device(
    connection: &Connection,
    credential: &DeviceCredential,
) -> Result<DevicePrincipal, CoreError> {
    let credential_digest = digest(credential.wire_value().as_bytes());
    let row = connection
        .query_row(
            "SELECT device_id, credential_generation, state FROM devices
             WHERE credential_digest = ?1",
            params![credential_digest.as_slice()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((device_id, generation, lifecycle)) = row else {
        return Err(CoreError::Failure(FailureCode::Unauthorized));
    };
    if lifecycle != "active" {
        return Err(CoreError::Failure(FailureCode::Unauthorized));
    }
    Ok(DevicePrincipal {
        device_id: parse_uuid(&device_id)?,
        credential_generation: generation
            .ok_or(CoreError::DatabaseIntegrityFailed)?
            .parse()
            .map_err(|_| CoreError::DatabaseIntegrityFailed)?,
    })
}

fn authenticate_enrollment(
    connection: &Connection,
    artifact: &EnrollmentArtifact,
    now_ms: i64,
) -> Result<Principal, CoreError> {
    let artifact_digest = digest(artifact.wire_value().as_bytes());
    let device_id = connection
        .query_row(
            "SELECT device_id FROM enrollment_artifacts
             WHERE credential_digest = ?1 AND state = 'active' AND expires_at_ms > ?2",
            params![artifact_digest.as_slice(), now_ms],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match device_id {
        Some(device_id) => Ok(Principal::Enrollment {
            device_id: parse_uuid(&device_id)?,
        }),
        None => Err(CoreError::Failure(FailureCode::Unauthorized)),
    }
}

fn insert_device(tx: &Transaction<'_>, device: NewDevice<'_>) -> Result<(), CoreError> {
    let display_name_json = serde_json::to_string(device.display_name)
        .map_err(|_| CoreError::DatabaseIntegrityFailed)?;
    let platform_json =
        serde_json::to_string(device.platform).map_err(|_| CoreError::DatabaseIntegrityFailed)?;
    tx.execute(
        "INSERT INTO devices
         (device_id, display_name_json, platform_json, state, credential_digest,
          credential_generation, created_at_ms, enrolled_at_ms, paused)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
        params![
            device.device_id.to_string(),
            display_name_json,
            platform_json,
            device.state,
            device.credential_digest.map(|value| value.as_slice()),
            device.credential_generation.map(|value| value.to_string()),
            device.created_at_ms,
            device.enrolled_at_ms,
        ],
    )?;
    Ok(())
}

fn load_device(connection: &Connection, device_id: Uuid) -> Result<DeviceRecord, CoreError> {
    let row = connection
        .query_row(
            "SELECT display_name_json, platform_json, state, credential_generation,
                    created_at_ms, enrolled_at_ms, revoked_at_ms, paused
             FROM devices WHERE device_id = ?1",
            params![device_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, bool>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((display_name, platform, state, generation, created, enrolled, revoked, paused)) = row
    else {
        return Err(CoreError::Failure(FailureCode::Unauthorized));
    };
    Ok(DeviceRecord {
        device_id,
        display_name: serde_json::from_str(&display_name)
            .map_err(|_| CoreError::DatabaseIntegrityFailed)?,
        platform: serde_json::from_str(&platform)
            .map_err(|_| CoreError::DatabaseIntegrityFailed)?,
        state: match state.as_str() {
            "pending" => DeviceLifecycle::Pending,
            "active" => DeviceLifecycle::Active,
            "revoked" => DeviceLifecycle::Revoked,
            _ => return Err(CoreError::DatabaseIntegrityFailed),
        },
        credential_generation: generation
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| CoreError::DatabaseIntegrityFailed)
            })
            .transpose()?,
        created_at_ms: created,
        enrolled_at_ms: enrolled,
        revoked_at_ms: revoked,
        paused,
    })
}

fn cleanup_enrollments(connection: &mut Connection, now_ms: i64) -> Result<(), CoreError> {
    let tx = connection.transaction()?;
    let mut expired = Vec::new();
    {
        let mut statement = tx.prepare(
            "SELECT credential_digest, device_id, expires_at_ms FROM enrollment_artifacts
             WHERE state = 'active' AND expires_at_ms <= ?1",
        )?;
        let rows = statement.query_map(params![now_ms], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            expired.push(row?);
        }
    }
    for (artifact_digest, device_id, expires_at_ms) in expired {
        tx.execute(
            "UPDATE enrollment_artifacts SET state = 'expired', tombstone_expires_at_ms = ?1
             WHERE credential_digest = ?2",
            params![
                expires_at_ms.saturating_add(ENROLLMENT_TOMBSTONE_LIFETIME_MS),
                artifact_digest
            ],
        )?;
        tx.execute(
            "DELETE FROM devices WHERE device_id = ?1 AND state = 'pending'",
            params![device_id],
        )?;
    }
    tx.execute(
        "DELETE FROM enrollment_artifacts
         WHERE state != 'active' AND tombstone_expires_at_ms <= ?1",
        params![now_ms],
    )?;
    tx.commit()?;
    Ok(())
}

fn check_receipt(
    connection: &Connection,
    principal_class: &str,
    principal_digest: &[u8; 32],
    operation: &str,
    request: &RequestIdentity,
) -> Result<Option<Uuid>, CoreError> {
    let receipt = connection
        .query_row(
            "SELECT body_sha256, resource_id FROM request_receipts
             WHERE principal_class = ?1 AND principal_digest = ?2 AND operation = ?3 AND request_id = ?4",
            params![
                principal_class,
                principal_digest.as_slice(),
                operation,
                request.request_id.to_string()
            ],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    match receipt {
        None => Ok(None),
        Some((body_sha256, resource_id)) if body_sha256 == request.body_sha256 => {
            Ok(Some(parse_uuid(&resource_id)?))
        }
        Some(_) => Err(CoreError::Failure(FailureCode::RequestIdConflict)),
    }
}

fn insert_receipt(tx: &Transaction<'_>, receipt: NewReceipt<'_>) -> Result<(), CoreError> {
    tx.execute(
        "INSERT INTO request_receipts
         (principal_class, principal_digest, operation, request_id, body_sha256,
          result_code, resource_id, result_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            receipt.principal_class,
            receipt.principal_digest.as_slice(),
            receipt.operation,
            receipt.request.request_id.to_string(),
            receipt.request.body_sha256.as_slice(),
            receipt.result_code,
            receipt.resource_id.to_string(),
            receipt.result_json,
        ],
    )?;
    Ok(())
}

fn replay_nonsecret_receipt(
    connection: &Connection,
    principal_digest: &[u8; 32],
    operation: &str,
    request: &RequestIdentity,
) -> Result<Option<MutationResult>, CoreError> {
    let row = connection
        .query_row(
            "SELECT body_sha256, result_json FROM request_receipts
             WHERE principal_class = 'administrator' AND principal_digest = ?1
               AND operation = ?2 AND request_id = ?3",
            params![
                principal_digest.as_slice(),
                operation,
                request.request_id.to_string()
            ],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((body_sha256, _)) if body_sha256 != request.body_sha256 => {
            Err(CoreError::Failure(FailureCode::RequestIdConflict))
        }
        Some((_, Some(result))) => deserialize_mutation_result(&result).map(Some),
        Some((_, None)) => Err(CoreError::DatabaseIntegrityFailed),
    }
}

fn replay_purge_receipt(
    connection: &Connection,
    principal_digest: &[u8; 32],
    request: &RequestIdentity,
) -> Result<Option<PurgeResult>, CoreError> {
    let row = connection
        .query_row(
            "SELECT body_sha256, result_json FROM request_receipts
             WHERE principal_class = 'administrator' AND principal_digest = ?1
               AND operation = 'purge' AND request_id = ?2",
            params![principal_digest.as_slice(), request.request_id.to_string()],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((body_sha256, _)) if body_sha256 != request.body_sha256 => {
            Err(CoreError::Failure(FailureCode::RequestIdConflict))
        }
        Some((_, Some(result))) => deserialize_purge_result(&result).map(Some),
        Some((_, None)) => Err(CoreError::DatabaseIntegrityFailed),
    }
}

fn serialize_mutation_result(result: &MutationResult) -> Result<String, CoreError> {
    serde_json::to_string(&serde_json::json!({
        "device_id": result.device_id.map(|value| value.to_string()),
        "changed_at_ms": result.changed_at_ms,
    }))
    .map_err(|_| CoreError::DatabaseIntegrityFailed)
}

fn deserialize_mutation_result(value: &str) -> Result<MutationResult, CoreError> {
    let value: serde_json::Value =
        serde_json::from_str(value).map_err(|_| CoreError::DatabaseIntegrityFailed)?;
    let changed_at_ms = value
        .get("changed_at_ms")
        .and_then(serde_json::Value::as_i64)
        .ok_or(CoreError::DatabaseIntegrityFailed)?;
    let device_id = value
        .get("device_id")
        .and_then(serde_json::Value::as_str)
        .map(parse_uuid)
        .transpose()?;
    Ok(MutationResult {
        device_id,
        changed_at_ms,
        cut_off_sessions: Vec::new(),
    })
}

fn serialize_purge_result(result: &PurgeResult) -> Result<String, CoreError> {
    serde_json::to_string(&serde_json::json!({
        "purge_id": result.purge_id.to_string(),
        "history_epoch": result.history_epoch.to_string(),
        "purged_through_cursor": result.purged_through_cursor,
        "purged_at_ms": result.purged_at_ms,
    }))
    .map_err(|_| CoreError::DatabaseIntegrityFailed)
}

fn deserialize_purge_result(value: &str) -> Result<PurgeResult, CoreError> {
    let value: serde_json::Value =
        serde_json::from_str(value).map_err(|_| CoreError::DatabaseIntegrityFailed)?;
    Ok(PurgeResult {
        purge_id: parse_uuid(
            value
                .get("purge_id")
                .and_then(serde_json::Value::as_str)
                .ok_or(CoreError::DatabaseIntegrityFailed)?,
        )?,
        history_epoch: parse_uuid(
            value
                .get("history_epoch")
                .and_then(serde_json::Value::as_str)
                .ok_or(CoreError::DatabaseIntegrityFailed)?,
        )?,
        purged_through_cursor: value
            .get("purged_through_cursor")
            .and_then(serde_json::Value::as_u64),
        purged_at_ms: value
            .get("purged_at_ms")
            .and_then(serde_json::Value::as_i64)
            .ok_or(CoreError::DatabaseIntegrityFailed)?,
        cut_off_sessions: Vec::new(),
    })
}

fn global_pause(connection: &Connection) -> Result<bool, CoreError> {
    Ok(metadata(connection, "global_pause")?.as_deref() == Some("1"))
}

fn device_pause(connection: &Connection, device_id: Uuid) -> Result<bool, CoreError> {
    Ok(connection
        .query_row(
            "SELECT paused FROM devices WHERE device_id = ?1",
            params![device_id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

fn recheck_publish_authority(
    state: &State,
    device_id: Uuid,
    credential_generation: u64,
    session_epoch: Uuid,
) -> Result<(), CoreError> {
    if session_epoch != state.history_epoch {
        return Err(CoreError::Failure(FailureCode::SessionEpochStale));
    }
    if global_pause(&state.connection)? || device_pause(&state.connection, device_id)? {
        return Err(CoreError::Failure(FailureCode::AdministrativelyPaused));
    }
    let record = load_device(&state.connection, device_id)?;
    if record.state != DeviceLifecycle::Active
        || record.credential_generation != Some(credential_generation)
    {
        return Err(CoreError::Failure(FailureCode::Unauthorized));
    }
    Ok(())
}

fn persist_publish_with_retention(
    state: &mut State,
    event: &StoredEvent,
    now_ms: i64,
) -> Result<(), CoreError> {
    let mut next_memory_events = state.memory_events.clone();
    if state.history_mode == HistoryMode::Memory {
        next_memory_events.insert(event.cursor, event.clone());
    }
    let tx = state.connection.transaction()?;
    set_metadata_tx(&tx, "cursor_high_water", &event.cursor.to_string())?;
    tx.execute(
        "INSERT INTO source_watermarks (device_id, source_seq) VALUES (?1, ?2)
         ON CONFLICT(device_id) DO UPDATE SET source_seq = excluded.source_seq",
        params![
            event.event.source_device_id.get().to_string(),
            event.event.source_seq.get().to_string()
        ],
    )?;
    tx.execute(
        "INSERT INTO message_tombstones
         (message_id, source_device_id, source_seq, accepted_cursor)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            event.event.message_id.get().to_string(),
            event.event.source_device_id.get().to_string(),
            event.event.source_seq.get().to_string(),
            event.cursor.to_string(),
        ],
    )?;
    if state.history_mode == HistoryMode::Sqlite {
        tx.execute(
            "INSERT INTO events
             (cursor, message_id, accepted_at_ms, expires_at_ms,
              source_display_name_json, event_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.cursor.to_string(),
                event.event.message_id.get().to_string(),
                event.accepted_at_ms,
                event.event.expires_at_ms,
                serde_json::to_string(&event.source_display_name)
                    .map_err(|_| CoreError::DatabaseIntegrityFailed)?,
                serde_json::to_string(&event.event)
                    .map_err(|_| CoreError::DatabaseIntegrityFailed)?,
            ],
        )?;
    }

    let retained: Vec<(u64, i64)> = match state.history_mode {
        HistoryMode::Memory => next_memory_events
            .values()
            .map(|stored| (stored.cursor, stored.event.expires_at_ms))
            .collect(),
        HistoryMode::Sqlite => {
            let mut statement =
                tx.prepare("SELECT cursor, expires_at_ms FROM events ORDER BY cursor")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            let mut retained = Vec::new();
            for row in rows {
                let (cursor, expires_at_ms) = row?;
                retained.push((
                    cursor
                        .parse()
                        .map_err(|_| CoreError::DatabaseIntegrityFailed)?,
                    expires_at_ms,
                ));
            }
            retained.sort_by_key(|(cursor, _)| *cursor);
            retained
        }
    };
    let remove = retention_removals(&retained, now_ms, state.limits.history_max_entries);
    for cursor in &remove {
        tx.execute(
            "DELETE FROM events WHERE cursor = ?1",
            params![cursor.to_string()],
        )?;
        next_memory_events.remove(cursor);
    }
    let lost_through = remove
        .last()
        .copied()
        .map_or(state.lost_through_cursor, |lost| {
            Some(state.lost_through_cursor.map_or(lost, |old| old.max(lost)))
        });
    set_optional_u64_metadata_tx(&tx, "lost_through_cursor", lost_through)?;
    tx.commit()?;
    state.cursor_high_water = event.cursor;
    if state.history_mode == HistoryMode::Memory {
        state.memory_events = next_memory_events;
    }
    state.lost_through_cursor = lost_through;
    Ok(())
}

fn source_high_water(connection: &Connection, device_id: Uuid) -> Result<Option<u64>, CoreError> {
    parse_optional_u64(
        connection
            .query_row(
                "SELECT source_seq FROM source_watermarks WHERE device_id = ?1",
                params![device_id.to_string()],
                |row| row.get(0),
            )
            .optional()?,
    )
}

fn tombstone(connection: &Connection, message_id: Uuid) -> Result<Option<u64>, CoreError> {
    parse_optional_u64(
        connection
            .query_row(
                "SELECT accepted_cursor FROM message_tombstones WHERE message_id = ?1",
                params![message_id.to_string()],
                |row| row.get(0),
            )
            .optional()?,
    )
}

fn find_retained_event(state: &State, message_id: Uuid) -> Result<Option<StoredEvent>, CoreError> {
    match state.history_mode {
        HistoryMode::Memory => Ok(state
            .memory_events
            .values()
            .find(|event| event.event.message_id.get() == message_id)
            .cloned()),
        HistoryMode::Sqlite => {
            let row = state
                .connection
                .query_row(
                    "SELECT cursor, accepted_at_ms, source_display_name_json, event_json
                     FROM events WHERE message_id = ?1",
                    params![message_id.to_string()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()?;
            row.map(stored_event_from_row).transpose()
        }
    }
}

fn retained_events(state: &State) -> Result<Vec<StoredEvent>, CoreError> {
    match state.history_mode {
        HistoryMode::Memory => Ok(state.memory_events.values().cloned().collect()),
        HistoryMode::Sqlite => {
            let mut statement = state.connection.prepare(
                "SELECT cursor, accepted_at_ms, source_display_name_json, event_json FROM events",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            let mut events = Vec::new();
            for row in rows {
                events.push(stored_event_from_row(row?)?);
            }
            events.sort_by_key(|event| event.cursor);
            Ok(events)
        }
    }
}

fn stored_event_from_row(row: (String, i64, String, String)) -> Result<StoredEvent, CoreError> {
    Ok(StoredEvent {
        cursor: row
            .0
            .parse()
            .map_err(|_| CoreError::DatabaseIntegrityFailed)?,
        accepted_at_ms: row.1,
        source_display_name: serde_json::from_str(&row.2)
            .map_err(|_| CoreError::DatabaseIntegrityFailed)?,
        event: serde_json::from_str(&row.3).map_err(|_| CoreError::DatabaseIntegrityFailed)?,
    })
}

fn expire_and_trim_locked(state: &mut State, now_ms: i64) -> Result<(), CoreError> {
    let events = retained_events(state)?;
    let retained: Vec<_> = events
        .iter()
        .map(|event| (event.cursor, event.event.expires_at_ms))
        .collect();
    let remove = retention_removals(&retained, now_ms, state.limits.history_max_entries);
    if remove.is_empty() {
        return Ok(());
    }
    let lost = *remove.last().expect("remove is nonempty");
    let tx = state.connection.transaction()?;
    for cursor in &remove {
        tx.execute(
            "DELETE FROM events WHERE cursor = ?1",
            params![cursor.to_string()],
        )?;
    }
    let lost_through = Some(state.lost_through_cursor.map_or(lost, |old| old.max(lost)));
    set_optional_u64_metadata_tx(&tx, "lost_through_cursor", lost_through)?;
    tx.commit()?;
    for cursor in remove {
        state.memory_events.remove(&cursor);
    }
    state.lost_through_cursor = lost_through;
    Ok(())
}

fn retention_removals(
    retained: &[(u64, i64)],
    now_ms: i64,
    history_max_entries: usize,
) -> Vec<u64> {
    let mut remove: Vec<u64> = retained
        .iter()
        .filter_map(|(cursor, expires_at_ms)| (*expires_at_ms <= now_ms).then_some(*cursor))
        .collect();
    let remaining = retained.len().saturating_sub(remove.len());
    if remaining > history_max_entries {
        let trim_count = remaining - history_max_entries;
        let trimmed: Vec<_> = retained
            .iter()
            .filter(|(cursor, _)| !remove.contains(cursor))
            .take(trim_count)
            .map(|(cursor, _)| *cursor)
            .collect();
        remove.extend(trimmed);
    }
    remove.sort_unstable();
    remove.dedup();
    remove
}

fn enqueue_publish_accepted(
    state: &mut State,
    source_session_id: Uuid,
    accepted: AcceptedPublish,
) -> Result<(), CoreError> {
    let session = state
        .sessions
        .get_mut(&source_session_id)
        .ok_or(CoreError::Failure(FailureCode::Unauthorized))?;
    if session.phase != SessionPhase::Live || session.history_epoch != state.history_epoch {
        return Err(CoreError::Failure(FailureCode::SessionEpochStale));
    }
    session
        .queue
        .push_back(SessionOutput::PublishAccepted(accepted));
    Ok(())
}

fn enqueue_accepted(
    state: &mut State,
    source_session_id: Uuid,
    event: &StoredEvent,
) -> Result<(), CoreError> {
    enqueue_publish_accepted(
        state,
        source_session_id,
        AcceptedPublish {
            cursor: event.cursor,
            expires_at_ms: event.event.expires_at_ms,
            duplicate: false,
        },
    )?;
    let queued = SessionOutput::LiveEvent(QueuedLiveEvent {
        history_epoch: state.history_epoch,
        event: event.retained(),
    });
    for session in state.sessions.values_mut() {
        if matches!(session.phase, SessionPhase::Buffering | SessionPhase::Live)
            && session.history_epoch == state.history_epoch
        {
            session.queue.push_back(queued.clone());
        }
    }
    Ok(())
}

fn cutoff_device_sessions(state: &mut State, device_id: Uuid) -> Vec<Uuid> {
    let session_ids: Vec<_> = state
        .sessions
        .iter()
        .filter_map(|(session_id, session)| (session.device_id == device_id).then_some(*session_id))
        .collect();
    for session_id in &session_ids {
        state.sessions.remove(session_id);
    }
    session_ids
}

fn cutoff_all_sessions(state: &mut State) -> Vec<Uuid> {
    let session_ids = state.sessions.keys().copied().collect();
    state.sessions.clear();
    session_ids
}

fn parse_uuid(value: &str) -> Result<Uuid, CoreError> {
    Uuid::parse_str(value).map_err(|_| CoreError::DatabaseIntegrityFailed)
}

fn parse_uuid_v4(value: &str) -> Result<Uuid, CoreError> {
    let uuid = parse_uuid(value)?;
    if value != uuid.to_string()
        || uuid.get_version() != Some(Version::Random)
        || uuid.get_variant() != Variant::RFC4122
    {
        return Err(CoreError::DatabaseIntegrityFailed);
    }
    Ok(uuid)
}

#[cfg(test)]
mod tests {
    use std::{env, fs, process::Command, sync::Arc, thread};

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use clipmesh_protocol::{
        AdministratorCredential, ClipboardEventV1, DeviceDisplayName, FailureCode, Platform,
        ResumeStatus,
    };
    use rusqlite::Connection;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    struct Fixture {
        _directory: TempDir,
        database_path: std::path::PathBuf,
        administrator: AdministratorCredential,
        core: HubCore,
    }

    fn fixture(mode: HistoryMode, limits: RetentionLimits) -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("hub.sqlite3");
        let administrator = administrator_credential(7);
        let core = HubCore::open(&database_path, mode, limits, &administrator, NOW).unwrap();
        Fixture {
            _directory: directory,
            database_path,
            administrator,
            core,
        }
    }

    fn administrator_credential(byte: u8) -> AdministratorCredential {
        let wire = format!("cm_admin_v1_{}", URL_SAFE_NO_PAD.encode([byte; 32]));
        AdministratorCredential::from_wire(&wire).unwrap()
    }

    fn display_name(value: &str) -> DeviceDisplayName {
        serde_json::from_value(serde_json::Value::String(value.to_owned())).unwrap()
    }

    fn request(byte: u8) -> RequestIdentity {
        RequestIdentity::new(Uuid::new_v4(), [byte; 32]).unwrap()
    }

    fn create_device(
        core: &HubCore,
        administrator: &AdministratorCredential,
        label: &str,
        request_byte: u8,
    ) -> CreatedDevice {
        core.create_managed_device(
            administrator,
            request(request_byte),
            display_name(label),
            Platform::LinuxWayland,
            NOW,
        )
        .unwrap()
    }

    fn event(
        source_device_id: Uuid,
        source_seq: u64,
        message_id: Uuid,
        text: &str,
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> ClipboardEventV1 {
        event_with_content_type(
            source_device_id,
            source_seq,
            message_id,
            text.as_bytes(),
            "text/plain",
            created_at_ms,
            expires_at_ms,
        )
    }

    fn event_with_content_type(
        source_device_id: Uuid,
        source_seq: u64,
        message_id: Uuid,
        bytes: &[u8],
        content_type: &str,
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> ClipboardEventV1 {
        let hash: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        serde_json::from_value(serde_json::json!({
            "message_id": message_id.to_string(),
            "source_device_id": source_device_id.to_string(),
            "source_seq": source_seq.to_string(),
            "created_at_ms": created_at_ms,
            "expires_at_ms": expires_at_ms,
            "content_type": content_type,
            "payload_bytes": bytes.len(),
            "content_sha256": hash,
            "payload_b64": URL_SAFE_NO_PAD.encode(bytes),
        }))
        .unwrap()
    }

    fn live_session(core: &HubCore, credential: &DeviceCredential) -> Uuid {
        let (session_id, _) = core.open_session(credential).unwrap();
        let plan = core.begin_resume(session_id, None, None, NOW).unwrap();
        assert_eq!(plan.status, ResumeStatus::Fresh);
        core.complete_resume(session_id).unwrap();
        session_id
    }

    fn take_output(core: &HubCore, session_id: Uuid) -> Result<Option<SessionOutput>, CoreError> {
        let mut state = core.state.lock().unwrap();
        let history_epoch = state.history_epoch;
        let session = state
            .sessions
            .get_mut(&session_id)
            .ok_or(CoreError::Failure(FailureCode::Unauthorized))?;
        if session.phase != SessionPhase::Live || session.history_epoch != history_epoch {
            return Err(CoreError::Failure(FailureCode::SessionEpochStale));
        }
        Ok(session.queue.pop_front())
    }

    fn take_live(core: &HubCore, session_id: Uuid) -> Result<Option<QueuedLiveEvent>, CoreError> {
        match take_output(core, session_id)? {
            Some(SessionOutput::LiveEvent(event)) => Ok(Some(event)),
            Some(SessionOutput::PublishAccepted(_)) => panic!("expected a live event"),
            None => Ok(None),
        }
    }

    fn run_restart_probe(
        database_path: &Path,
        mode: HistoryMode,
        prior_epoch: Uuid,
        expected_history_count: usize,
    ) {
        let output = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::process_restart_probe_child",
                "--nocapture",
            ])
            .env("CLIPMESH_RESTART_DB", database_path)
            .env(
                "CLIPMESH_RESTART_MODE",
                match mode {
                    HistoryMode::Sqlite => "sqlite",
                    HistoryMode::Memory => "memory",
                },
            )
            .env("CLIPMESH_RESTART_PRIOR_EPOCH", prior_epoch.to_string())
            .env(
                "CLIPMESH_RESTART_HISTORY_COUNT",
                expected_history_count.to_string(),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "restart probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn process_restart_probe_child() {
        let Ok(database_path) = env::var("CLIPMESH_RESTART_DB") else {
            return;
        };
        let mode = match env::var("CLIPMESH_RESTART_MODE").unwrap().as_str() {
            "sqlite" => HistoryMode::Sqlite,
            "memory" => HistoryMode::Memory,
            _ => panic!("unknown restart probe mode"),
        };
        let prior_epoch =
            Uuid::parse_str(&env::var("CLIPMESH_RESTART_PRIOR_EPOCH").unwrap()).unwrap();
        let expected_history_count: usize = env::var("CLIPMESH_RESTART_HISTORY_COUNT")
            .unwrap()
            .parse()
            .unwrap();
        let core = HubCore::open(
            database_path,
            mode,
            RetentionLimits::default(),
            &administrator_credential(7),
            NOW,
        )
        .unwrap();
        assert_eq!(core.history(NOW).unwrap().len(), expected_history_count);
        match mode {
            HistoryMode::Sqlite => assert_eq!(core.history_epoch(), prior_epoch),
            HistoryMode::Memory => {
                assert_ne!(core.history_epoch(), prior_epoch);
                assert_eq!(core.lost_through_cursor(), Some(1));
            }
        }
    }

    #[test]
    fn credential_classes_are_disjoint_and_records_follow_enrollment_lifecycle() {
        let fixture = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        assert_eq!(
            fixture
                .core
                .authorize(
                    PresentedCredential::Administrator(&fixture.administrator),
                    Authority::Administrator,
                    NOW,
                )
                .unwrap(),
            Principal::Administrator
        );
        assert_eq!(
            fixture.core.authorize(
                PresentedCredential::Administrator(&fixture.administrator),
                Authority::Device,
                NOW,
            ),
            Err(CoreError::Failure(FailureCode::Unauthorized))
        );

        let managed = create_device(&fixture.core, &fixture.administrator, "Managed", 1);
        let device_principal = fixture
            .core
            .authorize(
                PresentedCredential::Device(&managed.credential),
                Authority::Device,
                NOW,
            )
            .unwrap();
        assert_eq!(
            device_principal,
            Principal::Device(DevicePrincipal {
                device_id: managed.record.device_id,
                credential_generation: 1,
            })
        );
        assert_eq!(
            fixture.core.authorize(
                PresentedCredential::Device(&managed.credential),
                Authority::Administrator,
                NOW,
            ),
            Err(CoreError::Failure(FailureCode::Unauthorized))
        );

        let issued = fixture
            .core
            .issue_enrollment_artifact(
                &fixture.administrator,
                request(2),
                display_name("Mobile"),
                Platform::Ios,
                NOW,
            )
            .unwrap();
        assert_eq!(issued.record.state, DeviceLifecycle::Pending);
        assert!(matches!(
            fixture
                .core
                .authorize(
                    PresentedCredential::Enrollment(&issued.artifact),
                    Authority::Enrollment,
                    NOW,
                )
                .unwrap(),
            Principal::Enrollment { device_id } if device_id == issued.record.device_id
        ));

        let exchange_request = request(3);
        let enrolled = fixture
            .core
            .exchange_enrollment(&issued.artifact, exchange_request.clone(), NOW + 1)
            .unwrap();
        assert_eq!(enrolled.device_id, issued.record.device_id);
        assert_eq!(
            fixture
                .core
                .exchange_enrollment(&issued.artifact, exchange_request.clone(), NOW + 2,),
            Err(CoreError::SecretResultAlreadyCommitted {
                resource_id: issued.record.device_id,
            })
        );
        assert_eq!(
            fixture
                .core
                .exchange_enrollment(&issued.artifact, request(4), NOW + 2),
            Err(CoreError::Failure(FailureCode::EnrollmentArtifactInvalid))
        );
        let record = fixture.core.device(issued.record.device_id).unwrap();
        assert_eq!(record.state, DeviceLifecycle::Active);
        assert_eq!(record.credential_generation, Some(1));
        fixture
            .core
            .cleanup_expired_enrollments(NOW + 1 + ENROLLMENT_TOMBSTONE_LIFETIME_MS)
            .unwrap();
        assert_eq!(
            fixture.core.exchange_enrollment(
                &issued.artifact,
                exchange_request,
                NOW + 2 + ENROLLMENT_TOMBSTONE_LIFETIME_MS,
            ),
            Err(CoreError::Failure(FailureCode::Unauthorized))
        );
    }

    #[test]
    fn expired_enrollment_deletes_only_pending_device_and_later_drops_tombstone() {
        let mut fixture = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        let issued = fixture
            .core
            .issue_enrollment_artifact(
                &fixture.administrator,
                request(1),
                display_name("Expiring mobile"),
                Platform::Ipados,
                NOW,
            )
            .unwrap();
        assert_eq!(
            fixture.core.exchange_enrollment(
                &issued.artifact,
                request(2),
                NOW + ENROLLMENT_LIFETIME_MS,
            ),
            Err(CoreError::Failure(FailureCode::EnrollmentArtifactInvalid))
        );
        assert_eq!(
            fixture.core.device(issued.record.device_id),
            Err(CoreError::Failure(FailureCode::Unauthorized))
        );
        drop(fixture.core);
        let connection = Connection::open(&fixture.database_path).unwrap();
        let state: String = connection
            .query_row("SELECT state FROM enrollment_artifacts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(state, "expired");
        drop(connection);
        fixture.core = HubCore::open(
            &fixture.database_path,
            HistoryMode::Sqlite,
            RetentionLimits::default(),
            &fixture.administrator,
            NOW + ENROLLMENT_LIFETIME_MS + ENROLLMENT_TOMBSTONE_LIFETIME_MS,
        )
        .unwrap();
        fixture
            .core
            .cleanup_expired_enrollments(
                NOW + ENROLLMENT_LIFETIME_MS + ENROLLMENT_TOMBSTONE_LIFETIME_MS,
            )
            .unwrap();
        drop(fixture.core);
        let connection = Connection::open(&fixture.database_path).unwrap();
        let count: u64 = connection
            .query_row("SELECT count(*) FROM enrollment_artifacts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn concurrent_enrollment_exchanges_have_one_winner() {
        let fixture = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        let issued = fixture
            .core
            .issue_enrollment_artifact(
                &fixture.administrator,
                request(1),
                display_name("Racing mobile"),
                Platform::Ios,
                NOW,
            )
            .unwrap();
        let core = Arc::new(fixture.core);
        let left_core = Arc::clone(&core);
        let left_artifact = issued.artifact.clone();
        let left = thread::spawn(move || {
            left_core.exchange_enrollment(&left_artifact, request(2), NOW + 1)
        });
        let right_core = Arc::clone(&core);
        let right_artifact = issued.artifact.clone();
        let right = thread::spawn(move || {
            right_core.exchange_enrollment(&right_artifact, request(3), NOW + 1)
        });
        let results = [left.join().unwrap(), right.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    matches!(
                        result,
                        Err(CoreError::Failure(FailureCode::EnrollmentArtifactInvalid))
                    )
                })
                .count(),
            1
        );
        assert_eq!(
            core.device(issued.record.device_id).unwrap().state,
            DeviceLifecycle::Active
        );
    }

    #[test]
    fn unknown_database_shape_is_refused_without_schema_accommodation() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("unknown.sqlite3");
        let connection = Connection::open(&database_path).unwrap();
        connection
            .execute("CREATE TABLE unrelated (value TEXT)", [])
            .unwrap();
        drop(connection);
        assert!(matches!(
            HubCore::open(
                &database_path,
                HistoryMode::Sqlite,
                RetentionLimits::default(),
                &administrator_credential(9),
                NOW,
            ),
            Err(CoreError::DatabaseSchemaUnsupported)
        ));
        let connection = Connection::open(&database_path).unwrap();
        let devices_table: u64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'devices'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(devices_table, 0);

        let empty_path = directory.path().join("preexisting-empty.sqlite3");
        fs::File::create(&empty_path).unwrap();
        assert!(matches!(
            HubCore::open(
                &empty_path,
                HistoryMode::Sqlite,
                RetentionLimits::default(),
                &administrator_credential(9),
                NOW,
            ),
            Err(CoreError::DatabaseSchemaUnsupported)
        ));
    }

    #[test]
    fn publish_validation_rejections_leave_all_event_state_unchanged() {
        let fixture = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        let source = create_device(&fixture.core, &fixture.administrator, "Source", 1);
        let other = create_device(&fixture.core, &fixture.administrator, "Other", 2);
        let session = live_session(&fixture.core, &source.credential);
        let wrong_source = event(
            other.record.device_id,
            1,
            Uuid::new_v4(),
            "wrong source",
            NOW,
            NOW + 1_000,
        );
        assert_eq!(
            fixture.core.publish(session, wrong_source, NOW),
            Err(CoreError::Failure(FailureCode::SourceDeviceMismatch))
        );
        assert_eq!(fixture.core.cursor_high_water(), 0);
        assert!(fixture.core.history(NOW).unwrap().is_empty());
        assert_eq!(
            source_high_water(
                &fixture.core.state.lock().unwrap().connection,
                source.record.device_id,
            )
            .unwrap(),
            None
        );

        let future_and_wrong_type = event_with_content_type(
            source.record.device_id,
            1,
            Uuid::new_v4(),
            b"ordered validation",
            "application/octet-stream",
            NOW + 120_001,
            NOW + 121_000,
        );
        assert_eq!(
            fixture.core.publish(session, future_and_wrong_type, NOW),
            Err(CoreError::Failure(FailureCode::CreatedAtInFuture))
        );
        let wrong_type = event_with_content_type(
            source.record.device_id,
            1,
            Uuid::new_v4(),
            b"ordered validation",
            "application/octet-stream",
            NOW,
            NOW + 1_000,
        );
        assert_eq!(
            fixture.core.publish(session, wrong_type, NOW),
            Err(CoreError::Failure(FailureCode::ContentTypeUnsupported))
        );
        assert_eq!(fixture.core.cursor_high_water(), 0);
    }

    #[test]
    fn time_and_payload_limits_accept_exact_boundaries_and_reject_the_next_value() {
        let fixture = fixture(HistoryMode::Memory, RetentionLimits::default());
        let source = create_device(&fixture.core, &fixture.administrator, "Source", 1);
        let session = live_session(&fixture.core, &source.credential);
        let payload = vec![b'x'; RetentionLimits::default().max_payload_bytes as usize];
        let created = NOW + 120_000;
        let expires = created + RetentionLimits::default().retention_seconds as i64 * 1_000;
        let accepted = event_with_content_type(
            source.record.device_id,
            1,
            Uuid::new_v4(),
            &payload,
            "text/plain",
            created,
            expires,
        );
        fixture.core.publish(session, accepted, NOW).unwrap();
        assert_eq!(fixture.core.cursor_high_water(), 1);

        let too_large = vec![b'y'; payload.len() + 1];
        let rejected = event_with_content_type(
            source.record.device_id,
            2,
            Uuid::new_v4(),
            &too_large,
            "text/plain",
            NOW,
            NOW + 1_000,
        );
        assert_eq!(
            fixture.core.publish(session, rejected, NOW),
            Err(CoreError::Failure(FailureCode::PayloadTooLarge))
        );
        let expiry_too_long = event(
            source.record.device_id,
            2,
            Uuid::new_v4(),
            "expiry",
            NOW,
            NOW + RetentionLimits::default().retention_seconds as i64 * 1_000 + 1,
        );
        assert_eq!(
            fixture.core.publish(session, expiry_too_long, NOW),
            Err(CoreError::Failure(FailureCode::ExpiryExceedsRetention))
        );
    }

    #[test]
    fn concurrent_devices_share_one_unreused_cursor_order() {
        let fixture = fixture(
            HistoryMode::Sqlite,
            RetentionLimits {
                history_max_entries: 30,
                ..RetentionLimits::default()
            },
        );
        let core = Arc::new(fixture.core);
        let receiver_a = create_device(&core, &fixture.administrator, "Receiver A", 40);
        let receiver_b = create_device(&core, &fixture.administrator, "Receiver B", 41);
        let receiver_a_session = live_session(&core, &receiver_a.credential);
        let receiver_b_session = live_session(&core, &receiver_b.credential);
        let devices: Vec<_> = (0..25)
            .map(|index| {
                create_device(
                    &core,
                    &fixture.administrator,
                    &format!("Device {index}"),
                    index as u8,
                )
            })
            .collect();
        let handles: Vec<_> = devices
            .into_iter()
            .enumerate()
            .map(|(index, device)| {
                let core = Arc::clone(&core);
                thread::spawn(move || {
                    let session = live_session(&core, &device.credential);
                    core.publish(
                        session,
                        event(
                            device.record.device_id,
                            1,
                            Uuid::new_v4(),
                            &format!("event {index}"),
                            NOW,
                            NOW + 1_000,
                        ),
                        NOW,
                    )
                    .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let cursors: Vec<_> = core
            .history(NOW)
            .unwrap()
            .into_iter()
            .map(|event| event.cursor)
            .collect();
        assert_eq!(cursors, (1..=25).collect::<Vec<_>>());
        for receiver in [receiver_a_session, receiver_b_session] {
            let mut delivered = Vec::new();
            while let Some(event) = take_live(&core, receiver).unwrap() {
                delivered.push(event.event.cursor);
            }
            assert_eq!(delivered, (1..=25).collect::<Vec<_>>());
        }
    }

    #[test]
    fn retry_replay_and_purge_preserve_tombstones_without_advancing_state() {
        let fixture = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        let source = create_device(&fixture.core, &fixture.administrator, "Source", 1);
        let peer = create_device(&fixture.core, &fixture.administrator, "Peer", 9);
        let session = live_session(&fixture.core, &source.credential);
        let peer_session = live_session(&fixture.core, &peer.credential);
        let message_id = Uuid::new_v4();
        let original = event(
            source.record.device_id,
            1,
            message_id,
            "retained",
            NOW,
            NOW + 10_000,
        );
        fixture
            .core
            .publish(session, original.clone(), NOW)
            .unwrap();
        assert_eq!(
            take_output(&fixture.core, session).unwrap().unwrap(),
            SessionOutput::PublishAccepted(AcceptedPublish {
                cursor: 1,
                expires_at_ms: NOW + 10_000,
                duplicate: false,
            })
        );
        assert!(matches!(
            take_output(&fixture.core, session).unwrap(),
            Some(SessionOutput::LiveEvent(_))
        ));
        for _ in 0..10 {
            fixture
                .core
                .publish(session, original.clone(), NOW)
                .unwrap();
            assert_eq!(
                take_output(&fixture.core, session).unwrap().unwrap(),
                SessionOutput::PublishAccepted(AcceptedPublish {
                    cursor: 1,
                    expires_at_ms: NOW + 10_000,
                    duplicate: true,
                })
            );
        }
        assert!(take_live(&fixture.core, peer_session).unwrap().is_some());
        assert!(take_live(&fixture.core, peer_session).unwrap().is_none());
        let changed = event(
            source.record.device_id,
            1,
            message_id,
            "changed",
            NOW,
            NOW + 10_000,
        );
        assert_eq!(
            fixture.core.publish(session, changed, NOW),
            Err(CoreError::Failure(FailureCode::MessageIdConflict))
        );
        let reused_sequence = event(
            source.record.device_id,
            1,
            Uuid::new_v4(),
            "new id",
            NOW,
            NOW + 10_000,
        );
        assert_eq!(
            fixture.core.publish(session, reused_sequence, NOW),
            Err(CoreError::Failure(FailureCode::SourceSequenceReplay))
        );
        let old_epoch = fixture.core.history_epoch();
        let purged = fixture
            .core
            .purge(&fixture.administrator, request(2), NOW + 1)
            .unwrap();
        assert_ne!(purged.history_epoch, old_epoch);
        assert_eq!(purged.purged_through_cursor, Some(1));
        assert!(fixture.core.history(NOW + 1).unwrap().is_empty());
        let new_session = live_session(&fixture.core, &source.credential);
        assert_eq!(
            fixture.core.publish(new_session, original, NOW + 1),
            Err(CoreError::Failure(FailureCode::MessageIdReplay))
        );
        assert_eq!(fixture.core.cursor_high_water(), 1);
    }

    #[test]
    fn concurrent_identical_message_ids_commit_once_for_one_hundred_races() {
        let fixture = fixture(
            HistoryMode::Sqlite,
            RetentionLimits {
                history_max_entries: 200,
                ..RetentionLimits::default()
            },
        );
        let source = create_device(&fixture.core, &fixture.administrator, "Source", 1);
        let first_session = live_session(&fixture.core, &source.credential);
        let second_session = live_session(&fixture.core, &source.credential);
        let core = Arc::new(fixture.core);
        for sequence in 1..=100 {
            let event = event(
                source.record.device_id,
                sequence,
                Uuid::new_v4(),
                &format!("race {sequence}"),
                NOW,
                NOW + 10_000,
            );
            let left_core = Arc::clone(&core);
            let left_event = event.clone();
            let left =
                thread::spawn(move || left_core.publish(first_session, left_event, NOW).unwrap());
            let right_core = Arc::clone(&core);
            let right =
                thread::spawn(move || right_core.publish(second_session, event, NOW).unwrap());
            left.join().unwrap();
            right.join().unwrap();
            assert_eq!(core.cursor_high_water(), sequence);
        }
        assert_eq!(core.cursor_high_water(), 100);
        assert_eq!(core.history(NOW).unwrap().len(), 100);
    }

    #[test]
    fn resume_snapshot_and_buffered_live_events_share_one_boundary() {
        let fixture = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        let source = create_device(&fixture.core, &fixture.administrator, "Source", 1);
        let target = create_device(&fixture.core, &fixture.administrator, "Target", 2);
        let source_session = live_session(&fixture.core, &source.credential);
        fixture
            .core
            .publish(
                source_session,
                event(
                    source.record.device_id,
                    1,
                    Uuid::new_v4(),
                    "resume event",
                    NOW,
                    NOW + 10_000,
                ),
                NOW,
            )
            .unwrap();
        let (target_session, _) = fixture.core.open_session(&target.credential).unwrap();
        let plan = fixture
            .core
            .begin_resume(target_session, None, None, NOW)
            .unwrap();
        assert_eq!(plan.boundary_cursor, Some(1));
        assert_eq!(
            plan.events
                .iter()
                .map(|event| event.cursor)
                .collect::<Vec<_>>(),
            vec![1]
        );
        fixture
            .core
            .publish(
                source_session,
                event(
                    source.record.device_id,
                    2,
                    Uuid::new_v4(),
                    "live event",
                    NOW,
                    NOW + 10_000,
                ),
                NOW,
            )
            .unwrap();
        fixture.core.complete_resume(target_session).unwrap();
        assert_eq!(
            take_live(&fixture.core, target_session)
                .unwrap()
                .unwrap()
                .event
                .cursor,
            2
        );
    }

    #[test]
    fn resume_rejects_unbound_or_ahead_cursors_and_epoch_change_replays_window() {
        let fixture = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        let source = create_device(&fixture.core, &fixture.administrator, "Source", 1);
        let source_session = live_session(&fixture.core, &source.credential);
        fixture
            .core
            .publish(
                source_session,
                event(
                    source.record.device_id,
                    1,
                    Uuid::new_v4(),
                    "retained",
                    NOW,
                    NOW + 10_000,
                ),
                NOW,
            )
            .unwrap();
        let target = create_device(&fixture.core, &fixture.administrator, "Target", 2);
        let (unbound_session, _) = fixture.core.open_session(&target.credential).unwrap();
        assert_eq!(
            fixture
                .core
                .begin_resume(unbound_session, None, Some(1), NOW),
            Err(CoreError::Failure(FailureCode::ResumeCursorWithoutEpoch))
        );
        let (ahead_session, _) = fixture.core.open_session(&target.credential).unwrap();
        assert_eq!(
            fixture.core.begin_resume(
                ahead_session,
                Some(fixture.core.history_epoch()),
                Some(2),
                NOW,
            ),
            Err(CoreError::Failure(FailureCode::CursorAhead))
        );
        let (changed_session, _) = fixture.core.open_session(&target.credential).unwrap();
        let changed = fixture
            .core
            .begin_resume(changed_session, Some(Uuid::new_v4()), Some(999), NOW)
            .unwrap();
        assert_eq!(changed.status, ResumeStatus::EpochChanged);
        assert_eq!(
            changed
                .events
                .iter()
                .map(|event| event.cursor)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn trimming_and_expiry_report_truthful_resume_gaps() {
        let fixture = fixture(
            HistoryMode::Sqlite,
            RetentionLimits {
                history_max_entries: 2,
                ..RetentionLimits::default()
            },
        );
        let source = create_device(&fixture.core, &fixture.administrator, "Source", 1);
        let target = create_device(&fixture.core, &fixture.administrator, "Target", 2);
        let source_session = live_session(&fixture.core, &source.credential);
        for sequence in 1..=4 {
            fixture
                .core
                .publish(
                    source_session,
                    event(
                        source.record.device_id,
                        sequence,
                        Uuid::new_v4(),
                        &format!("event {sequence}"),
                        NOW,
                        NOW + 10_000,
                    ),
                    NOW,
                )
                .unwrap();
        }
        assert_eq!(
            fixture
                .core
                .history(NOW)
                .unwrap()
                .iter()
                .map(|event| event.cursor)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert_eq!(fixture.core.lost_through_cursor(), Some(2));
        let (target_session, _) = fixture.core.open_session(&target.credential).unwrap();
        let plan = fixture
            .core
            .begin_resume(
                target_session,
                Some(fixture.core.history_epoch()),
                Some(1),
                NOW,
            )
            .unwrap();
        assert_eq!(plan.status, ResumeStatus::Gap);
        assert_eq!(
            plan.events
                .iter()
                .map(|event| event.cursor)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );

        let expiring = event(
            source.record.device_id,
            5,
            Uuid::new_v4(),
            "expires",
            NOW,
            NOW + 1,
        );
        fixture.core.publish(source_session, expiring, NOW).unwrap();
        assert!(fixture
            .core
            .history(NOW + 1)
            .unwrap()
            .iter()
            .all(|event| event.cursor != 5));
        assert_eq!(fixture.core.lost_through_cursor(), Some(5));
    }

    #[test]
    fn retention_failure_rolls_back_the_entire_publish_unit() {
        let fixture = fixture(
            HistoryMode::Sqlite,
            RetentionLimits {
                history_max_entries: 1,
                ..RetentionLimits::default()
            },
        );
        let source = create_device(&fixture.core, &fixture.administrator, "Source", 1);
        let session = live_session(&fixture.core, &source.credential);
        let first_message_id = Uuid::new_v4();
        fixture
            .core
            .publish(
                session,
                event(
                    source.record.device_id,
                    1,
                    first_message_id,
                    "first",
                    NOW,
                    NOW + 10_000,
                ),
                NOW,
            )
            .unwrap();
        take_output(&fixture.core, session).unwrap().unwrap();
        take_output(&fixture.core, session).unwrap().unwrap();

        fixture
            .core
            .state
            .lock()
            .unwrap()
            .connection
            .execute_batch(
                "CREATE TRIGGER fail_event_delete BEFORE DELETE ON events
                 BEGIN SELECT RAISE(FAIL, 'injected retention failure'); END;",
            )
            .unwrap();
        let second_message_id = Uuid::new_v4();
        assert_eq!(
            fixture.core.publish(
                session,
                event(
                    source.record.device_id,
                    2,
                    second_message_id,
                    "second",
                    NOW,
                    NOW + 10_000,
                ),
                NOW,
            ),
            Err(CoreError::StorageUnavailable)
        );

        let state = fixture.core.state.lock().unwrap();
        assert_eq!(state.cursor_high_water, 1);
        assert_eq!(
            source_high_water(&state.connection, source.record.device_id).unwrap(),
            Some(1)
        );
        assert_eq!(
            tombstone(&state.connection, first_message_id).unwrap(),
            Some(1)
        );
        assert_eq!(
            tombstone(&state.connection, second_message_id).unwrap(),
            None
        );
        assert_eq!(retained_events(&state).unwrap().len(), 1);
        assert!(state.sessions.get(&session).unwrap().queue.is_empty());
    }

    #[test]
    fn sqlite_history_survives_restart_and_memory_history_rotates_without_payload_durability() {
        let mut sqlite = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        let device = create_device(&sqlite.core, &sqlite.administrator, "Persistent", 1);
        let session = live_session(&sqlite.core, &device.credential);
        sqlite
            .core
            .publish(
                session,
                event(
                    device.record.device_id,
                    1,
                    Uuid::new_v4(),
                    "persistent event",
                    NOW,
                    NOW + 10_000,
                ),
                NOW,
            )
            .unwrap();
        let epoch = sqlite.core.history_epoch();
        drop(sqlite.core);
        run_restart_probe(&sqlite.database_path, HistoryMode::Sqlite, epoch, 1);
        sqlite.core = HubCore::open(
            &sqlite.database_path,
            HistoryMode::Sqlite,
            RetentionLimits::default(),
            &sqlite.administrator,
            NOW,
        )
        .unwrap();
        assert_eq!(sqlite.core.history_epoch(), epoch);
        assert_eq!(sqlite.core.history(NOW).unwrap().len(), 1);
        let reopened = live_session(&sqlite.core, &device.credential);
        assert_eq!(
            sqlite.core.publish(
                reopened,
                event(
                    device.record.device_id,
                    1,
                    Uuid::new_v4(),
                    "replayed sequence",
                    NOW,
                    NOW + 10_000,
                ),
                NOW,
            ),
            Err(CoreError::Failure(FailureCode::SourceSequenceReplay))
        );

        let mut memory = fixture(HistoryMode::Memory, RetentionLimits::default());
        let memory_device = create_device(&memory.core, &memory.administrator, "Memory", 2);
        let memory_session = live_session(&memory.core, &memory_device.credential);
        let canary = "memory-only-payload-canary";
        memory
            .core
            .publish(
                memory_session,
                event(
                    memory_device.record.device_id,
                    1,
                    Uuid::new_v4(),
                    canary,
                    NOW,
                    NOW + 10_000,
                ),
                NOW,
            )
            .unwrap();
        let old_epoch = memory.core.history_epoch();
        drop(memory.core);
        run_restart_probe(&memory.database_path, HistoryMode::Memory, old_epoch, 0);
        let database_bytes = fs::read(&memory.database_path).unwrap();
        assert!(!database_bytes
            .windows(canary.len())
            .any(|window| window == canary.as_bytes()));
        let connection = Connection::open(&memory.database_path).unwrap();
        let event_count: u64 = connection
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(event_count, 0);
        drop(connection);
        memory.core = HubCore::open(
            &memory.database_path,
            HistoryMode::Memory,
            RetentionLimits::default(),
            &memory.administrator,
            NOW,
        )
        .unwrap();
        assert_ne!(memory.core.history_epoch(), old_epoch);
        assert!(memory.core.history(NOW).unwrap().is_empty());
        assert_eq!(memory.core.lost_through_cursor(), Some(1));
        let new_session = live_session(&memory.core, &memory_device.credential);
        assert_eq!(
            memory.core.publish(
                new_session,
                event(
                    memory_device.record.device_id,
                    1,
                    Uuid::new_v4(),
                    "old sequence",
                    NOW,
                    NOW + 10_000,
                ),
                NOW,
            ),
            Err(CoreError::Failure(FailureCode::SourceSequenceReplay))
        );
        let memory_epoch = memory.core.history_epoch();
        drop(memory.core);
        memory.core = HubCore::open(
            &memory.database_path,
            HistoryMode::Sqlite,
            RetentionLimits::default(),
            &memory.administrator,
            NOW,
        )
        .unwrap();
        assert_ne!(memory.core.history_epoch(), memory_epoch);
        assert!(memory.core.history(NOW).unwrap().is_empty());
        assert_eq!(memory.core.lost_through_cursor(), Some(1));
    }

    #[test]
    fn administrative_mutations_clear_affected_session_queues() {
        let fixture = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        let source = create_device(&fixture.core, &fixture.administrator, "Source", 1);
        let peer = create_device(&fixture.core, &fixture.administrator, "Peer", 2);
        let source_session = live_session(&fixture.core, &source.credential);
        let peer_session = live_session(&fixture.core, &peer.credential);
        fixture
            .core
            .publish(
                source_session,
                event(
                    source.record.device_id,
                    1,
                    Uuid::new_v4(),
                    "queued before rotation",
                    NOW,
                    NOW + 10_000,
                ),
                NOW,
            )
            .unwrap();
        let rotated = fixture
            .core
            .rotate_credential(
                &fixture.administrator,
                request(3),
                source.record.device_id,
                NOW + 1,
            )
            .unwrap();
        assert_eq!(rotated.cut_off_sessions, vec![source_session]);
        assert_eq!(
            take_output(&fixture.core, source_session),
            Err(CoreError::Failure(FailureCode::Unauthorized))
        );
        assert_eq!(
            take_live(&fixture.core, peer_session)
                .unwrap()
                .unwrap()
                .event
                .cursor,
            1
        );

        let new_source_session = live_session(&fixture.core, &rotated.credential);
        let second_message_id = Uuid::new_v4();
        fixture
            .core
            .publish(
                new_source_session,
                event(
                    source.record.device_id,
                    2,
                    second_message_id,
                    "queued before pause",
                    NOW + 1,
                    NOW + 10_000,
                ),
                NOW + 1,
            )
            .unwrap();
        let paused = fixture
            .core
            .set_pause(
                &fixture.administrator,
                request(4),
                Some(peer.record.device_id),
                true,
                NOW + 2,
            )
            .unwrap();
        assert_eq!(paused.cut_off_sessions, vec![peer_session]);
        assert_eq!(
            take_output(&fixture.core, peer_session),
            Err(CoreError::Failure(FailureCode::Unauthorized))
        );
        assert_eq!(
            take_output(&fixture.core, new_source_session)
                .unwrap()
                .unwrap(),
            SessionOutput::PublishAccepted(AcceptedPublish {
                cursor: 2,
                expires_at_ms: NOW + 10_000,
                duplicate: false,
            })
        );
        assert_eq!(
            take_live(&fixture.core, new_source_session)
                .unwrap()
                .unwrap()
                .event
                .cursor,
            2
        );
        let global = fixture
            .core
            .set_pause(&fixture.administrator, request(5), None, true, NOW + 3)
            .unwrap();
        assert_eq!(global.cut_off_sessions, vec![new_source_session]);
        assert_eq!(
            fixture.core.open_session(&rotated.credential),
            Err(CoreError::Failure(FailureCode::AdministrativelyPaused))
        );
    }

    #[test]
    fn revoke_preserves_other_device_and_old_epoch_publish_cannot_commit() {
        let fixture = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        let revoked = create_device(&fixture.core, &fixture.administrator, "Revoked", 1);
        let peer = create_device(&fixture.core, &fixture.administrator, "Peer", 2);
        let revoked_session = live_session(&fixture.core, &revoked.credential);
        let peer_session = live_session(&fixture.core, &peer.credential);
        fixture
            .core
            .revoke_device(
                &fixture.administrator,
                request(3),
                revoked.record.device_id,
                NOW,
            )
            .unwrap();
        assert_eq!(
            fixture.core.publish(
                revoked_session,
                event(
                    revoked.record.device_id,
                    1,
                    Uuid::new_v4(),
                    "must not commit",
                    NOW,
                    NOW + 1_000,
                ),
                NOW,
            ),
            Err(CoreError::Failure(FailureCode::Unauthorized))
        );
        assert_eq!(
            fixture.core.open_session(&revoked.credential),
            Err(CoreError::Failure(FailureCode::Unauthorized))
        );
        fixture
            .core
            .publish(
                peer_session,
                event(
                    peer.record.device_id,
                    1,
                    Uuid::new_v4(),
                    "peer remains live",
                    NOW,
                    NOW + 1_000,
                ),
                NOW,
            )
            .unwrap();
        assert_eq!(fixture.core.cursor_high_water(), 1);
    }

    #[test]
    fn cursor_exhaustion_never_wraps_and_marks_core_not_ready() {
        let fixture = fixture(HistoryMode::Memory, RetentionLimits::default());
        let source = create_device(&fixture.core, &fixture.administrator, "Source", 1);
        let session = live_session(&fixture.core, &source.credential);
        fixture
            .core
            .set_cursor_high_water_for_test(u64::MAX)
            .unwrap();
        assert_eq!(
            fixture.core.publish(
                session,
                event(
                    source.record.device_id,
                    1,
                    Uuid::new_v4(),
                    "no wrap",
                    NOW,
                    NOW + 1_000,
                ),
                NOW,
            ),
            Err(CoreError::Failure(FailureCode::HubCursorExhausted))
        );
        assert_eq!(fixture.core.cursor_high_water(), u64::MAX);
        assert!(!fixture.core.is_ready());
    }

    #[test]
    fn request_ids_are_v4_and_conflicting_reuse_changes_no_state() {
        assert_eq!(
            RequestIdentity::new(Uuid::nil(), [0; 32]),
            Err(CoreError::Failure(FailureCode::ProtocolSchemaInvalid))
        );
        let fixture = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        let request_id = Uuid::new_v4();
        let first = RequestIdentity::new(request_id, [1; 32]).unwrap();
        let created = fixture
            .core
            .create_managed_device(
                &fixture.administrator,
                first,
                display_name("One"),
                Platform::Macos,
                NOW,
            )
            .unwrap();
        assert_eq!(
            fixture.core.create_managed_device(
                &fixture.administrator,
                RequestIdentity::new(request_id, [2; 32]).unwrap(),
                display_name("Two"),
                Platform::Macos,
                NOW,
            ),
            Err(CoreError::Failure(FailureCode::RequestIdConflict))
        );
        let state = fixture.core.state.lock().unwrap();
        let count: u64 = state
            .connection
            .query_row("SELECT count(*) FROM devices", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert!(load_device(&state.connection, created.record.device_id).is_ok());
    }

    #[test]
    fn sqlite_file_uses_secure_delete_delete_journal_and_owner_only_mode() {
        let fixture = fixture(HistoryMode::Sqlite, RetentionLimits::default());
        let state = fixture.core.state.lock().unwrap();
        let secure_delete: u8 = state
            .connection
            .query_row("PRAGMA secure_delete", [], |row| row.get(0))
            .unwrap();
        let journal_mode: String = state
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let synchronous: u8 = state
            .connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(secure_delete, 1);
        assert_eq!(journal_mode, "delete");
        assert_eq!(synchronous, 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&fixture.database_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
