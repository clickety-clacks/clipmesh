//! Persistent, transport-neutral desktop-agent domain logic.
//!
//! Platform adapters and transports call this crate. It owns no operating-system
//! clipboard integration, listener, identity, enrollment, or deployment code.

mod store;

use std::{fmt, path::Path};

pub use clipmesh_hub_core::{ClipContentV1, WireContentV1};
use store::StateStore;
use thiserror::Error;
use uuid::Uuid;

const HARD_MAX_PAYLOAD_BYTES: usize = 1_048_576;
const OUTBOX_MAX_EVENTS: usize = 20;
const OUTBOX_MAX_BYTES: usize = 1_048_576;
const ACK_INTERVAL_MS: i64 = 2_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishEventV1 {
    pub message_id: Uuid,
    pub clear_generation: u64,
    pub created_at_ms: i64,
    pub content: ClipContentV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    Resume,
    Live,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentState {
    StartingUnknownLock,
    ActiveUnlockedConnecting,
    ActiveUnlockedLive,
    Locked,
    LocallyPaused,
    OutboxFull,
    AdapterFailed,
    Stopping,
}

#[derive(Clone, Eq, PartialEq)]
pub struct SessionParameters {
    pub self_peer_id: String,
    pub history_epoch: Uuid,
    pub clear_generation: u64,
    pub max_payload_bytes: usize,
    pub retention_seconds: u64,
    pub server_time_offset_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HintClassification {
    Ordinary,
    Confidential,
    Transient,
}

#[derive(Clone, Eq, PartialEq)]
pub struct PlatformRevision(String);

impl PlatformRevision {
    pub fn synthetic(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn storage_value(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_storage(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Debug for PlatformRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PlatformRevision([redacted])")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct LocalObservation {
    pub bytes: Vec<u8>,
    pub revision: PlatformRevision,
    pub hint: HintClassification,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ObservationToken {
    state_generation: u64,
    observation: LocalObservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationSuppression {
    StateChanged,
    StaleNotification,
    ExplicitHint,
    LocalOnly,
    RemoteWriteLoop,
    InvalidPayload,
    OutboxFull,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservationResult {
    Suppressed(ObservationSuppression),
    Queued(OutboxItem),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboxItem {
    pub event: PublishEventV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoopMarker {
    pub message_id: Uuid,
    pub content: ClipContentV1,
    pub revision: PlatformRevision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentSnapshot {
    pub history_epoch: Option<Uuid>,
    pub clear_generation: Option<u64>,
    pub last_cursor: Option<u64>,
    pub outbox: Vec<OutboxItem>,
    pub loop_marker: Option<LoopMarker>,
    pub processed_message_count: usize,
    pub history_count: usize,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ReceivedEvent {
    pub history_epoch: Uuid,
    pub clear_generation: u64,
    pub cursor: u64,
    pub delivery: Delivery,
    pub accepted_at_ms: i64,
    pub expires_at_ms: i64,
    pub source_peer_id: String,
    pub message_id: Uuid,
    pub created_at_ms: i64,
    pub content_type: String,
    pub payload_b64: String,
    pub payload_bytes: usize,
    pub content_sha256: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveResult {
    RecordedOnly,
    Applied,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AckCursor {
    pub history_epoch: Uuid,
    pub clear_generation: u64,
    pub cursor: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalControl {
    Status,
    Pause,
    Resume,
    ClearLocalHistory,
    LocalOnlyNext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermanentPublishFailure {
    Validation,
    Replay,
    StaleGeneration,
}

impl PermanentPublishFailure {
    fn code(self) -> &'static str {
        match self {
            Self::Validation => "publish_validation_failed",
            Self::Replay => "message_id_replay",
            Self::StaleGeneration => "clear_generation_stale",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Status {
    pub state: AgentState,
    pub outbox_events: usize,
    pub hinted_suppressions: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReconnectBackoff {
    attempt: u32,
    live_since_ms: Option<i64>,
}

impl ReconnectBackoff {
    pub fn next_delay_ms(&mut self, random_sample: u64) -> u64 {
        let delay = AgentCore::reconnect_delay_ms(self.attempt, random_sample);
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    pub fn entered_live(&mut self, now_ms: i64) {
        self.live_since_ms = Some(now_ms);
    }

    pub fn disconnected(&mut self, now_ms: i64) {
        if self
            .live_since_ms
            .is_some_and(|started| now_ms.saturating_sub(started) >= 30_000)
        {
            self.attempt = 0;
        }
        self.live_since_ms = None;
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}

pub trait ClipboardAdapter {
    fn is_current(&mut self, revision: &PlatformRevision) -> Result<bool, AdapterError>;
    fn write_text(&mut self, bytes: &[u8]) -> Result<PlatformRevision, AdapterError>;
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("adapter_unavailable")]
pub struct AdapterError;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CoreError {
    #[error("state_path_insecure")]
    StatePathInsecure,
    #[error("local_state_unavailable")]
    LocalStateUnavailable,
    #[error("session_epoch_stale")]
    SessionEpochStale,
    #[error("clear_generation_stale")]
    ClearGenerationStale,
    #[error("cursor_ahead")]
    CursorOrderInvalid,
    #[error("protocol_schema_invalid")]
    InvalidEvent,
    #[error("adapter_unavailable")]
    AdapterUnavailable,
    #[error("protocol_schema_invalid")]
    InvalidTransition,
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
}

pub struct AgentCore {
    store: StateStore,
    state: AgentState,
    state_generation: u64,
    session: Option<SessionParameters>,
    local_only_next: bool,
    pending_ack: Option<AckCursor>,
    last_ack_sent_at_ms: Option<i64>,
    hinted_suppressions: u64,
}

impl AgentCore {
    /// Opens version 1 state or initializes it when the database is absent.
    pub fn open(path: &Path) -> Result<Self, CoreError> {
        let mut store = StateStore::open_or_initialize(path)?;
        store.clear_loop_marker()?;
        Ok(Self {
            store,
            state: AgentState::StartingUnknownLock,
            state_generation: 0,
            session: None,
            local_only_next: false,
            pending_ack: None,
            last_ack_sent_at_ms: None,
            hinted_suppressions: 0,
        })
    }

    pub fn state(&self) -> AgentState {
        self.state
    }

    pub fn snapshot(&self) -> Result<PersistentSnapshot, CoreError> {
        self.store.snapshot()
    }

    pub fn start_unlocked(&mut self) {
        if self.state == AgentState::StartingUnknownLock {
            self.transition(AgentState::ActiveUnlockedConnecting);
        }
    }

    pub fn set_session(&mut self, parameters: SessionParameters) -> Result<(), CoreError> {
        if self.state != AgentState::ActiveUnlockedConnecting {
            return Err(CoreError::InvalidTransition);
        }
        if parameters.self_peer_id.is_empty()
            || parameters.history_epoch.get_version_num() != 4
            || parameters.max_payload_bytes == 0
            || parameters.max_payload_bytes > HARD_MAX_PAYLOAD_BYTES
            || parameters.retention_seconds == 0
        {
            return Err(CoreError::InvalidEvent);
        }
        let snapshot = self.store.snapshot()?;
        match snapshot.clear_generation {
            Some(generation) if parameters.clear_generation < generation => {
                return Err(CoreError::ClearGenerationStale);
            }
            Some(generation) if parameters.clear_generation > generation => {
                self.store.apply_generation_change(
                    &parameters.history_epoch,
                    parameters.clear_generation,
                    None,
                )?
            }
            Some(_) if snapshot.history_epoch != Some(parameters.history_epoch) => self
                .store
                .apply_epoch_change(&parameters.history_epoch, parameters.clear_generation)?,
            _ => self
                .store
                .establish_context(&parameters.history_epoch, parameters.clear_generation)?,
        }
        self.session = Some(parameters);
        self.last_ack_sent_at_ms = None;
        Ok(())
    }

    pub fn finish_resume(&mut self, now_ms: i64) -> Option<AckCursor> {
        if self.state != AgentState::ActiveUnlockedConnecting || self.session.is_none() {
            return None;
        }
        self.state = AgentState::ActiveUnlockedLive;
        self.take_ack(now_ms, true)
    }

    pub fn disconnect(&mut self) {
        self.session = None;
        self.pending_ack = None;
        if matches!(
            self.state,
            AgentState::ActiveUnlockedLive | AgentState::OutboxFull
        ) {
            self.transition(AgentState::ActiveUnlockedConnecting);
        }
    }

    pub fn set_locked(&mut self, locked: bool) {
        if locked {
            self.session = None;
            self.pending_ack = None;
            self.transition(AgentState::Locked);
        } else if self.state == AgentState::Locked {
            self.transition(AgentState::ActiveUnlockedConnecting);
        }
    }

    pub fn adapter_failed(&mut self) {
        self.session = None;
        self.pending_ack = None;
        self.transition(AgentState::AdapterFailed);
    }

    pub fn stop(&mut self) {
        self.session = None;
        self.pending_ack = None;
        self.transition(AgentState::Stopping);
    }

    pub fn begin_observation(&self, observation: LocalObservation) -> Option<ObservationToken> {
        (self.state == AgentState::ActiveUnlockedLive).then_some(ObservationToken {
            state_generation: self.state_generation,
            observation,
        })
    }

    pub fn commit_observation<A: ClipboardAdapter>(
        &mut self,
        token: ObservationToken,
        local_utc_ms: i64,
        adapter: &mut A,
    ) -> Result<ObservationResult, CoreError> {
        if token.state_generation != self.state_generation
            || self.state != AgentState::ActiveUnlockedLive
        {
            return Ok(ObservationResult::Suppressed(
                ObservationSuppression::StateChanged,
            ));
        }
        if !adapter
            .is_current(&token.observation.revision)
            .map_err(|_| CoreError::AdapterUnavailable)?
        {
            return Ok(ObservationResult::Suppressed(
                ObservationSuppression::StaleNotification,
            ));
        }
        if token.observation.hint != HintClassification::Ordinary {
            self.hinted_suppressions = self.hinted_suppressions.saturating_add(1);
            return Ok(ObservationResult::Suppressed(
                ObservationSuppression::ExplicitHint,
            ));
        }
        if self.local_only_next {
            self.local_only_next = false;
            return Ok(ObservationResult::Suppressed(
                ObservationSuppression::LocalOnly,
            ));
        }

        let session = self.session.as_ref().ok_or(CoreError::InvalidTransition)?;
        let content =
            match ClipContentV1::from_platform(&token.observation.bytes, session.max_payload_bytes)
                .map_err(map_content_error)
            {
                Ok(content) => content,
                Err(
                    CoreError::PayloadEmpty
                    | CoreError::PayloadEncodingInvalid
                    | CoreError::PayloadTooLarge,
                ) => {
                    return Ok(ObservationResult::Suppressed(
                        ObservationSuppression::InvalidPayload,
                    ));
                }
                Err(error) => return Err(error),
            };

        if let Some(marker) = self.store.snapshot()?.loop_marker {
            if token.observation.revision == marker.revision {
                if content.same_content(&marker.content) {
                    return Ok(ObservationResult::Suppressed(
                        ObservationSuppression::RemoteWriteLoop,
                    ));
                }
                self.adapter_failed();
                return Err(CoreError::AdapterUnavailable);
            }
            self.store.clear_loop_marker()?;
        }

        let created_at_ms = local_utc_ms.saturating_add(session.server_time_offset_ms);
        self.store.remove_stale_outbox(
            created_at_ms,
            session.retention_seconds,
            session.clear_generation,
        )?;
        let (count, bytes) = self.store.outbox_usage()?;
        if count >= OUTBOX_MAX_EVENTS
            || bytes.saturating_add(content.as_storage_blob().len()) > OUTBOX_MAX_BYTES
        {
            self.state = AgentState::OutboxFull;
            return Ok(ObservationResult::Suppressed(
                ObservationSuppression::OutboxFull,
            ));
        }

        let event = PublishEventV1 {
            message_id: Uuid::new_v4(),
            clear_generation: session.clear_generation,
            created_at_ms,
            content,
        };
        self.store.insert_outbox(&event)?;
        Ok(ObservationResult::Queued(OutboxItem { event }))
    }

    pub fn outbox_for_retry(&self) -> Result<Vec<OutboxItem>, CoreError> {
        if !matches!(
            self.state,
            AgentState::ActiveUnlockedLive | AgentState::OutboxFull
        ) {
            return Ok(Vec::new());
        }
        self.store.outbox_items()
    }

    pub fn publish_accepted(&mut self, message_id: Uuid) -> Result<(), CoreError> {
        self.store.remove_outbox(message_id)?;
        self.leave_outbox_full_if_possible()
    }

    pub fn publish_rejected(
        &mut self,
        message_id: Uuid,
        failure: PermanentPublishFailure,
        retryable: bool,
    ) -> Result<(), CoreError> {
        if !retryable {
            self.store
                .record_publish_failure(message_id, failure.code())?;
            self.leave_outbox_full_if_possible()?;
        }
        Ok(())
    }

    pub fn receive_event<A: ClipboardAdapter>(
        &mut self,
        received: ReceivedEvent,
        local_utc_ms: i64,
        adapter: &mut A,
    ) -> Result<ReceiveResult, CoreError> {
        let session = self.session.as_ref().ok_or(CoreError::InvalidTransition)?;
        if received.history_epoch != session.history_epoch {
            return Err(CoreError::SessionEpochStale);
        }
        if received.clear_generation != session.clear_generation {
            return Err(CoreError::ClearGenerationStale);
        }
        if received.cursor == 0
            || received.message_id.get_version_num() != 4
            || received.source_peer_id.is_empty()
            || received.expires_at_ms <= local_utc_ms
        {
            return Err(CoreError::InvalidEvent);
        }
        let valid_state = matches!(
            (self.state, received.delivery),
            (AgentState::ActiveUnlockedConnecting, Delivery::Resume)
                | (AgentState::ActiveUnlockedLive, Delivery::Live)
                | (AgentState::OutboxFull, Delivery::Live)
        );
        if !valid_state {
            return Err(CoreError::InvalidTransition);
        }
        let snapshot = self.store.snapshot()?;
        if snapshot
            .last_cursor
            .is_some_and(|cursor| received.cursor <= cursor)
        {
            return Err(CoreError::CursorOrderInvalid);
        }
        let content = ClipContentV1::from_wire(
            &received.content_type,
            &received.payload_b64,
            received.payload_bytes,
            &received.content_sha256,
            session.max_payload_bytes,
        )
        .map_err(map_content_error)?;
        let already_processed = self.store.has_processed(received.message_id)?;
        let apply = !already_processed
            && received.delivery == Delivery::Live
            && received.source_peer_id != session.self_peer_id
            && self.state == AgentState::ActiveUnlockedLive;

        let marker = if apply {
            let revision = adapter
                .write_text(content.to_platform())
                .map_err(|_| CoreError::AdapterUnavailable)?;
            Some(LoopMarker {
                message_id: received.message_id,
                content: content.clone(),
                revision,
            })
        } else {
            None
        };
        self.store
            .record_received(&received, &content, marker.as_ref())?;
        self.pending_ack = Some(AckCursor {
            history_epoch: received.history_epoch,
            clear_generation: received.clear_generation,
            cursor: received.cursor,
        });
        Ok(if apply {
            ReceiveResult::Applied
        } else {
            ReceiveResult::RecordedOnly
        })
    }

    pub fn poll_ack(&mut self, now_ms: i64) -> Option<AckCursor> {
        self.take_ack(now_ms, false)
    }

    pub fn clear_notice(
        &mut self,
        history_epoch: Uuid,
        clear_generation: u64,
        cleared_through_cursor: Option<u64>,
    ) -> Result<(), CoreError> {
        let current = self
            .store
            .snapshot()?
            .clear_generation
            .ok_or(CoreError::InvalidTransition)?;
        if clear_generation <= current {
            return Err(CoreError::ClearGenerationStale);
        }
        self.store.apply_generation_change(
            &history_epoch,
            clear_generation,
            cleared_through_cursor,
        )?;
        if let Some(session) = self.session.as_mut() {
            session.history_epoch = history_epoch;
            session.clear_generation = clear_generation;
        }
        self.pending_ack = None;
        self.state_generation = self.state_generation.wrapping_add(1);
        Ok(())
    }

    pub fn local_control(&mut self, control: LocalControl) -> Result<Status, CoreError> {
        match control {
            LocalControl::Status => {}
            LocalControl::Pause => {
                self.session = None;
                self.pending_ack = None;
                self.transition(AgentState::LocallyPaused);
            }
            LocalControl::Resume => {
                if self.state == AgentState::LocallyPaused {
                    self.transition(AgentState::ActiveUnlockedConnecting);
                }
            }
            LocalControl::ClearLocalHistory => self.store.clear_local_history()?,
            LocalControl::LocalOnlyNext => self.local_only_next = true,
        }
        self.status()
    }

    pub fn status(&self) -> Result<Status, CoreError> {
        Ok(Status {
            state: self.state,
            outbox_events: self.store.outbox_usage()?.0,
            hinted_suppressions: self.hinted_suppressions,
        })
    }

    pub fn reconnect_delay_ms(attempt: u32, random_sample: u64) -> u64 {
        let exponential = 500_u64.checked_shl(attempt.min(63)).unwrap_or(u64::MAX);
        let ceiling = exponential.min(30_000);
        random_sample % (ceiling + 1)
    }

    fn transition(&mut self, state: AgentState) {
        self.state = state;
        self.state_generation = self.state_generation.wrapping_add(1);
    }

    fn leave_outbox_full_if_possible(&mut self) -> Result<(), CoreError> {
        let (count, bytes) = self.store.outbox_usage()?;
        if self.state == AgentState::OutboxFull
            && count < OUTBOX_MAX_EVENTS
            && bytes < OUTBOX_MAX_BYTES
        {
            self.state = if self.session.is_some() {
                AgentState::ActiveUnlockedLive
            } else {
                AgentState::ActiveUnlockedConnecting
            };
        }
        Ok(())
    }

    fn take_ack(&mut self, now_ms: i64, force: bool) -> Option<AckCursor> {
        let due = force
            || self
                .last_ack_sent_at_ms
                .is_none_or(|sent_at| now_ms.saturating_sub(sent_at) >= ACK_INTERVAL_MS);
        if !due {
            return None;
        }
        let acknowledgement = self.pending_ack.take()?;
        self.last_ack_sent_at_ms = Some(now_ms);
        Some(acknowledgement)
    }
}

fn map_content_error(error: clipmesh_hub_core::CoreError) -> CoreError {
    use clipmesh_hub_core::FailureCode;

    match error {
        clipmesh_hub_core::CoreError::Failure(code) => match code {
            FailureCode::ContentTypeUnsupported => CoreError::ContentTypeUnsupported,
            FailureCode::PayloadEncodingInvalid => CoreError::PayloadEncodingInvalid,
            FailureCode::PayloadEmpty => CoreError::PayloadEmpty,
            FailureCode::PayloadTooLarge => CoreError::PayloadTooLarge,
            FailureCode::PayloadLengthMismatch => CoreError::PayloadLengthMismatch,
            FailureCode::PayloadHashMismatch => CoreError::PayloadHashMismatch,
            _ => CoreError::InvalidEvent,
        },
    }
}
