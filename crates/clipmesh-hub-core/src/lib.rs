//! Transport-neutral ClipMesh hub policy and durable state.
//!
//! The edge supplies a stable Tailnet peer ID after admission. This crate does
//! not implement LocalAPI, HTTP, WebSocket, TLS, a listener, or membership.

use std::{
    collections::{HashMap, VecDeque},
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 1;
const HARD_MAX_PAYLOAD_BYTES: usize = 1_048_576;
const EXPECTED_TABLES: &[&str] = &["clear_receipts", "clips", "hub_meta", "message_tombstones"];
const EXPECTED_SCHEMA: &[(&str, &str)] = &[
    (
        "hub_meta",
        "CREATE TABLE hub_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),history_epoch TEXT NOT NULL,clear_generation TEXT NOT NULL,cursor_high_water TEXT NOT NULL,lost_through_cursor TEXT)",
    ),
    (
        "message_tombstones",
        "CREATE TABLE message_tombstones(message_id TEXT PRIMARY KEY,accepted_cursor TEXT NOT NULL,clear_generation TEXT NOT NULL)",
    ),
    (
        "clips",
        "CREATE TABLE clips(cursor TEXT PRIMARY KEY,message_id TEXT NOT NULL UNIQUE,source_peer_id TEXT NOT NULL,clear_generation TEXT NOT NULL,created_at_ms INTEGER NOT NULL,accepted_at_ms INTEGER NOT NULL,expires_at_ms INTEGER NOT NULL,content BLOB NOT NULL)",
    ),
    (
        "clear_receipts",
        "CREATE TABLE clear_receipts(request_id TEXT PRIMARY KEY,expected_generation TEXT NOT NULL,committed_generation TEXT NOT NULL,cleared_through_cursor TEXT)",
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionLimits {
    pub retention_seconds: u64,
    pub history_max_entries: usize,
    pub max_payload_bytes: usize,
}

impl Default for RetentionLimits {
    fn default() -> Self {
        Self {
            retention_seconds: 604_800,
            history_max_entries: 500,
            max_payload_bytes: 262_144,
        }
    }
}

impl RetentionLimits {
    fn validate(self) -> Result<Self, CoreError> {
        if !(60..=31_536_000).contains(&self.retention_seconds)
            || !(1..=10_000).contains(&self.history_max_entries)
            || !(1..=HARD_MAX_PAYLOAD_BYTES).contains(&self.max_payload_bytes)
        {
            return Err(CoreError::Failure(FailureCode::ConfigValueInvalid));
        }
        Ok(self)
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct StablePeerId(String);

impl StablePeerId {
    /// Accepts only the stable ID asserted by the trusted transport boundary.
    pub fn from_boundary(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
            return Err(CoreError::Failure(FailureCode::TailnetPeerUnverified));
        }
        Ok(Self(value))
    }

    fn storage_value(&self) -> &str {
        &self.0
    }

    /// Returns the transport-boundary value for protocol serialization.
    ///
    /// Callers must not include this value in diagnostics.
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

#[derive(Clone, Eq, PartialEq)]
pub struct ClipContentV1(Vec<u8>);

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

impl ClipContentV1 {
    pub fn from_wire(
        content_type: &str,
        payload_b64: &str,
        payload_bytes: usize,
        content_sha256: &str,
        max_payload_bytes: usize,
    ) -> Result<Self, CoreError> {
        if content_type != "text/plain" {
            return Err(CoreError::Failure(FailureCode::ContentTypeUnsupported));
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| CoreError::Failure(FailureCode::PayloadEncodingInvalid))?;
        Self::validate(
            bytes,
            max_payload_bytes,
            Some((payload_bytes, content_sha256)),
        )
    }

    pub fn from_platform(bytes: &[u8], max_payload_bytes: usize) -> Result<Self, CoreError> {
        Self::validate(bytes.to_vec(), max_payload_bytes, None)
    }

    pub fn from_storage_blob(bytes: &[u8]) -> Result<Self, CoreError> {
        Self::validate(bytes.to_vec(), HARD_MAX_PAYLOAD_BYTES, None)
            .map_err(|_| CoreError::Failure(FailureCode::DatabaseIntegrityFailed))
    }

    fn validate(
        bytes: Vec<u8>,
        max_payload_bytes: usize,
        wire_metadata: Option<(usize, &str)>,
    ) -> Result<Self, CoreError> {
        if bytes.is_empty() {
            return Err(CoreError::Failure(FailureCode::PayloadEmpty));
        }
        if std::str::from_utf8(&bytes).is_err() {
            return Err(CoreError::Failure(FailureCode::PayloadEncodingInvalid));
        }
        if bytes.len() > max_payload_bytes {
            return Err(CoreError::Failure(FailureCode::PayloadTooLarge));
        }
        if let Some((declared_length, declared_hash)) = wire_metadata {
            if declared_length != bytes.len() {
                return Err(CoreError::Failure(FailureCode::PayloadLengthMismatch));
            }
            if declared_hash != sha256_hex(&bytes) {
                return Err(CoreError::Failure(FailureCode::PayloadHashMismatch));
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureCode {
    ConfigValueInvalid,
    TailnetPeerUnverified,
    DatabaseSchemaUnsupported,
    DatabaseIntegrityFailed,
    StorageUnavailable,
    SessionContextStale,
    ResumeContextIncomplete,
    ResumeCursorWithoutContext,
    CursorAhead,
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
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CoreError {
    #[error("hub operation failed: {0:?}")]
    Failure(FailureCode),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionHello {
    pub session_id: Uuid,
    pub self_peer_id: StablePeerId,
    pub history_epoch: Uuid,
    pub clear_generation: u64,
    pub newest_cursor: Option<u64>,
}

/// Content-free queue accounting for an edge's slow-consumer boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionQueueMetrics {
    pub events: usize,
    /// Conservative encoded-frame upper bound; this never exposes payloads.
    pub wire_upper_bound_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishInput {
    pub message_id: Uuid,
    pub clear_generation: u64,
    pub created_at_ms: i64,
    pub content: ClipContentV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishAccepted {
    pub message_id: Uuid,
    pub cursor: u64,
    pub expires_at_ms: i64,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedClip {
    pub cursor: u64,
    pub message_id: Uuid,
    pub source_peer_id: StablePeerId,
    pub clear_generation: u64,
    pub created_at_ms: i64,
    pub accepted_at_ms: i64,
    pub expires_at_ms: i64,
    pub content: ClipContentV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResumeStatus {
    Fresh,
    Complete,
    Gap,
    EpochChanged,
    GenerationChanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumePlan {
    pub status: ResumeStatus,
    pub history_epoch: Uuid,
    pub clear_generation: u64,
    pub requested_after_cursor: Option<u64>,
    pub boundary_cursor: Option<u64>,
    pub lost_through_cursor: Option<u64>,
    pub clips: Vec<RetainedClip>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeStarted {
    pub status: ResumeStatus,
    pub history_epoch: Uuid,
    pub clear_generation: u64,
    pub requested_after_cursor: Option<u64>,
    pub boundary_cursor: Option<u64>,
    pub lost_through_cursor: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeComplete {
    pub history_epoch: Uuid,
    pub clear_generation: u64,
    pub boundary_cursor: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClearAccepted {
    pub request_id: Uuid,
    pub clear_generation: u64,
    pub cleared_through_cursor: Option<u64>,
    pub duplicate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClearNotice {
    pub request_id: Uuid,
    pub clear_generation: u64,
    pub cleared_through_cursor: Option<u64>,
}

/// Complete transport-neutral events ready for a session writer.
#[derive(Debug, Eq, PartialEq)]
pub enum SessionEvent {
    ResumeStarted(ResumeStarted),
    ResumeClip(RetainedClip),
    ResumeComplete(ResumeComplete),
    PublishAccepted(PublishAccepted),
    Live(RetainedClip),
    ClearAccepted(ClearAccepted),
    ClearNotice(ClearNotice),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionPhase {
    AwaitResume,
    Replaying,
    Live,
    Stale,
}

#[derive(Debug)]
struct Session {
    peer_id: StablePeerId,
    clear_generation: u64,
    phase: SessionPhase,
    queue: VecDeque<SessionEvent>,
    buffered: Vec<RetainedClip>,
    highest_offered_cursor: Option<u64>,
    acknowledged_cursor: Option<u64>,
}

struct State {
    connection: Connection,
    history_epoch: Uuid,
    clear_generation: u64,
    cursor_high_water: u64,
    lost_through_cursor: Option<u64>,
    sessions: HashMap<Uuid, Session>,
}

/// One queued event whose transport handoff remains ordered with hub mutations.
///
/// The caller keeps this lease alive until the synchronous handoff completes,
/// then calls [`SessionEventLease::complete`]. Dropping an incomplete lease
/// leaves the event queued, so a later shared clear can still retract it.
pub struct SessionEventLease<'a> {
    state: MutexGuard<'a, State>,
    session_id: Uuid,
}

impl SessionEventLease<'_> {
    pub fn event(&self) -> &SessionEvent {
        self.state
            .sessions
            .get(&self.session_id)
            .and_then(|session| session.queue.front())
            .expect("leased session event remains queued")
    }

    /// Marks the synchronous handoff complete before releasing the mutation seam.
    pub fn complete(mut self) {
        self.state
            .sessions
            .get_mut(&self.session_id)
            .and_then(|session| session.queue.pop_front())
            .expect("leased session event remains queued");
    }
}

pub struct HubCore {
    database_path: PathBuf,
    limits: RetentionLimits,
    state: Mutex<State>,
}

impl HubCore {
    pub fn open(
        database_path: impl AsRef<Path>,
        limits: RetentionLimits,
    ) -> Result<Self, CoreError> {
        let limits = limits.validate()?;
        let database_path = database_path.as_ref().to_path_buf();
        reject_symlink(&database_path)?;
        let initialize = match fs::metadata(&database_path) {
            Ok(metadata) => metadata.len() == 0,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => return Err(CoreError::Failure(FailureCode::StorageUnavailable)),
        };

        if !initialize {
            validate_existing_schema(&database_path)?;
        }

        let mut connection = Connection::open(&database_path)
            .map_err(|_| CoreError::Failure(FailureCode::StorageUnavailable))?;
        connection
            .execute_batch("PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON;")
            .map_err(|_| CoreError::Failure(FailureCode::StorageUnavailable))?;
        if initialize {
            initialize_schema(&mut connection)?;
            set_owner_only(&database_path)?;
        }
        let (history_epoch, clear_generation, cursor_high_water, lost_through_cursor) =
            load_meta(&connection)?;
        Ok(Self {
            database_path,
            limits,
            state: Mutex::new(State {
                connection,
                history_epoch,
                clear_generation,
                cursor_high_water,
                lost_through_cursor,
                sessions: HashMap::new(),
            }),
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub fn limits(&self) -> RetentionLimits {
        self.limits
    }

    pub fn history_epoch(&self) -> Uuid {
        self.state
            .lock()
            .expect("hub state lock poisoned")
            .history_epoch
    }

    pub fn newest_cursor(&self) -> Option<u64> {
        nonzero(
            self.state
                .lock()
                .expect("hub state lock poisoned")
                .cursor_high_water,
        )
    }

    pub fn clear_generation(&self) -> u64 {
        self.state
            .lock()
            .expect("hub state lock poisoned")
            .clear_generation
    }

    pub fn open_session(&self, peer_id: StablePeerId) -> SessionHello {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        let session_id = Uuid::new_v4();
        let clear_generation = state.clear_generation;
        state.sessions.insert(
            session_id,
            Session {
                peer_id: peer_id.clone(),
                clear_generation,
                phase: SessionPhase::AwaitResume,
                queue: VecDeque::new(),
                buffered: Vec::new(),
                highest_offered_cursor: None,
                acknowledged_cursor: None,
            },
        );
        SessionHello {
            session_id,
            self_peer_id: peer_id,
            history_epoch: state.history_epoch,
            clear_generation: state.clear_generation,
            newest_cursor: nonzero(state.cursor_high_water),
        }
    }

    /// Releases a transport session after its edge has stopped output.
    ///
    /// The edge calls this only after it has stopped the socket. Removing the
    /// session at the state seam makes a subsequent session-limit admission
    /// reflect actual connected writers rather than stale transport state.
    pub fn close_session(&self, session_id: Uuid) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        state
            .sessions
            .remove(&session_id)
            .map(|_| ())
            .ok_or(CoreError::Failure(FailureCode::SessionContextStale))
    }

    pub fn begin_resume(
        &self,
        session_id: Uuid,
        known_history_epoch: Option<Uuid>,
        known_clear_generation: Option<u64>,
        after_cursor: Option<u64>,
        now_ms: i64,
    ) -> Result<ResumePlan, CoreError> {
        validate_resume_shape(known_history_epoch, known_clear_generation, after_cursor)?;
        let mut state = self.state.lock().expect("hub state lock poisoned");
        let phase = state
            .sessions
            .get(&session_id)
            .map(|session| session.phase)
            .ok_or(CoreError::Failure(FailureCode::SessionContextStale))?;
        if phase != SessionPhase::AwaitResume {
            return Err(CoreError::Failure(FailureCode::SessionContextStale));
        }
        apply_retention(&mut state, self.limits, now_ms)?;
        let boundary = nonzero(state.cursor_high_water);
        if after_cursor
            .zip(boundary)
            .is_some_and(|(after, bound)| after > bound)
            || (after_cursor.is_some() && boundary.is_none())
        {
            return Err(CoreError::Failure(FailureCode::CursorAhead));
        }
        let status = if known_clear_generation.is_some_and(|known| known != state.clear_generation)
        {
            ResumeStatus::GenerationChanged
        } else if known_history_epoch.is_some_and(|known| known != state.history_epoch) {
            ResumeStatus::EpochChanged
        } else if known_history_epoch.is_none() {
            ResumeStatus::Fresh
        } else if after_cursor
            .zip(state.lost_through_cursor)
            .is_some_and(|(after, lost)| after < lost)
        {
            ResumeStatus::Gap
        } else {
            ResumeStatus::Complete
        };
        let effective_after = match status {
            ResumeStatus::GenerationChanged | ResumeStatus::EpochChanged | ResumeStatus::Fresh => {
                None
            }
            ResumeStatus::Gap | ResumeStatus::Complete => after_cursor,
        };
        let clips = load_clips(&state.connection, effective_after, boundary, now_ms)?;
        let highest = clips.last().map(|clip| clip.cursor).or(effective_after);
        let clear_generation = state.clear_generation;
        let plan = ResumePlan {
            status,
            history_epoch: state.history_epoch,
            clear_generation: state.clear_generation,
            requested_after_cursor: after_cursor,
            boundary_cursor: boundary,
            lost_through_cursor: state.lost_through_cursor,
            clips,
        };
        let session = state.sessions.get_mut(&session_id).expect("session exists");
        session.phase = SessionPhase::Replaying;
        session.clear_generation = clear_generation;
        session.highest_offered_cursor = highest;
        session
            .queue
            .push_back(SessionEvent::ResumeStarted(ResumeStarted {
                status: plan.status,
                history_epoch: plan.history_epoch,
                clear_generation: plan.clear_generation,
                requested_after_cursor: plan.requested_after_cursor,
                boundary_cursor: plan.boundary_cursor,
                lost_through_cursor: plan.lost_through_cursor,
            }));
        session
            .queue
            .extend(plan.clips.iter().cloned().map(SessionEvent::ResumeClip));
        session
            .queue
            .push_back(SessionEvent::ResumeComplete(ResumeComplete {
                history_epoch: plan.history_epoch,
                clear_generation: plan.clear_generation,
                boundary_cursor: plan.boundary_cursor,
            }));
        Ok(plan)
    }

    pub fn complete_resume(&self, session_id: Uuid) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        let generation = state.clear_generation;
        let session = state
            .sessions
            .get_mut(&session_id)
            .ok_or(CoreError::Failure(FailureCode::SessionContextStale))?;
        if session.phase != SessionPhase::Replaying || session.clear_generation != generation {
            return Err(CoreError::Failure(FailureCode::SessionContextStale));
        }
        for clip in session.buffered.drain(..) {
            session.highest_offered_cursor = Some(clip.cursor);
            session.queue.push_back(SessionEvent::Live(clip));
        }
        session.phase = SessionPhase::Live;
        Ok(())
    }

    /// Leases the next event while holding the same seam used by mutations.
    pub fn lease_next_session_event(
        &self,
        session_id: Uuid,
    ) -> Result<Option<SessionEventLease<'_>>, CoreError> {
        let state = self.state.lock().expect("hub state lock poisoned");
        let session = state
            .sessions
            .get(&session_id)
            .ok_or(CoreError::Failure(FailureCode::SessionContextStale))?;
        if session.queue.is_empty() {
            return Ok(None);
        }
        Ok(Some(SessionEventLease { state, session_id }))
    }

    /// Returns content-free queue bounds while holding the same state seam as
    /// event enqueueing. The edge uses this to close slow consumers before its
    /// transport can retain an unbounded backlog.
    pub fn session_queue_metrics(
        &self,
        session_id: Uuid,
    ) -> Result<SessionQueueMetrics, CoreError> {
        let state = self.state.lock().expect("hub state lock poisoned");
        let session = state
            .sessions
            .get(&session_id)
            .ok_or(CoreError::Failure(FailureCode::SessionContextStale))?;
        let wire_upper_bound_bytes = session
            .queue
            .iter()
            .map(|event| match event {
                SessionEvent::ResumeClip(clip) | SessionEvent::Live(clip) => {
                    4096 + 4 * clip.content.as_storage_blob().len().div_ceil(3)
                }
                _ => 1024,
            })
            .sum();
        Ok(SessionQueueMetrics {
            events: session.queue.len(),
            wire_upper_bound_bytes,
        })
    }

    pub fn publish(
        &self,
        session_id: Uuid,
        input: PublishInput,
        now_ms: i64,
    ) -> Result<PublishAccepted, CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        require_session_exists(&state, session_id)?;
        compare_generation(input.clear_generation, state.clear_generation)?;
        let source_peer_id = require_live_session(&state, session_id)?;

        if let Some(retained) =
            load_clip_by_message_id(&state.connection, input.message_id, now_ms)?
        {
            if retained.source_peer_id == source_peer_id
                && retained.clear_generation == input.clear_generation
                && retained.created_at_ms == input.created_at_ms
                && retained.content.same_content(&input.content)
            {
                let accepted = PublishAccepted {
                    message_id: input.message_id,
                    cursor: retained.cursor,
                    expires_at_ms: retained.expires_at_ms,
                    duplicate: true,
                };
                enqueue_acceptance(&mut state, session_id, accepted.clone());
                return Ok(accepted);
            }
            return Err(CoreError::Failure(FailureCode::MessageIdConflict));
        }
        if tombstone_exists(&state.connection, input.message_id)? {
            return Err(CoreError::Failure(FailureCode::MessageIdReplay));
        }
        let retention_ms = retention_ms(self.limits.retention_seconds)?;
        if input.created_at_ms > now_ms.saturating_add(120_000) {
            return Err(CoreError::Failure(FailureCode::CreatedAtInFuture));
        }
        if input.created_at_ms < now_ms.saturating_sub(retention_ms) {
            return Err(CoreError::Failure(FailureCode::EventTooOld));
        }
        if input.content.as_storage_blob().len() > self.limits.max_payload_bytes {
            return Err(CoreError::Failure(FailureCode::PayloadTooLarge));
        }
        let cursor = state
            .cursor_high_water
            .checked_add(1)
            .ok_or(CoreError::Failure(FailureCode::HubCursorExhausted))?;
        let expires_at_ms = now_ms.saturating_add(retention_ms);
        let clip = RetainedClip {
            cursor,
            message_id: input.message_id,
            source_peer_id,
            clear_generation: state.clear_generation,
            created_at_ms: input.created_at_ms,
            accepted_at_ms: now_ms,
            expires_at_ms,
            content: input.content,
        };
        persist_publish(&mut state, &clip, self.limits, now_ms)?;
        let accepted = PublishAccepted {
            message_id: clip.message_id,
            cursor,
            expires_at_ms,
            duplicate: false,
        };
        enqueue_clip(&mut state, session_id, accepted.clone(), clip);
        Ok(accepted)
    }

    /// Checks the publish state boundary before an edge decodes payload bytes.
    ///
    /// A malformed payload must not mask a stale session or clear generation.
    /// The edge calls this at ingress and `publish` repeats the same checks at
    /// its serialized mutation point to close the intervening-state race.
    pub fn validate_publish_context(
        &self,
        session_id: Uuid,
        clear_generation: u64,
    ) -> Result<(), CoreError> {
        let state = self.state.lock().expect("hub state lock poisoned");
        require_session_exists(&state, session_id)?;
        compare_generation(clear_generation, state.clear_generation)?;
        require_live_session(&state, session_id)?;
        Ok(())
    }

    pub fn history(&self, session_id: Uuid, now_ms: i64) -> Result<Vec<RetainedClip>, CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        require_active_session(&state, session_id)?;
        apply_retention(&mut state, self.limits, now_ms)?;
        load_clips(
            &state.connection,
            None,
            nonzero(state.cursor_high_water),
            now_ms,
        )
    }

    /// Runs the retention work that the hub scheduler invokes every 60 seconds while ready.
    pub fn run_periodic_retention(&self, now_ms: i64) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        apply_retention(&mut state, self.limits, now_ms)
    }

    pub fn acknowledge(
        &self,
        session_id: Uuid,
        history_epoch: Uuid,
        clear_generation: u64,
        cursor: u64,
    ) -> Result<(), CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        if history_epoch != state.history_epoch || clear_generation != state.clear_generation {
            return Err(CoreError::Failure(FailureCode::AckInvalid));
        }
        let session = state
            .sessions
            .get_mut(&session_id)
            .ok_or(CoreError::Failure(FailureCode::SessionContextStale))?;
        if !matches!(session.phase, SessionPhase::Replaying | SessionPhase::Live)
            || session
                .highest_offered_cursor
                .is_none_or(|highest| cursor > highest)
            || session
                .acknowledged_cursor
                .is_some_and(|prior| cursor < prior)
        {
            return Err(CoreError::Failure(FailureCode::AckInvalid));
        }
        session.acknowledged_cursor = Some(cursor);
        Ok(())
    }

    pub fn clear_history(
        &self,
        session_id: Uuid,
        request_id: Uuid,
        expected_clear_generation: u64,
    ) -> Result<ClearAccepted, CoreError> {
        let mut state = self.state.lock().expect("hub state lock poisoned");
        require_session_exists(&state, session_id)?;
        if let Some(receipt) = load_clear_receipt(&state.connection, request_id)? {
            if receipt.0 != expected_clear_generation {
                return Err(CoreError::Failure(FailureCode::RequestIdConflict));
            }
            let accepted = ClearAccepted {
                request_id,
                clear_generation: receipt.1,
                cleared_through_cursor: receipt.2,
                duplicate: true,
            };
            enqueue_clear_acceptance(&mut state, session_id, accepted.clone());
            return Ok(accepted);
        }
        compare_generation(expected_clear_generation, state.clear_generation)?;
        require_live_session(&state, session_id)?;
        let next_generation = state
            .clear_generation
            .checked_add(1)
            .ok_or(CoreError::Failure(FailureCode::ClearGenerationExhausted))?;
        let cleared_through_cursor = nonzero(state.cursor_high_water);
        let tx = state
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        tx.execute("DELETE FROM clips", []).map_err(storage_error)?;
        tx.execute(
            "UPDATE hub_meta SET clear_generation=?1, lost_through_cursor=?2 WHERE singleton=1",
            params![
                counter_key(next_generation),
                cleared_through_cursor.map(counter_key)
            ],
        )
        .map_err(storage_error)?;
        tx.execute(
            "INSERT INTO clear_receipts(request_id, expected_generation, committed_generation, cleared_through_cursor) VALUES (?1, ?2, ?3, ?4)",
            params![request_id.to_string(), counter_key(expected_clear_generation), counter_key(next_generation), cleared_through_cursor.map(counter_key)],
        )
        .map_err(storage_error)?;
        tx.commit().map_err(storage_error)?;
        state.clear_generation = next_generation;
        state.lost_through_cursor = cleared_through_cursor;
        let accepted = ClearAccepted {
            request_id,
            clear_generation: next_generation,
            cleared_through_cursor,
            duplicate: false,
        };
        let notice = ClearNotice {
            request_id,
            clear_generation: next_generation,
            cleared_through_cursor,
        };
        for (current_session_id, session) in &mut state.sessions {
            session.queue.retain(|output| match output {
                SessionEvent::ResumeStarted(started) => started.clear_generation >= next_generation,
                SessionEvent::ResumeClip(clip) | SessionEvent::Live(clip) => {
                    clip.clear_generation >= next_generation
                }
                SessionEvent::ResumeComplete(complete) => {
                    complete.clear_generation >= next_generation
                }
                SessionEvent::PublishAccepted(_)
                | SessionEvent::ClearAccepted(_)
                | SessionEvent::ClearNotice(_) => true,
            });
            session
                .buffered
                .retain(|clip| clip.clear_generation >= next_generation);
            if *current_session_id == session_id {
                session
                    .queue
                    .push_back(SessionEvent::ClearAccepted(accepted.clone()));
            }
            session
                .queue
                .push_back(SessionEvent::ClearNotice(notice.clone()));
            session.phase = SessionPhase::Stale;
        }
        Ok(accepted)
    }
}

fn reject_symlink(path: &Path) -> Result<(), CoreError> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(CoreError::Failure(FailureCode::DatabaseIntegrityFailed));
    }
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| CoreError::Failure(FailureCode::StorageUnavailable))
}

#[cfg(not(unix))]
fn set_owner_only(_: &Path) -> Result<(), CoreError> {
    Ok(())
}

fn initialize_schema(connection: &mut Connection) -> Result<(), CoreError> {
    connection
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
        .map_err(storage_error)?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    tx.execute_batch(
        "CREATE TABLE hub_meta (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            history_epoch TEXT NOT NULL,
            clear_generation TEXT NOT NULL,
            cursor_high_water TEXT NOT NULL,
            lost_through_cursor TEXT
         );
         CREATE TABLE message_tombstones (
            message_id TEXT PRIMARY KEY,
            accepted_cursor TEXT NOT NULL,
            clear_generation TEXT NOT NULL
         );
         CREATE TABLE clips (
            cursor TEXT PRIMARY KEY,
            message_id TEXT NOT NULL UNIQUE,
            source_peer_id TEXT NOT NULL,
            clear_generation TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            accepted_at_ms INTEGER NOT NULL,
            expires_at_ms INTEGER NOT NULL,
            content BLOB NOT NULL
         );
         CREATE TABLE clear_receipts (
            request_id TEXT PRIMARY KEY,
            expected_generation TEXT NOT NULL,
            committed_generation TEXT NOT NULL,
            cleared_through_cursor TEXT
         );
         PRAGMA user_version=1;",
    )
    .map_err(storage_error)?;
    tx.execute(
        "INSERT INTO hub_meta(singleton, history_epoch, clear_generation, cursor_high_water, lost_through_cursor) VALUES (1, ?1, ?2, ?3, NULL)",
        params![Uuid::new_v4().to_string(), counter_key(1), counter_key(0)],
    )
    .map_err(storage_error)?;
    tx.commit().map_err(storage_error)
}

fn validate_existing_schema(path: &Path) -> Result<(), CoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| CoreError::Failure(FailureCode::DatabaseSchemaUnsupported))?;
    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| CoreError::Failure(FailureCode::DatabaseSchemaUnsupported))?;
    if version != SCHEMA_VERSION {
        return Err(CoreError::Failure(FailureCode::DatabaseSchemaUnsupported));
    }
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .map_err(|_| CoreError::Failure(FailureCode::DatabaseSchemaUnsupported))?;
    let tables: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .map_err(|_| CoreError::Failure(FailureCode::DatabaseSchemaUnsupported))?
        .collect::<Result<_, _>>()
        .map_err(|_| CoreError::Failure(FailureCode::DatabaseSchemaUnsupported))?;
    if tables != EXPECTED_TABLES {
        return Err(CoreError::Failure(FailureCode::DatabaseSchemaUnsupported));
    }
    for (table, expected) in EXPECTED_SCHEMA {
        let sql: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                params![table],
                |row| row.get(0),
            )
            .map_err(|_| CoreError::Failure(FailureCode::DatabaseSchemaUnsupported))?;
        if normalize_schema(&sql) != normalize_schema(expected) {
            return Err(CoreError::Failure(FailureCode::DatabaseSchemaUnsupported));
        }
    }
    let integrity: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|_| CoreError::Failure(FailureCode::DatabaseIntegrityFailed))?;
    if integrity != "ok" {
        return Err(CoreError::Failure(FailureCode::DatabaseIntegrityFailed));
    }
    Ok(())
}

fn normalize_schema(sql: &str) -> String {
    sql.chars()
        .filter(|character| {
            !character.is_ascii_whitespace() && *character != '"' && *character != '`'
        })
        .collect()
}

fn load_meta(connection: &Connection) -> Result<(Uuid, u64, u64, Option<u64>), CoreError> {
    connection
        .query_row(
            "SELECT history_epoch, clear_generation, cursor_high_water, lost_through_cursor FROM hub_meta WHERE singleton=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?)),
        )
        .map_err(|_| CoreError::Failure(FailureCode::DatabaseIntegrityFailed))
        .and_then(|(epoch, generation, cursor, lost)| {
            Ok((
                Uuid::parse_str(&epoch).map_err(|_| CoreError::Failure(FailureCode::DatabaseIntegrityFailed))?,
                parse_counter(&generation)?,
                parse_counter(&cursor)?,
                lost.map(|value| parse_counter(&value)).transpose()?,
            ))
        })
}

fn validate_resume_shape(
    epoch: Option<Uuid>,
    generation: Option<u64>,
    cursor: Option<u64>,
) -> Result<(), CoreError> {
    if cursor.is_some() && (epoch.is_none() || generation.is_none()) {
        return Err(CoreError::Failure(FailureCode::ResumeCursorWithoutContext));
    }
    if epoch.is_some() != generation.is_some() {
        return Err(CoreError::Failure(FailureCode::ResumeContextIncomplete));
    }
    Ok(())
}

fn require_active_session(state: &State, session_id: Uuid) -> Result<(), CoreError> {
    let session = state
        .sessions
        .get(&session_id)
        .ok_or(CoreError::Failure(FailureCode::SessionContextStale))?;
    if !matches!(session.phase, SessionPhase::Replaying | SessionPhase::Live)
        || session.clear_generation != state.clear_generation
    {
        return Err(CoreError::Failure(FailureCode::SessionContextStale));
    }
    Ok(())
}

fn require_session_exists(state: &State, session_id: Uuid) -> Result<(), CoreError> {
    state
        .sessions
        .contains_key(&session_id)
        .then_some(())
        .ok_or(CoreError::Failure(FailureCode::SessionContextStale))
}

fn require_live_session(state: &State, session_id: Uuid) -> Result<StablePeerId, CoreError> {
    let session = state
        .sessions
        .get(&session_id)
        .ok_or(CoreError::Failure(FailureCode::SessionContextStale))?;
    if session.phase != SessionPhase::Live || session.clear_generation != state.clear_generation {
        return Err(CoreError::Failure(FailureCode::SessionContextStale));
    }
    Ok(session.peer_id.clone())
}

fn compare_generation(input: u64, current: u64) -> Result<(), CoreError> {
    if input < current {
        Err(CoreError::Failure(FailureCode::ClearGenerationStale))
    } else if input > current {
        Err(CoreError::Failure(FailureCode::ClearGenerationAhead))
    } else {
        Ok(())
    }
}

fn persist_publish(
    state: &mut State,
    clip: &RetainedClip,
    limits: RetentionLimits,
    now_ms: i64,
) -> Result<(), CoreError> {
    let tx = state
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    tx.execute(
        "INSERT INTO clips(cursor, message_id, source_peer_id, clear_generation, created_at_ms, accepted_at_ms, expires_at_ms, content) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![counter_key(clip.cursor), clip.message_id.to_string(), clip.source_peer_id.storage_value(), counter_key(clip.clear_generation), clip.created_at_ms, clip.accepted_at_ms, clip.expires_at_ms, clip.content.as_storage_blob()],
    )
    .map_err(storage_error)?;
    tx.execute(
        "INSERT INTO message_tombstones(message_id, accepted_cursor, clear_generation) VALUES (?1, ?2, ?3)",
        params![clip.message_id.to_string(), counter_key(clip.cursor), counter_key(clip.clear_generation)],
    )
    .map_err(storage_error)?;
    let mut lost = state.lost_through_cursor;
    trim_transaction(&tx, limits, now_ms, &mut lost)?;
    tx.execute(
        "UPDATE hub_meta SET cursor_high_water=?1, lost_through_cursor=?2 WHERE singleton=1",
        params![counter_key(clip.cursor), lost.map(counter_key)],
    )
    .map_err(storage_error)?;
    tx.commit().map_err(storage_error)?;
    state.cursor_high_water = clip.cursor;
    state.lost_through_cursor = lost;
    Ok(())
}

fn apply_retention(
    state: &mut State,
    limits: RetentionLimits,
    now_ms: i64,
) -> Result<(), CoreError> {
    let tx = state
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    let mut lost = state.lost_through_cursor;
    trim_transaction(&tx, limits, now_ms, &mut lost)?;
    tx.execute(
        "UPDATE hub_meta SET lost_through_cursor=?1 WHERE singleton=1",
        params![lost.map(counter_key)],
    )
    .map_err(storage_error)?;
    tx.commit().map_err(storage_error)?;
    state.lost_through_cursor = lost;
    Ok(())
}

fn trim_transaction(
    tx: &rusqlite::Transaction<'_>,
    limits: RetentionLimits,
    now_ms: i64,
    lost: &mut Option<u64>,
) -> Result<(), CoreError> {
    let expired = selected_cursors(
        tx,
        "SELECT cursor FROM clips WHERE expires_at_ms <= ?1 ORDER BY cursor",
        params![now_ms],
    )?;
    delete_cursors(tx, &expired)?;
    update_lost(lost, expired.into_iter());
    let count: usize = tx
        .query_row("SELECT COUNT(*) FROM clips", [], |row| row.get(0))
        .map_err(storage_error)?;
    if count > limits.history_max_entries {
        let remove = count - limits.history_max_entries;
        let trimmed = selected_cursors(
            tx,
            "SELECT cursor FROM clips ORDER BY cursor LIMIT ?1",
            params![remove],
        )?;
        delete_cursors(tx, &trimmed)?;
        update_lost(lost, trimmed.into_iter());
    }
    Ok(())
}

fn selected_cursors<P: rusqlite::Params>(
    tx: &rusqlite::Transaction<'_>,
    sql: &str,
    params: P,
) -> Result<Vec<u64>, CoreError> {
    let mut statement = tx.prepare(sql).map_err(storage_error)?;
    let rows = statement
        .query_map(params, |row| row.get::<_, String>(0))
        .map_err(storage_error)?
        .map(|value| {
            value
                .map_err(storage_error)
                .and_then(|value| parse_counter(&value))
        })
        .collect();
    rows
}

fn delete_cursors(tx: &rusqlite::Transaction<'_>, cursors: &[u64]) -> Result<(), CoreError> {
    for cursor in cursors {
        tx.execute(
            "DELETE FROM clips WHERE cursor=?1",
            params![counter_key(*cursor)],
        )
        .map_err(storage_error)?;
    }
    Ok(())
}

fn update_lost(lost: &mut Option<u64>, cursors: impl Iterator<Item = u64>) {
    for cursor in cursors {
        *lost = Some(lost.unwrap_or(0).max(cursor));
    }
}

fn load_clip_by_message_id(
    connection: &Connection,
    message_id: Uuid,
    now_ms: i64,
) -> Result<Option<RetainedClip>, CoreError> {
    let row = connection
        .query_row(
            "SELECT cursor, message_id, source_peer_id, clear_generation, created_at_ms, accepted_at_ms, expires_at_ms, content FROM clips WHERE message_id=?1 AND expires_at_ms > ?2",
            params![message_id.to_string(), now_ms],
            clip_row,
        )
        .optional()
        .map_err(storage_error)?;
    row.map(decode_clip_row).transpose()
}

fn tombstone_exists(connection: &Connection, message_id: Uuid) -> Result<bool, CoreError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM message_tombstones WHERE message_id=?1)",
            params![message_id.to_string()],
            |row| row.get(0),
        )
        .map_err(storage_error)
}

type ClipRow = (String, String, String, String, i64, i64, i64, Vec<u8>);

fn clip_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClipRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn decode_clip_row(row: ClipRow) -> Result<RetainedClip, CoreError> {
    Ok(RetainedClip {
        cursor: parse_counter(&row.0)?,
        message_id: Uuid::parse_str(&row.1)
            .map_err(|_| CoreError::Failure(FailureCode::DatabaseIntegrityFailed))?,
        source_peer_id: StablePeerId::from_boundary(row.2)
            .map_err(|_| CoreError::Failure(FailureCode::DatabaseIntegrityFailed))?,
        clear_generation: parse_counter(&row.3)?,
        created_at_ms: row.4,
        accepted_at_ms: row.5,
        expires_at_ms: row.6,
        content: ClipContentV1::from_storage_blob(&row.7)?,
    })
}

fn load_clips(
    connection: &Connection,
    after: Option<u64>,
    boundary: Option<u64>,
    now_ms: i64,
) -> Result<Vec<RetainedClip>, CoreError> {
    let after = after.unwrap_or(0);
    let boundary = boundary.unwrap_or(0);
    let mut statement = connection
        .prepare("SELECT cursor, message_id, source_peer_id, clear_generation, created_at_ms, accepted_at_ms, expires_at_ms, content FROM clips WHERE cursor > ?1 AND cursor <= ?2 AND expires_at_ms > ?3 ORDER BY cursor")
        .map_err(storage_error)?;
    let clips = statement
        .query_map(
            params![counter_key(after), counter_key(boundary), now_ms],
            clip_row,
        )
        .map_err(storage_error)?
        .map(|row| row.map_err(storage_error).and_then(decode_clip_row))
        .collect();
    clips
}

fn load_clear_receipt(
    connection: &Connection,
    request_id: Uuid,
) -> Result<Option<(u64, u64, Option<u64>)>, CoreError> {
    connection
        .query_row(
            "SELECT expected_generation, committed_generation, cleared_through_cursor FROM clear_receipts WHERE request_id=?1",
            params![request_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)),
        )
        .optional()
        .map_err(storage_error)?
        .map(|(expected, committed, cleared)| Ok((parse_counter(&expected)?, parse_counter(&committed)?, cleared.map(|value| parse_counter(&value)).transpose()?)))
        .transpose()
}

fn enqueue_acceptance(state: &mut State, source_session_id: Uuid, accepted: PublishAccepted) {
    if let Some(session) = state.sessions.get_mut(&source_session_id) {
        session
            .queue
            .push_back(SessionEvent::PublishAccepted(accepted));
    }
}

fn enqueue_clear_acceptance(state: &mut State, session_id: Uuid, accepted: ClearAccepted) {
    if let Some(session) = state.sessions.get_mut(&session_id) {
        session
            .queue
            .push_back(SessionEvent::ClearAccepted(accepted));
    }
}

fn enqueue_clip(
    state: &mut State,
    source_session_id: Uuid,
    accepted: PublishAccepted,
    clip: RetainedClip,
) {
    enqueue_acceptance(state, source_session_id, accepted);
    for session in state.sessions.values_mut() {
        if session.clear_generation != clip.clear_generation {
            continue;
        }
        match session.phase {
            SessionPhase::Replaying => session.buffered.push(clip.clone()),
            SessionPhase::Live => {
                session.highest_offered_cursor = Some(clip.cursor);
                session.queue.push_back(SessionEvent::Live(clip.clone()));
            }
            SessionPhase::AwaitResume | SessionPhase::Stale => {}
        }
    }
}

fn counter_key(value: u64) -> String {
    format!("{value:020}")
}

fn parse_counter(value: &str) -> Result<u64, CoreError> {
    if value.len() != 20 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(CoreError::Failure(FailureCode::DatabaseIntegrityFailed));
    }
    value
        .parse()
        .map_err(|_| CoreError::Failure(FailureCode::DatabaseIntegrityFailed))
}

fn nonzero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn retention_ms(seconds: u64) -> Result<i64, CoreError> {
    i64::try_from(seconds.saturating_mul(1000))
        .map_err(|_| CoreError::Failure(FailureCode::ConfigValueInvalid))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn storage_error(_: rusqlite::Error) -> CoreError {
    CoreError::Failure(FailureCode::StorageUnavailable)
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, thread};

    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    fn peer(value: &str) -> StablePeerId {
        StablePeerId::from_boundary(value).unwrap()
    }

    fn content(value: &str, max: usize) -> ClipContentV1 {
        ClipContentV1::from_platform(value.as_bytes(), max).unwrap()
    }

    fn core(limits: RetentionLimits) -> (tempfile::TempDir, HubCore) {
        let directory = tempdir().unwrap();
        let core = HubCore::open(directory.path().join("hub.sqlite3"), limits).unwrap();
        (directory, core)
    }

    fn live(core: &HubCore, name: &str) -> SessionHello {
        let hello = core.open_session(peer(name));
        let plan = core
            .begin_resume(hello.session_id, None, None, None, NOW)
            .unwrap();
        assert_eq!(plan.status, ResumeStatus::Fresh);
        core.complete_resume(hello.session_id).unwrap();
        hello
    }

    fn publish_input(message_id: Uuid, generation: u64, text: &str) -> PublishInput {
        PublishInput {
            message_id,
            clear_generation: generation,
            created_at_ms: NOW,
            content: content(text, HARD_MAX_PAYLOAD_BYTES),
        }
    }

    fn consume_session_events(
        core: &HubCore,
        session_id: Uuid,
        mut inspect: impl FnMut(&SessionEvent),
    ) {
        while let Some(lease) = core.lease_next_session_event(session_id).unwrap() {
            inspect(lease.event());
            lease.complete();
        }
    }

    #[test]
    fn content_seam_owns_wire_storage_platform_preview_and_redaction() {
        let original = ClipContentV1::from_platform(b"a\x01\n  b", 64).unwrap();
        let wire = original.to_wire();
        let decoded = ClipContentV1::from_wire(
            wire.content_type,
            &wire.payload_b64,
            wire.payload_bytes,
            &wire.content_sha256,
            64,
        )
        .unwrap();
        let stored = ClipContentV1::from_storage_blob(decoded.as_storage_blob()).unwrap();
        assert_eq!(stored.to_platform(), b"a\x01\n  b");
        assert_eq!(stored.to_preview(4), "a� b");
        assert!(stored.same_content(&original));
        assert_eq!(format!("{stored:?}"), "ClipContentV1([redacted])");
        assert_eq!(format!("{wire:?}"), "WireContentV1([redacted])");
        assert!(!format!("{:?}", peer("peer-secret")).contains("peer-secret"));
    }

    #[test]
    fn schema_matrix_initializes_empty_opens_v1_unchanged_and_refuses_others_byte_identically() {
        let directory = tempdir().unwrap();
        let empty = directory.path().join("empty.sqlite3");
        fs::File::create(&empty).unwrap();
        drop(HubCore::open(&empty, RetentionLimits::default()).unwrap());
        let valid = directory.path().join("valid.sqlite3");
        drop(HubCore::open(&valid, RetentionLimits::default()).unwrap());
        let before = fs::read(&valid).unwrap();
        drop(HubCore::open(&valid, RetentionLimits::default()).unwrap());
        assert_eq!(fs::read(&valid).unwrap(), before);

        for (name, sql) in [
            ("zero.sqlite3", "CREATE TABLE devices(id TEXT);"),
            (
                "versioned.sqlite3",
                "PRAGMA user_version=2; CREATE TABLE old_state(id TEXT);",
            ),
            (
                "wrong-v1.sqlite3",
                "PRAGMA user_version=1; CREATE TABLE old_state(id TEXT);",
            ),
        ] {
            let path = directory.path().join(name);
            Connection::open(&path).unwrap().execute_batch(sql).unwrap();
            let before = fs::read(&path).unwrap();
            assert_eq!(
                HubCore::open(&path, RetentionLimits::default()).err(),
                Some(CoreError::Failure(FailureCode::DatabaseSchemaUnsupported))
            );
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn equal_members_publish_resume_ack_and_read_history() {
        let (_directory, core) = core(RetentionLimits::default());
        let left = live(&core, "peer-left");
        let right = live(&core, "peer-right");
        let accepted = core
            .publish(
                left.session_id,
                publish_input(Uuid::new_v4(), 1, "shared"),
                NOW,
            )
            .unwrap();
        assert_eq!(accepted.cursor, 1);
        let history = core.history(right.session_id, NOW).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].source_peer_id, peer("peer-left"));
        core.acknowledge(right.session_id, right.history_epoch, 1, 1)
            .unwrap();
    }

    #[test]
    fn retained_exact_retry_precedes_time_window_and_tombstone_replay() {
        let limits = RetentionLimits {
            retention_seconds: 60,
            ..RetentionLimits::default()
        };
        let (_directory, core) = core(limits);
        let source = live(&core, "peer-source");
        let message_id = Uuid::new_v4();
        let input = publish_input(message_id, 1, "once");
        let first = core.publish(source.session_id, input.clone(), NOW).unwrap();
        let retry = core.publish(source.session_id, input, NOW + 1).unwrap();
        assert_eq!(retry.cursor, first.cursor);
        assert!(retry.duplicate);
        let changed = publish_input(message_id, 1, "changed");
        assert_eq!(
            core.publish(source.session_id, changed, NOW + 1),
            Err(CoreError::Failure(FailureCode::MessageIdConflict))
        );
        core.history(source.session_id, NOW + 60_000).unwrap();
        assert_eq!(
            core.publish(
                source.session_id,
                publish_input(message_id, 1, "once"),
                NOW + 60_000
            ),
            Err(CoreError::Failure(FailureCode::MessageIdReplay))
        );
    }

    #[test]
    fn one_hundred_concurrent_retries_commit_once() {
        let (_directory, core) = core(RetentionLimits::default());
        let source = live(&core, "peer-race");
        let core = Arc::new(core);
        let message_id = Uuid::new_v4();
        let mut handles = Vec::new();
        for _ in 0..100 {
            let core = Arc::clone(&core);
            handles.push(thread::spawn(move || {
                core.publish(source.session_id, publish_input(message_id, 1, "race"), NOW)
                    .unwrap()
            }));
        }
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| !result.duplicate).count(), 1);
        assert!(results.iter().all(|result| result.cursor == 1));
        assert_eq!(core.history(source.session_id, NOW).unwrap().len(), 1);
    }

    #[test]
    fn count_and_age_retention_remove_oldest_and_report_gap() {
        let limits = RetentionLimits {
            retention_seconds: 60,
            history_max_entries: 2,
            max_payload_bytes: 1024,
        };
        let (_directory, core) = core(limits);
        let source = live(&core, "peer-retention");
        for index in 0..3 {
            core.publish(
                source.session_id,
                publish_input(Uuid::new_v4(), 1, &format!("clip-{index}")),
                NOW + index,
            )
            .unwrap();
        }
        let history = core.history(source.session_id, NOW + 3).unwrap();
        assert_eq!(
            history.iter().map(|clip| clip.cursor).collect::<Vec<_>>(),
            vec![2, 3]
        );
        let reconnect = core.open_session(peer("peer-retention"));
        let plan = core
            .begin_resume(
                reconnect.session_id,
                Some(reconnect.history_epoch),
                Some(1),
                Some(0),
                NOW + 3,
            )
            .unwrap();
        assert_eq!(plan.status, ResumeStatus::Gap);
        assert_eq!(plan.clips.len(), 2);
        assert!(core
            .history(source.session_id, NOW + 60_003)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn default_retention_keeps_the_newest_five_hundred_clips() {
        let limits = RetentionLimits::default();
        assert_eq!(limits.retention_seconds, 604_800);
        assert_eq!(limits.history_max_entries, 500);
        let (_directory, core) = core(limits);
        let source = live(&core, "peer-default-retention");
        for index in 0..501 {
            core.publish(
                source.session_id,
                publish_input(Uuid::new_v4(), 1, &format!("clip-{index}")),
                NOW + index,
            )
            .unwrap();
        }
        let history = core.history(source.session_id, NOW + 502).unwrap();
        assert_eq!(history.len(), 500);
        assert_eq!(history.first().unwrap().cursor, 2);
        assert_eq!(history.last().unwrap().cursor, 501);
    }

    #[test]
    fn resume_buffers_concurrent_live_clips_in_cursor_order() {
        let (_directory, core) = core(RetentionLimits::default());
        let source = live(&core, "peer-source");
        core.publish(
            source.session_id,
            publish_input(Uuid::new_v4(), 1, "resume"),
            NOW,
        )
        .unwrap();
        let target = core.open_session(peer("peer-target"));
        let plan = core
            .begin_resume(target.session_id, None, None, None, NOW)
            .unwrap();
        assert_eq!(plan.boundary_cursor, Some(1));
        core.publish(
            source.session_id,
            publish_input(Uuid::new_v4(), 1, "live-2"),
            NOW + 1,
        )
        .unwrap();
        core.publish(
            source.session_id,
            publish_input(Uuid::new_v4(), 1, "live-3"),
            NOW + 2,
        )
        .unwrap();
        core.complete_resume(target.session_id).unwrap();
        let state = core.state.lock().unwrap();
        let session = state.sessions.get(&target.session_id).unwrap();
        let cursors: Vec<_> = session
            .queue
            .iter()
            .filter_map(|output| match output {
                SessionEvent::Live(clip) => Some(clip.cursor),
                SessionEvent::ResumeStarted(_)
                | SessionEvent::ResumeClip(_)
                | SessionEvent::ResumeComplete(_)
                | SessionEvent::PublishAccepted(_)
                | SessionEvent::ClearAccepted(_)
                | SessionEvent::ClearNotice(_) => None,
            })
            .collect();
        assert_eq!(cursors, vec![2, 3]);
    }

    #[test]
    fn shared_clear_is_atomic_equal_member_idempotent_and_cuts_old_queues() {
        let (_directory, core) = core(RetentionLimits::default());
        let left = live(&core, "peer-left");
        let right = live(&core, "peer-right");
        core.publish(
            left.session_id,
            publish_input(Uuid::new_v4(), 1, "before-clear"),
            NOW,
        )
        .unwrap();
        let request_id = Uuid::new_v4();
        let cleared = core.clear_history(right.session_id, request_id, 1).unwrap();
        assert_eq!(cleared.clear_generation, 2);
        assert_eq!(cleared.cleared_through_cursor, Some(1));
        assert!(core
            .state
            .lock()
            .unwrap()
            .sessions
            .values()
            .all(|session| session
                .queue
                .iter()
                .all(|output| !matches!(output, SessionEvent::Live(_)))));
        let mut left_saw_notice = false;
        consume_session_events(&core, left.session_id, |event| {
            left_saw_notice |= matches!(
                event,
                SessionEvent::ClearNotice(notice)
                    if notice.request_id == request_id && notice.clear_generation == 2
            );
        });
        assert!(left_saw_notice);
        let mut right_saw_accepted = false;
        let mut right_saw_notice = false;
        consume_session_events(&core, right.session_id, |event| {
            right_saw_accepted |= matches!(
                event,
                SessionEvent::ClearAccepted(accepted)
                    if accepted.request_id == request_id && accepted.clear_generation == 2
            );
            right_saw_notice |= matches!(
                event,
                SessionEvent::ClearNotice(notice)
                    if notice.request_id == request_id && notice.clear_generation == 2
            );
        });
        assert!(right_saw_accepted);
        assert!(right_saw_notice);
        let retry_session = live(&core, "peer-left");
        let duplicate = core
            .clear_history(retry_session.session_id, request_id, 1)
            .unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.clear_generation, 2);
        assert_eq!(
            core.publish(
                retry_session.session_id,
                publish_input(Uuid::new_v4(), 1, "stale"),
                NOW + 1
            ),
            Err(CoreError::Failure(FailureCode::ClearGenerationStale))
        );
    }

    #[test]
    fn concurrent_clear_requests_have_one_winner() {
        let (_directory, core) = core(RetentionLimits::default());
        let left = live(&core, "peer-left");
        let right = live(&core, "peer-right");
        let core = Arc::new(core);
        let left_core = Arc::clone(&core);
        let right_core = Arc::clone(&core);
        let a = thread::spawn(move || left_core.clear_history(left.session_id, Uuid::new_v4(), 1));
        let b =
            thread::spawn(move || right_core.clear_history(right.session_id, Uuid::new_v4(), 1));
        let results = [a.join().unwrap(), b.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(results.iter().any(|result| matches!(
            result,
            Err(CoreError::Failure(FailureCode::ClearGenerationStale))
        )));
        assert_eq!(core.clear_generation(), 2);
    }

    #[test]
    fn periodic_retention_entry_deletes_expired_rows_without_an_operation_trigger() {
        let limits = RetentionLimits {
            retention_seconds: 60,
            ..RetentionLimits::default()
        };
        let (_directory, core) = core(limits);
        let source = live(&core, "peer-periodic-retention");
        core.publish(
            source.session_id,
            publish_input(Uuid::new_v4(), 1, "expires"),
            NOW,
        )
        .unwrap();
        core.run_periodic_retention(NOW + 60_000).unwrap();
        let retained: u64 = core
            .state
            .lock()
            .unwrap()
            .connection
            .query_row("SELECT COUNT(*) FROM clips", [], |row| row.get(0))
            .unwrap();
        assert_eq!(retained, 0);
    }

    #[test]
    fn sqlite_restart_preserves_epoch_generation_cursor_tombstones_receipts_and_content() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("hub.sqlite3");
        let core = HubCore::open(&path, RetentionLimits::default()).unwrap();
        let epoch = core.history_epoch();
        let source = live(&core, "peer-restart");
        let message_id = Uuid::new_v4();
        core.publish(
            source.session_id,
            publish_input(message_id, 1, "persistent"),
            NOW,
        )
        .unwrap();
        let request_id = Uuid::new_v4();
        core.clear_history(source.session_id, request_id, 1)
            .unwrap();
        drop(core);
        let core = HubCore::open(&path, RetentionLimits::default()).unwrap();
        assert_eq!(core.history_epoch(), epoch);
        assert_eq!(core.clear_generation(), 2);
        let source = live(&core, "peer-restart");
        assert!(
            core.clear_history(source.session_id, request_id, 1)
                .unwrap()
                .duplicate
        );
        assert_eq!(
            core.publish(
                source.session_id,
                publish_input(message_id, 2, "persistent"),
                NOW + 1
            ),
            Err(CoreError::Failure(FailureCode::MessageIdReplay))
        );
    }
}
