//! Persistent desktop-agent domain logic.
//!
//! Platform adapters and transports call this crate. It owns no operating-system
//! clipboard integration, listener, credential, enrollment, or deployment code.

mod store;

use std::path::Path;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use clipmesh_protocol::{ClipboardEventV1, Delivery, FailureCode, LimitsV1, U64Decimal, UuidV4};
use sha2::{Digest, Sha256};
use store::StateStore;
use thiserror::Error;
use uuid::Uuid;

const OUTBOX_MAX_EVENTS: usize = 20;
const OUTBOX_MAX_BYTES: usize = 1_048_576;
const ACK_INTERVAL_MS: i64 = 2_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentState {
    StartingUnknownLock,
    ActiveUnlockedConnecting,
    ActiveUnlockedLive,
    Locked,
    LocallyPaused,
    AdministrativelyPaused,
    OutboxFull,
    AdapterFailed,
    Stopping,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionParameters {
    pub history_epoch: UuidV4,
    pub limits: LimitsV1,
    pub server_time_offset_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalObservation {
    pub bytes: Vec<u8>,
    pub sensitive: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationToken {
    generation: u64,
    observation: LocalObservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationSuppression {
    Inactive,
    StateChanged,
    Sensitive,
    LocalOnly,
    ConsecutiveDuplicate,
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
    pub event: ClipboardEventV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoopMarker {
    pub message_id: UuidV4,
    pub content_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentSnapshot {
    pub highest_source_seq: u64,
    pub history_epoch: Option<UuidV4>,
    pub last_cursor: Option<U64Decimal>,
    pub outbox: Vec<OutboxItem>,
    pub loop_marker: Option<LoopMarker>,
    pub processed_message_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceivedEvent {
    pub history_epoch: UuidV4,
    pub cursor: U64Decimal,
    pub delivery: Delivery,
    pub event: ClipboardEventV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveResult {
    RecordedOnly,
    ClipboardAlreadyEqual,
    Applied,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AckCursor {
    pub history_epoch: UuidV4,
    pub cursor: U64Decimal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalControl {
    Status,
    Pause,
    Resume,
    ClearLocalCache,
    LocalOnlyNext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Status {
    pub state: AgentState,
    pub outbox_events: usize,
    pub sensitive_suppressions: u64,
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
    fn current_bytes(&mut self) -> Result<Vec<u8>, FailureCode>;
    fn write_text(&mut self, text: &str) -> Result<(), FailureCode>;
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CoreError {
    #[error("local_state_unavailable")]
    LocalStateUnavailable,
    #[error("device_sequence_exhausted")]
    DeviceSequenceExhausted,
    #[error("session_epoch_stale")]
    SessionEpochStale,
    #[error("cursor_ahead")]
    CursorOrderInvalid,
    #[error("protocol_schema_invalid")]
    InvalidEvent,
    #[error("adapter_unavailable")]
    AdapterUnavailable,
    #[error("protocol_schema_invalid")]
    InvalidTransition,
}

pub struct AgentCore {
    store: StateStore,
    device_id: UuidV4,
    state: AgentState,
    generation: u64,
    session: Option<SessionParameters>,
    local_only_next: bool,
    prior_local_hash: Option<String>,
    pending_ack: Option<AckCursor>,
    last_ack_sent_at_ms: Option<i64>,
    sensitive_suppressions: u64,
}

impl AgentCore {
    /// Creates a new state store during explicit device initialization.
    pub fn initialize(path: &Path, device_id: UuidV4) -> Result<Self, CoreError> {
        Self::with_store(StateStore::initialize(path)?, device_id)
    }

    /// Opens an existing enrolled device. A missing store is a loud failure.
    pub fn open(path: &Path, device_id: UuidV4) -> Result<Self, CoreError> {
        Self::with_store(StateStore::open(path)?, device_id)
    }

    fn with_store(store: StateStore, device_id: UuidV4) -> Result<Self, CoreError> {
        Ok(Self {
            store,
            device_id,
            state: AgentState::StartingUnknownLock,
            generation: 0,
            session: None,
            local_only_next: false,
            prior_local_hash: None,
            pending_ack: None,
            last_ack_sent_at_ms: None,
            sensitive_suppressions: 0,
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
            self.transition(AgentState::ActiveUnlockedConnecting, true);
        }
    }

    pub fn set_session(&mut self, parameters: SessionParameters) -> Result<(), CoreError> {
        if !matches!(
            self.state,
            AgentState::ActiveUnlockedConnecting | AgentState::AdministrativelyPaused
        ) {
            return Err(CoreError::InvalidTransition);
        }
        self.session = Some(parameters);
        self.state = AgentState::ActiveUnlockedConnecting;
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
            self.transition(AgentState::ActiveUnlockedConnecting, true);
        }
    }

    pub fn administratively_paused(&mut self) {
        self.session = None;
        self.pending_ack = None;
        self.transition(AgentState::AdministrativelyPaused, true);
    }

    pub fn set_locked(&mut self, locked: bool) {
        if locked {
            self.session = None;
            self.pending_ack = None;
            self.transition(AgentState::Locked, true);
        } else if self.state == AgentState::Locked {
            self.transition(AgentState::ActiveUnlockedConnecting, true);
        }
    }

    pub fn adapter_failed(&mut self) {
        self.session = None;
        self.pending_ack = None;
        self.transition(AgentState::AdapterFailed, true);
    }

    pub fn stop(&mut self) {
        self.session = None;
        self.pending_ack = None;
        self.transition(AgentState::Stopping, true);
    }

    pub fn begin_observation(&self, observation: LocalObservation) -> Option<ObservationToken> {
        (self.state == AgentState::ActiveUnlockedLive).then_some(ObservationToken {
            generation: self.generation,
            observation,
        })
    }

    pub fn commit_observation(
        &mut self,
        token: ObservationToken,
        local_utc_ms: i64,
    ) -> Result<ObservationResult, CoreError> {
        if token.generation != self.generation || self.state != AgentState::ActiveUnlockedLive {
            return Ok(ObservationResult::Suppressed(
                ObservationSuppression::StateChanged,
            ));
        }
        let session = self.session.as_ref().ok_or(CoreError::InvalidEvent)?;
        if token.observation.sensitive {
            self.sensitive_suppressions = self.sensitive_suppressions.saturating_add(1);
            return Ok(ObservationResult::Suppressed(
                ObservationSuppression::Sensitive,
            ));
        }
        if self.local_only_next {
            self.local_only_next = false;
            return Ok(ObservationResult::Suppressed(
                ObservationSuppression::LocalOnly,
            ));
        }

        let content_hash = sha256_hex(&token.observation.bytes);
        if self.prior_local_hash.as_deref() == Some(content_hash.as_str()) {
            return Ok(ObservationResult::Suppressed(
                ObservationSuppression::ConsecutiveDuplicate,
            ));
        }
        self.prior_local_hash = Some(content_hash.clone());

        if let Some(marker) = self.store.snapshot()?.loop_marker {
            self.store.clear_loop_marker()?;
            if marker.content_sha256 == content_hash {
                return Ok(ObservationResult::Suppressed(
                    ObservationSuppression::RemoteWriteLoop,
                ));
            }
        }

        if token.observation.bytes.is_empty()
            || std::str::from_utf8(&token.observation.bytes).is_err()
            || token.observation.bytes.len() > session.limits.max_payload_bytes as usize
        {
            return Ok(ObservationResult::Suppressed(
                ObservationSuppression::InvalidPayload,
            ));
        }

        let created_at_ms = local_utc_ms.saturating_add(session.server_time_offset_ms);
        self.store.remove_expired(created_at_ms)?;
        let (count, bytes) = self.store.outbox_usage()?;
        if count >= OUTBOX_MAX_EVENTS
            || bytes.saturating_add(token.observation.bytes.len()) > OUTBOX_MAX_BYTES
        {
            self.state = AgentState::OutboxFull;
            return Ok(ObservationResult::Suppressed(
                ObservationSuppression::OutboxFull,
            ));
        }

        let source_seq = self
            .store
            .snapshot()?
            .highest_source_seq
            .checked_add(1)
            .ok_or(CoreError::DeviceSequenceExhausted)?;
        let event = build_event(
            &self.device_id,
            source_seq,
            created_at_ms,
            session.limits.retention_seconds,
            &token.observation.bytes,
            &content_hash,
        )?;
        self.store.insert_outbox(source_seq, &event)?;
        Ok(ObservationResult::Queued(OutboxItem { event }))
    }

    pub fn outbox_for_retry(&self) -> Result<Vec<OutboxItem>, CoreError> {
        self.store.outbox_items()
    }

    pub fn publish_accepted(&mut self, message_id: &UuidV4) -> Result<(), CoreError> {
        self.store.remove_outbox(message_id)?;
        self.leave_outbox_full_if_possible()?;
        Ok(())
    }

    pub fn expire_outbox(&mut self, now_ms: i64) -> Result<usize, CoreError> {
        let expired = self.store.remove_expired(now_ms)?;
        self.leave_outbox_full_if_possible()?;
        Ok(expired)
    }

    pub fn publish_rejected(
        &mut self,
        message_id: &UuidV4,
        code: FailureCode,
    ) -> Result<(), CoreError> {
        if !code.retryable() {
            self.store.remove_outbox(message_id)?;
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
        let session = self.session.as_ref().ok_or(CoreError::SessionEpochStale)?;
        if received.history_epoch != session.history_epoch {
            return Err(CoreError::SessionEpochStale);
        }
        let delivery_is_valid = matches!(
            (self.state, &received.delivery),
            (AgentState::ActiveUnlockedConnecting, Delivery::Resume)
                | (AgentState::ActiveUnlockedLive, Delivery::Live)
                | (AgentState::OutboxFull, Delivery::Live)
        );
        if !delivery_is_valid {
            return Err(CoreError::InvalidTransition);
        }
        let snapshot = self.store.snapshot()?;
        if let Some(cursor) = snapshot.last_cursor {
            if received.cursor <= cursor {
                return Err(CoreError::CursorOrderInvalid);
            }
        }
        received
            .event
            .validate(
                local_utc_ms.saturating_add(session.server_time_offset_ms),
                session.limits.max_payload_bytes,
                session.limits.retention_seconds,
            )
            .map_err(|_| CoreError::InvalidEvent)?;
        let already_processed = self.store.has_processed(&received.event.message_id)?;
        let should_apply = received.delivery == Delivery::Live
            && received.event.source_device_id != self.device_id
            && !already_processed
            && self.state == AgentState::ActiveUnlockedLive;

        let mut result = ReceiveResult::RecordedOnly;
        let mut marker = None;
        if should_apply {
            let bytes = received
                .event
                .payload_b64
                .decode_wire_bytes()
                .map_err(|_| CoreError::InvalidEvent)?;
            let current = adapter
                .current_bytes()
                .map_err(|_| CoreError::AdapterUnavailable)?;
            if current == bytes {
                result = ReceiveResult::ClipboardAlreadyEqual;
            } else {
                let text = std::str::from_utf8(&bytes).map_err(|_| CoreError::InvalidEvent)?;
                adapter
                    .write_text(text)
                    .map_err(|_| CoreError::AdapterUnavailable)?;
                marker = Some(LoopMarker {
                    message_id: clone_uuid(&received.event.message_id)?,
                    content_sha256: sha256_hex(&bytes),
                });
                result = ReceiveResult::Applied;
            }
        }
        self.store.record_received(
            &received.history_epoch,
            received.cursor,
            &received.event.message_id,
            marker.as_ref(),
        )?;
        self.pending_ack = Some(AckCursor {
            history_epoch: received.history_epoch,
            cursor: received.cursor,
        });
        Ok(result)
    }

    pub fn poll_ack(&mut self, now_ms: i64) -> Option<AckCursor> {
        self.take_ack(now_ms, false)
    }

    pub fn purge_notice(
        &mut self,
        history_epoch: &UuidV4,
        purged_through_cursor: Option<U64Decimal>,
    ) -> Result<(), CoreError> {
        self.generation = self.generation.wrapping_add(1);
        self.store
            .apply_purge(history_epoch, purged_through_cursor)?;
        if let Some(session) = self.session.as_mut() {
            session.history_epoch = clone_uuid(history_epoch)?;
        }
        self.prior_local_hash = None;
        self.pending_ack = None;
        Ok(())
    }

    pub fn local_control(&mut self, control: LocalControl) -> Result<Status, CoreError> {
        match control {
            LocalControl::Status => {}
            LocalControl::Pause => {
                self.session = None;
                self.pending_ack = None;
                self.transition(AgentState::LocallyPaused, true);
            }
            LocalControl::Resume => {
                if self.state == AgentState::LocallyPaused {
                    self.transition(AgentState::ActiveUnlockedConnecting, true);
                }
            }
            LocalControl::ClearLocalCache => {
                self.generation = self.generation.wrapping_add(1);
                self.store.clear_local_cache()?;
                self.prior_local_hash = None;
                self.pending_ack = None;
            }
            LocalControl::LocalOnlyNext => self.local_only_next = true,
        }
        self.status()
    }

    pub fn status(&self) -> Result<Status, CoreError> {
        Ok(Status {
            state: self.state,
            outbox_events: self.store.outbox_usage()?.0,
            sensitive_suppressions: self.sensitive_suppressions,
        })
    }

    pub fn reconnect_delay_ms(attempt: u32, random_sample: u64) -> u64 {
        let exponential = 500_u64.checked_shl(attempt.min(63)).unwrap_or(u64::MAX);
        let ceiling = exponential.min(30_000);
        random_sample % (ceiling + 1)
    }

    fn transition(&mut self, state: AgentState, cancel_observation: bool) {
        self.state = state;
        if cancel_observation {
            self.generation = self.generation.wrapping_add(1);
        }
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

fn build_event(
    device_id: &UuidV4,
    source_seq: u64,
    created_at_ms: i64,
    retention_seconds: u64,
    bytes: &[u8],
    content_hash: &str,
) -> Result<ClipboardEventV1, CoreError> {
    let event = serde_json::json!({
        "message_id": Uuid::new_v4().to_string(),
        "source_device_id": device_id.get().to_string(),
        "source_seq": source_seq.to_string(),
        "created_at_ms": created_at_ms,
        "expires_at_ms": created_at_ms.saturating_add(
            retention_seconds.saturating_mul(1000).min(i64::MAX as u64) as i64
        ),
        "content_type": "text/plain",
        "payload_bytes": bytes.len(),
        "content_sha256": content_hash,
        "payload_b64": URL_SAFE_NO_PAD.encode(bytes),
    });
    serde_json::from_value(event).map_err(|_| CoreError::InvalidEvent)
}

fn clone_uuid(value: &UuidV4) -> Result<UuidV4, CoreError> {
    serde_json::from_value(serde_json::Value::String(value.get().to_string()))
        .map_err(|_| CoreError::InvalidEvent)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
