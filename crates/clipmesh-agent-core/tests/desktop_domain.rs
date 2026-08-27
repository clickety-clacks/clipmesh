use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use clipmesh_agent_core::{
    AdapterError, AgentCore, AgentState, ClipContentV1, ClipboardAdapter, CoreError, Delivery,
    HintClassification, LocalControl, LocalObservation, ObservationResult, ObservationSuppression,
    PermanentPublishFailure, PlatformRevision, ReceiveResult, ReceivedEvent, ReconnectBackoff,
    SessionParameters,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

const NOW: i64 = 1_700_000_000_000;
const LOCAL_PEER: &str = "peer-local-synthetic";
const REMOTE_PEER: &str = "peer-remote-synthetic";

#[derive(Default)]
struct SyntheticClipboard {
    current_revision: Option<PlatformRevision>,
    writes: Vec<Vec<u8>>,
    next_write: u64,
    fail_current: bool,
    fail_write: bool,
    drop_history_after_write: Option<PathBuf>,
}

impl SyntheticClipboard {
    fn observe(&mut self, bytes: &[u8], revision: &str) -> LocalObservation {
        let revision = PlatformRevision::synthetic(revision);
        self.current_revision = Some(revision.clone());
        LocalObservation {
            bytes: bytes.to_vec(),
            revision,
            hint: HintClassification::Ordinary,
        }
    }
}

impl ClipboardAdapter for SyntheticClipboard {
    fn is_current(&mut self, revision: &PlatformRevision) -> Result<bool, AdapterError> {
        if self.fail_current {
            return Err(AdapterError);
        }
        Ok(self.current_revision.as_ref() == Some(revision))
    }

    fn write_text(&mut self, bytes: &[u8]) -> Result<PlatformRevision, AdapterError> {
        if self.fail_write {
            return Err(AdapterError);
        }
        self.next_write += 1;
        let revision = PlatformRevision::synthetic(format!("remote-{}", self.next_write));
        self.current_revision = Some(revision.clone());
        self.writes.push(bytes.to_vec());
        if let Some(path) = self.drop_history_after_write.take() {
            Connection::open(path)
                .unwrap()
                .execute("DROP TABLE history", [])
                .unwrap();
        }
        Ok(revision)
    }
}

#[test]
fn absent_state_initializes_and_outbox_retries_exactly_after_restart() {
    let (_directory, path) = state_path();
    let first;
    {
        let (mut agent, mut clipboard) = live_agent(&path);
        first = queue(&mut agent, &mut clipboard, b"first", "local-1", NOW);
        assert_eq!(agent.snapshot().unwrap().outbox.len(), 1);
    }

    let mut restarted = AgentCore::open(&path).unwrap();
    restarted.start_unlocked();
    restarted.set_session(session()).unwrap();
    assert!(restarted.outbox_for_retry().unwrap().is_empty());
    restarted.finish_resume(NOW);
    let retried = restarted.outbox_for_retry().unwrap();
    assert_eq!(retried.len(), 1);
    assert_eq!(retried[0].event, first);

    let mut clipboard = SyntheticClipboard::default();
    let second = queue(
        &mut restarted,
        &mut clipboard,
        b"second",
        "local-2",
        NOW + 1,
    );
    assert_ne!(second.message_id, first.message_id);
}

#[test]
fn resume_never_writes_and_live_remote_always_writes_even_when_bytes_match() {
    let (_directory, path) = state_path();
    let mut agent = connecting_agent(&path);
    let mut clipboard = SyntheticClipboard::default();

    assert_eq!(
        agent
            .receive_event(
                received(1, Delivery::Resume, REMOTE_PEER, b"same"),
                NOW,
                &mut clipboard
            )
            .unwrap(),
        ReceiveResult::RecordedOnly
    );
    assert!(clipboard.writes.is_empty());
    agent.finish_resume(NOW);

    clipboard.current_revision = Some(PlatformRevision::synthetic("already-same"));
    assert_eq!(
        agent
            .receive_event(
                received(2, Delivery::Live, REMOTE_PEER, b"same"),
                NOW,
                &mut clipboard
            )
            .unwrap(),
        ReceiveResult::Applied
    );
    assert_eq!(clipboard.writes, [b"same".to_vec()]);
}

#[test]
fn revision_marker_suppresses_stale_and_duplicate_notifications_without_a_timer() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);

    agent
        .receive_event(
            received(1, Delivery::Live, REMOTE_PEER, b"R1"),
            NOW,
            &mut clipboard,
        )
        .unwrap();
    let stale_r1 = agent
        .begin_observation(LocalObservation {
            bytes: b"R1".to_vec(),
            revision: PlatformRevision::synthetic("remote-1"),
            hint: HintClassification::Ordinary,
        })
        .unwrap();
    agent
        .receive_event(
            received(2, Delivery::Live, REMOTE_PEER, b"R2"),
            NOW,
            &mut clipboard,
        )
        .unwrap();

    assert_eq!(
        agent
            .commit_observation(stale_r1, NOW, &mut clipboard)
            .unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::StaleNotification)
    );
    for _ in 0..3 {
        let duplicate = agent
            .begin_observation(LocalObservation {
                bytes: b"R2".to_vec(),
                revision: PlatformRevision::synthetic("remote-2"),
                hint: HintClassification::Ordinary,
            })
            .unwrap();
        assert_eq!(
            agent
                .commit_observation(duplicate, NOW + 60_000, &mut clipboard)
                .unwrap(),
            ObservationResult::Suppressed(ObservationSuppression::RemoteWriteLoop)
        );
    }
    assert_eq!(
        agent.snapshot().unwrap().loop_marker.unwrap().revision,
        PlatformRevision::synthetic("remote-2")
    );

    let later = clipboard.observe(b"R2", "local-3");
    let later = agent.begin_observation(later).unwrap();
    assert!(matches!(
        agent
            .commit_observation(later, NOW + 120_000, &mut clipboard)
            .unwrap(),
        ObservationResult::Queued(_)
    ));
    assert!(agent.snapshot().unwrap().loop_marker.is_none());
}

#[test]
fn equal_revision_with_changed_content_fails_the_adapter_and_retains_marker() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    agent
        .receive_event(
            received(1, Delivery::Live, REMOTE_PEER, b"remote"),
            NOW,
            &mut clipboard,
        )
        .unwrap();
    let bad = agent
        .begin_observation(LocalObservation {
            bytes: b"different".to_vec(),
            revision: PlatformRevision::synthetic("remote-1"),
            hint: HintClassification::Ordinary,
        })
        .unwrap();
    assert_eq!(
        agent.commit_observation(bad, NOW, &mut clipboard),
        Err(CoreError::AdapterUnavailable)
    );
    assert_eq!(agent.state(), AgentState::AdapterFailed);
    assert!(agent.snapshot().unwrap().loop_marker.is_some());
    assert!(agent.snapshot().unwrap().outbox.is_empty());
}

#[test]
fn lock_pause_and_disconnection_never_backfill_observations() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    let observation = clipboard.observe(b"race", "local-race");
    let token = agent.begin_observation(observation).unwrap();
    agent.set_locked(true);
    assert_eq!(
        agent
            .commit_observation(token, NOW, &mut clipboard)
            .unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::StateChanged)
    );
    assert!(agent.snapshot().unwrap().outbox.is_empty());

    agent.set_locked(false);
    agent.set_session(session()).unwrap();
    agent.finish_resume(NOW);
    agent.local_control(LocalControl::Pause).unwrap();
    assert_eq!(agent.state(), AgentState::LocallyPaused);
    assert!(agent
        .begin_observation(clipboard.observe(b"paused", "paused"))
        .is_none());
    agent.local_control(LocalControl::Resume).unwrap();
    assert_eq!(agent.state(), AgentState::ActiveUnlockedConnecting);
    assert!(agent
        .begin_observation(clipboard.observe(b"connecting", "connecting"))
        .is_none());
}

#[test]
fn explicit_hints_and_local_only_next_consume_no_content_state() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    for hint in [
        HintClassification::Confidential,
        HintClassification::Transient,
    ] {
        let mut observation = clipboard.observe(b"not persisted", "hinted");
        observation.hint = hint;
        let token = agent.begin_observation(observation).unwrap();
        assert_eq!(
            agent
                .commit_observation(token, NOW, &mut clipboard)
                .unwrap(),
            ObservationResult::Suppressed(ObservationSuppression::ExplicitHint)
        );
    }
    agent.local_control(LocalControl::LocalOnlyNext).unwrap();
    let first = clipboard.observe(b"local only", "local-only");
    let first = agent.begin_observation(first).unwrap();
    assert_eq!(
        agent
            .commit_observation(first, NOW, &mut clipboard)
            .unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::LocalOnly)
    );
    queue(&mut agent, &mut clipboard, b"ordinary", "ordinary", NOW + 1);
    assert_eq!(agent.snapshot().unwrap().outbox.len(), 1);
    assert_eq!(agent.status().unwrap().hinted_suppressions, 2);
}

#[test]
fn outbox_overflow_preserves_existing_rows_and_stores_no_new_content() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    for index in 0..20 {
        queue(
            &mut agent,
            &mut clipboard,
            format!("value {index}").as_bytes(),
            &format!("local-{index}"),
            NOW + index,
        );
    }
    let before = agent.snapshot().unwrap().outbox;
    let overflow = clipboard.observe(b"overflow-canary", "overflow");
    let overflow = agent.begin_observation(overflow).unwrap();
    assert_eq!(
        agent
            .commit_observation(overflow, NOW + 21, &mut clipboard)
            .unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::OutboxFull)
    );
    let after = agent.snapshot().unwrap().outbox;
    assert_eq!(after, before);
    assert_eq!(agent.state(), AgentState::OutboxFull);
    agent.publish_accepted(after[0].event.message_id).unwrap();
    assert_eq!(agent.state(), AgentState::ActiveUnlockedLive);
}

#[test]
fn local_history_clear_preserves_resume_context_outbox_marker_and_clipboard() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    queue(&mut agent, &mut clipboard, b"pending", "local", NOW);
    agent
        .receive_event(
            received(1, Delivery::Live, REMOTE_PEER, b"remote"),
            NOW,
            &mut clipboard,
        )
        .unwrap();
    let before = agent.snapshot().unwrap();
    let clipboard_before = clipboard.writes.clone();
    agent
        .local_control(LocalControl::ClearLocalHistory)
        .unwrap();
    let after = agent.snapshot().unwrap();
    assert_eq!(after.outbox, before.outbox);
    assert_eq!(after.history_epoch, before.history_epoch);
    assert_eq!(after.clear_generation, before.clear_generation);
    assert_eq!(after.last_cursor, before.last_cursor);
    assert_eq!(after.loop_marker, before.loop_marker);
    assert_eq!(after.history_count, 0);
    assert_eq!(after.processed_message_count, 0);
    assert_eq!(clipboard.writes, clipboard_before);
}

#[test]
fn shared_clear_deletes_old_generation_product_state_without_clipboard_write() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    queue(&mut agent, &mut clipboard, b"pending", "local", NOW);
    agent
        .receive_event(
            received(1, Delivery::Live, REMOTE_PEER, b"remote"),
            NOW,
            &mut clipboard,
        )
        .unwrap();
    let writes = clipboard.writes.clone();
    agent
        .clear_notice(session().history_epoch, 2, Some(1))
        .unwrap();
    let snapshot = agent.snapshot().unwrap();
    assert!(snapshot.outbox.is_empty());
    assert!(snapshot.loop_marker.is_none());
    assert_eq!(snapshot.history_count, 0);
    assert_eq!(snapshot.processed_message_count, 0);
    assert_eq!(snapshot.clear_generation, Some(2));
    assert_eq!(snapshot.last_cursor, Some(1));
    assert_eq!(clipboard.writes, writes);
}

#[test]
fn five_hundred_resume_rows_coalesce_to_one_ack_and_live_acks_are_bounded() {
    let (_directory, path) = state_path();
    let mut agent = connecting_agent(&path);
    let mut clipboard = SyntheticClipboard::default();
    for cursor in 1..=500 {
        agent
            .receive_event(
                received(cursor, Delivery::Resume, REMOTE_PEER, b"resume"),
                NOW,
                &mut clipboard,
            )
            .unwrap();
    }
    let ack = agent.finish_resume(NOW).unwrap();
    assert_eq!(ack.cursor, 500);
    assert!(agent.poll_ack(NOW).is_none());
    agent
        .receive_event(
            received(501, Delivery::Live, REMOTE_PEER, b"live"),
            NOW,
            &mut clipboard,
        )
        .unwrap();
    assert!(agent.poll_ack(NOW + 1_999).is_none());
    assert_eq!(agent.poll_ack(NOW + 2_000).unwrap().cursor, 501);
}

#[test]
fn state_path_failures_are_distinct_and_unsupported_schema_is_byte_identical() {
    let (directory, path) = state_path();
    drop(AgentCore::open(&path).unwrap());

    let bytes_before = std::fs::read(&path).unwrap();
    let connection = Connection::open(&path).unwrap();
    connection.pragma_update(None, "user_version", 9).unwrap();
    drop(connection);
    let unsupported_before = std::fs::read(&path).unwrap();
    assert_eq!(
        AgentCore::open(&path).err().unwrap(),
        CoreError::LocalStateUnavailable
    );
    assert_eq!(std::fs::read(&path).unwrap(), unsupported_before);
    assert_ne!(bytes_before, unsupported_before);

    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let broad_parent = directory.path().join("broad");
        std::fs::create_dir(&broad_parent).unwrap();
        std::fs::set_permissions(&broad_parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            AgentCore::open(&broad_parent.join("state.sqlite3"))
                .err()
                .unwrap(),
            CoreError::StatePathInsecure
        );

        let symlink_path = directory.path().join("linked.sqlite3");
        symlink(&path, &symlink_path).unwrap();
        assert_eq!(
            AgentCore::open(&symlink_path).err().unwrap(),
            CoreError::StatePathInsecure
        );

        let broad_file = directory.path().join("broad-file.sqlite3");
        drop(AgentCore::open(&broad_file).unwrap());
        std::fs::set_permissions(&broad_file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let broad_file_before = std::fs::read(&broad_file).unwrap();
        assert_eq!(
            AgentCore::open(&broad_file).err().unwrap(),
            CoreError::StatePathInsecure
        );
        assert_eq!(std::fs::read(&broad_file).unwrap(), broad_file_before);

        let read_only = directory.path().join("read-only.sqlite3");
        drop(AgentCore::open(&read_only).unwrap());
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o400)).unwrap();
        let read_only_before = std::fs::read(&read_only).unwrap();
        assert_eq!(
            AgentCore::open(&read_only).err().unwrap(),
            CoreError::LocalStateUnavailable
        );
        assert_eq!(std::fs::read(&read_only).unwrap(), read_only_before);
    }

    let corrupt = directory.path().join("corrupt.sqlite3");
    std::fs::write(&corrupt, b"not a SQLite database").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&corrupt, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let corrupt_before = std::fs::read(&corrupt).unwrap();
    assert_eq!(
        AgentCore::open(&corrupt).err().unwrap(),
        CoreError::LocalStateUnavailable
    );
    assert_eq!(std::fs::read(&corrupt).unwrap(), corrupt_before);
}

#[test]
fn marker_is_cleared_on_restart_and_backoff_uses_full_jitter() {
    let (_directory, path) = state_path();
    {
        let (mut agent, mut clipboard) = live_agent(&path);
        agent
            .receive_event(
                received(1, Delivery::Live, REMOTE_PEER, b"remote"),
                NOW,
                &mut clipboard,
            )
            .unwrap();
        assert!(agent.snapshot().unwrap().loop_marker.is_some());
    }
    let restarted = AgentCore::open(&path).unwrap();
    assert!(restarted.snapshot().unwrap().loop_marker.is_none());

    let mut backoff = ReconnectBackoff::default();
    assert!(backoff.next_delay_ms(u64::MAX) <= 500);
    assert!(backoff.next_delay_ms(u64::MAX) <= 1_000);
    backoff.entered_live(NOW);
    backoff.disconnected(NOW + 30_000);
    assert_eq!(backoff.attempt(), 0);
    assert!(AgentCore::reconnect_delay_ms(63, u64::MAX) <= 30_000);
}

#[test]
fn canonical_content_seam_preserves_bytes_and_sanitizes_preview() {
    let bytes = b"hello\0  world\nnext";
    let content = ClipContentV1::from_platform(bytes, 262_144).unwrap();
    let wire = content.to_wire();
    let decoded = ClipContentV1::from_wire(
        wire.content_type,
        &wire.payload_b64,
        wire.payload_bytes,
        &wire.content_sha256,
        262_144,
    )
    .unwrap();
    assert!(content.same_content(&decoded));
    assert_eq!(decoded.to_platform(), bytes);
    assert_eq!(decoded.to_preview(12), "hello� world");
}

#[test]
fn source_echo_is_recorded_without_a_platform_write() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    assert_eq!(
        agent
            .receive_event(
                received(1, Delivery::Live, LOCAL_PEER, b"echo"),
                NOW,
                &mut clipboard,
            )
            .unwrap(),
        ReceiveResult::RecordedOnly
    );
    assert!(clipboard.writes.is_empty());
    assert_eq!(agent.snapshot().unwrap().history_count, 1);
}

#[test]
fn retryable_rejection_keeps_exact_outbox_and_permanent_rejection_removes_content() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    let event = queue(&mut agent, &mut clipboard, b"pending", "local", NOW);
    agent
        .publish_rejected(event.message_id, PermanentPublishFailure::Validation, true)
        .unwrap();
    assert_eq!(agent.snapshot().unwrap().outbox[0].event, event);
    agent
        .publish_rejected(event.message_id, PermanentPublishFailure::Replay, false)
        .unwrap();
    assert!(agent.snapshot().unwrap().outbox.is_empty());
}

#[test]
fn reconnect_generation_change_deletes_old_state_and_rejects_rollback() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    queue(&mut agent, &mut clipboard, b"old generation", "local", NOW);
    agent.disconnect();
    let mut next = session();
    next.clear_generation = 2;
    agent.set_session(next).unwrap();
    let snapshot = agent.snapshot().unwrap();
    assert!(snapshot.outbox.is_empty());
    assert_eq!(snapshot.clear_generation, Some(2));

    agent.disconnect();
    assert_eq!(
        agent.set_session(session()),
        Err(CoreError::ClearGenerationStale)
    );
    assert_eq!(agent.snapshot().unwrap().clear_generation, Some(2));
}

#[test]
fn adapter_errors_make_the_agent_inactive_until_external_repair() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    let observation = clipboard.observe(b"local", "local-error");
    let token = agent.begin_observation(observation).unwrap();
    clipboard.fail_current = true;
    assert_eq!(
        agent.commit_observation(token, NOW, &mut clipboard),
        Err(CoreError::AdapterUnavailable)
    );
    assert_eq!(agent.state(), AgentState::AdapterFailed);
    assert!(agent
        .begin_observation(clipboard.observe(b"later", "later"))
        .is_none());

    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    clipboard.fail_write = true;
    assert_eq!(
        agent.receive_event(
            received(1, Delivery::Live, REMOTE_PEER, b"remote"),
            NOW,
            &mut clipboard,
        ),
        Err(CoreError::AdapterUnavailable)
    );
    assert_eq!(agent.state(), AgentState::AdapterFailed);
    assert!(agent
        .begin_observation(clipboard.observe(b"later", "later"))
        .is_none());
}

#[test]
fn post_write_storage_failure_stops_every_observable_seam() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    clipboard.drop_history_after_write = Some(path);
    assert_eq!(
        agent.receive_event(
            received(1, Delivery::Live, REMOTE_PEER, b"remote"),
            NOW,
            &mut clipboard,
        ),
        Err(CoreError::LocalStateUnavailable)
    );
    assert_eq!(clipboard.writes, [b"remote".to_vec()]);
    assert_eq!(agent.state(), AgentState::Stopping);
    assert!(agent
        .begin_observation(clipboard.observe(b"remote", "remote-1"))
        .is_none());
    assert!(agent.outbox_for_retry().unwrap().is_empty());
}

#[test]
fn live_cursor_gap_changes_no_state_and_is_never_acked_past() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    agent
        .receive_event(
            received(1, Delivery::Live, REMOTE_PEER, b"one"),
            NOW,
            &mut clipboard,
        )
        .unwrap();
    assert_eq!(agent.poll_ack(NOW).unwrap().cursor, 1);
    assert_eq!(
        agent.receive_event(
            received(3, Delivery::Live, REMOTE_PEER, b"three"),
            NOW + 2_000,
            &mut clipboard,
        ),
        Err(CoreError::CursorOrderInvalid)
    );
    assert_eq!(agent.snapshot().unwrap().last_cursor, Some(1));
    assert!(agent.poll_ack(NOW + 2_000).is_none());
    assert_eq!(clipboard.writes, [b"one".to_vec()]);
}

#[test]
fn epoch_change_preserves_the_process_lifetime_loop_marker() {
    let (_directory, path) = state_path();
    let (mut agent, mut clipboard) = live_agent(&path);
    agent
        .receive_event(
            received(1, Delivery::Live, REMOTE_PEER, b"remote"),
            NOW,
            &mut clipboard,
        )
        .unwrap();
    agent.disconnect();
    let mut next = session();
    next.history_epoch = Uuid::parse_str("00000000-0000-4000-8000-000000000004").unwrap();
    agent.set_session(next).unwrap();
    assert!(agent.snapshot().unwrap().loop_marker.is_some());
    agent.finish_resume(NOW + 1);

    let echo = agent
        .begin_observation(LocalObservation {
            bytes: b"remote".to_vec(),
            revision: PlatformRevision::synthetic("remote-1"),
            hint: HintClassification::Ordinary,
        })
        .unwrap();
    assert_eq!(
        agent
            .commit_observation(echo, NOW + 2, &mut clipboard)
            .unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::RemoteWriteLoop)
    );
    assert!(agent.snapshot().unwrap().loop_marker.is_some());
    assert!(agent.snapshot().unwrap().outbox.is_empty());
}

fn state_path() -> (TempDir, PathBuf) {
    let directory = TempDir::new().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = directory.path().join("agent.sqlite3");
    (directory, path)
}

fn connecting_agent(path: &Path) -> AgentCore {
    let mut agent = AgentCore::open(path).unwrap();
    agent.start_unlocked();
    agent.set_session(session()).unwrap();
    agent
}

fn live_agent(path: &Path) -> (AgentCore, SyntheticClipboard) {
    let mut agent = connecting_agent(path);
    agent.finish_resume(NOW);
    (agent, SyntheticClipboard::default())
}

fn session() -> SessionParameters {
    SessionParameters {
        self_peer_id: LOCAL_PEER.to_owned(),
        history_epoch: Uuid::parse_str("00000000-0000-4000-8000-000000000003").unwrap(),
        clear_generation: 1,
        max_payload_bytes: 262_144,
        retention_seconds: 604_800,
        server_time_offset_ms: 0,
    }
}

fn queue(
    agent: &mut AgentCore,
    clipboard: &mut SyntheticClipboard,
    bytes: &[u8],
    revision: &str,
    now_ms: i64,
) -> clipmesh_agent_core::PublishEventV1 {
    let observation = clipboard.observe(bytes, revision);
    let token = agent.begin_observation(observation).unwrap();
    match agent.commit_observation(token, now_ms, clipboard).unwrap() {
        ObservationResult::Queued(item) => item.event,
        result => panic!("expected queued observation, got {result:?}"),
    }
}

fn received(cursor: u64, delivery: Delivery, source_peer: &str, bytes: &[u8]) -> ReceivedEvent {
    let content_sha256: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    ReceivedEvent {
        history_epoch: session().history_epoch,
        clear_generation: 1,
        cursor,
        delivery,
        accepted_at_ms: NOW,
        expires_at_ms: NOW + 604_800_000,
        source_peer_id: source_peer.to_owned(),
        message_id: Uuid::parse_str(&format!("00000000-0000-4000-8000-{cursor:012}")).unwrap(),
        created_at_ms: NOW,
        content_type: "text/plain".to_owned(),
        payload_b64: URL_SAFE_NO_PAD.encode(bytes),
        payload_bytes: bytes.len(),
        content_sha256,
    }
}
