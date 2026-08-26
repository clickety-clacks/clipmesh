use std::path::Path;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use clipmesh_agent_core::{
    AgentCore, AgentState, ClipboardAdapter, LocalControl, LocalObservation, ObservationResult,
    ObservationSuppression, ReceiveResult, ReceivedEvent, ReconnectBackoff, SessionParameters,
};
use clipmesh_protocol::{ClipboardEventV1, Delivery, FailureCode, UuidV4};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const NOW: i64 = 1_700_000_000_000;
const LOCAL_DEVICE: &str = "00000000-0000-4000-8000-000000000001";
const REMOTE_DEVICE: &str = "00000000-0000-4000-8000-000000000002";
const EPOCH: &str = "00000000-0000-4000-8000-000000000003";

#[derive(Default)]
struct SyntheticClipboard {
    bytes: Vec<u8>,
    writes: Vec<String>,
}

impl ClipboardAdapter for SyntheticClipboard {
    fn current_bytes(&mut self) -> Result<Vec<u8>, FailureCode> {
        Ok(self.bytes.clone())
    }

    fn write_text(&mut self, text: &str) -> Result<(), FailureCode> {
        self.bytes = text.as_bytes().to_vec();
        self.writes.push(text.to_owned());
        Ok(())
    }
}

#[test]
fn outbox_sequence_and_cursor_survive_restart() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("agent.sqlite3");
    let first_id;
    {
        let mut agent = live_agent(&path);
        let queued = queue(&mut agent, b"first", NOW);
        first_id = queued.message_id.get();
        let snapshot = agent.snapshot().unwrap();
        assert_eq!(snapshot.highest_source_seq, 1);
        assert_eq!(snapshot.outbox.len(), 1);
    }

    let mut restarted = AgentCore::open(&path, uuid(LOCAL_DEVICE)).unwrap();
    restarted.start_unlocked();
    restarted.set_session(session()).unwrap();
    restarted.finish_resume(NOW);
    let retried = restarted.outbox_for_retry().unwrap();
    assert_eq!(retried.len(), 1);
    assert_eq!(retried[0].event.message_id.get(), first_id);
    assert_eq!(retried[0].event.source_seq.get(), 1);
    let second = queue(&mut restarted, b"second", NOW + 1);
    assert_eq!(second.source_seq.get(), 2);
}

#[test]
fn resume_is_live_only_and_remote_loop_marker_has_no_timer() {
    let directory = TempDir::new().unwrap();
    let mut agent = connecting_agent(&directory.path().join("agent.sqlite3"));
    let mut clipboard = SyntheticClipboard {
        bytes: b"local value".to_vec(),
        ..Default::default()
    };

    let resume = received(1, Delivery::Resume, REMOTE_DEVICE, b"old remote");
    assert_eq!(
        agent.receive_event(resume, NOW, &mut clipboard).unwrap(),
        ReceiveResult::RecordedOnly
    );
    assert!(clipboard.writes.is_empty());
    assert!(agent.finish_resume(NOW).is_some());

    let live = received(2, Delivery::Live, REMOTE_DEVICE, b"new remote");
    assert_eq!(
        agent.receive_event(live, NOW, &mut clipboard).unwrap(),
        ReceiveResult::Applied
    );
    assert_eq!(clipboard.writes, ["new remote"]);

    let marker_observation = agent
        .begin_observation(LocalObservation {
            bytes: b"new remote".to_vec(),
            sensitive: false,
        })
        .unwrap();
    assert_eq!(
        agent
            .commit_observation(marker_observation, NOW + 60_000)
            .unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::RemoteWriteLoop)
    );
    let later = queue(&mut agent, b"later local", NOW + 60_001);
    assert_eq!(later.source_seq.get(), 1);
}

#[test]
fn lock_and_pause_cancel_uncommitted_observations() {
    let directory = TempDir::new().unwrap();
    let mut agent = live_agent(&directory.path().join("agent.sqlite3"));
    let token = agent
        .begin_observation(LocalObservation {
            bytes: b"race value".to_vec(),
            sensitive: false,
        })
        .unwrap();
    agent.set_locked(true);
    assert_eq!(
        agent.commit_observation(token, NOW).unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::StateChanged)
    );
    assert!(agent.snapshot().unwrap().outbox.is_empty());

    agent.set_locked(false);
    assert_eq!(agent.state(), AgentState::ActiveUnlockedConnecting);
    agent.set_session(session()).unwrap();
    agent.finish_resume(NOW);
    let token = agent
        .begin_observation(LocalObservation {
            bytes: b"pause race".to_vec(),
            sensitive: false,
        })
        .unwrap();
    agent.local_control(LocalControl::Pause).unwrap();
    assert_eq!(
        agent.commit_observation(token, NOW).unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::StateChanged)
    );
    assert_eq!(agent.state(), AgentState::LocallyPaused);
}

#[test]
fn sensitive_local_only_and_clear_are_payload_safe() {
    let directory = TempDir::new().unwrap();
    let mut agent = live_agent(&directory.path().join("agent.sqlite3"));
    agent.local_control(LocalControl::LocalOnlyNext).unwrap();
    let local_only = agent
        .begin_observation(LocalObservation {
            bytes: b"local only".to_vec(),
            sensitive: false,
        })
        .unwrap();
    assert_eq!(
        agent.commit_observation(local_only, NOW).unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::LocalOnly)
    );
    let sensitive = agent
        .begin_observation(LocalObservation {
            bytes: b"sensitive".to_vec(),
            sensitive: true,
        })
        .unwrap();
    assert_eq!(
        agent.commit_observation(sensitive, NOW).unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::Sensitive)
    );
    queue(&mut agent, b"ordinary", NOW + 1);
    let sequence = agent.snapshot().unwrap().highest_source_seq;
    agent.local_control(LocalControl::ClearLocalCache).unwrap();
    let snapshot = agent.snapshot().unwrap();
    assert!(snapshot.outbox.is_empty());
    assert_eq!(snapshot.highest_source_seq, sequence);
    assert_eq!(agent.status().unwrap().sensitive_suppressions, 1);
}

#[test]
fn outbox_bounds_do_not_allocate_the_overflow_sequence() {
    let directory = TempDir::new().unwrap();
    let mut agent = live_agent(&directory.path().join("agent.sqlite3"));
    for index in 0..20 {
        queue(&mut agent, format!("value {index}").as_bytes(), NOW + index);
    }
    let token = agent
        .begin_observation(LocalObservation {
            bytes: b"overflow".to_vec(),
            sensitive: false,
        })
        .unwrap();
    assert_eq!(
        agent.commit_observation(token, NOW + 21).unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::OutboxFull)
    );
    let snapshot = agent.snapshot().unwrap();
    assert_eq!(snapshot.highest_source_seq, 20);
    assert_eq!(snapshot.outbox.len(), 20);
    let accepted = snapshot.outbox[0].event.message_id.get().to_string();
    agent.publish_accepted(&uuid(&accepted)).unwrap();
    assert_eq!(agent.state(), AgentState::ActiveUnlockedLive);
}

#[test]
fn acknowledgements_coalesce_and_purge_clears_payload_state() {
    let directory = TempDir::new().unwrap();
    let mut agent = connecting_agent(&directory.path().join("agent.sqlite3"));
    let mut clipboard = SyntheticClipboard::default();
    agent
        .receive_event(
            received(1, Delivery::Resume, REMOTE_DEVICE, b"resume"),
            NOW,
            &mut clipboard,
        )
        .unwrap();
    let resume_ack = agent.finish_resume(NOW).unwrap();
    assert_eq!(resume_ack.cursor.get(), 1);
    agent
        .receive_event(
            received(2, Delivery::Live, REMOTE_DEVICE, b"live two"),
            NOW,
            &mut clipboard,
        )
        .unwrap();
    assert!(agent.poll_ack(NOW + 1_999).is_none());
    assert_eq!(agent.poll_ack(NOW + 2_000).unwrap().cursor.get(), 2);

    queue(&mut agent, b"pending", NOW + 2_001);
    let sequence = agent.snapshot().unwrap().highest_source_seq;
    let new_epoch = uuid("00000000-0000-4000-8000-000000000004");
    agent
        .purge_notice(&new_epoch, Some("2".parse().unwrap()))
        .unwrap();
    let snapshot = agent.snapshot().unwrap();
    assert!(snapshot.outbox.is_empty());
    assert_eq!(snapshot.processed_message_count, 0);
    assert_eq!(snapshot.highest_source_seq, sequence);
    assert_eq!(snapshot.history_epoch, Some(new_epoch));
    assert_eq!(clipboard.bytes, b"live two");
}

#[test]
fn disconnected_observations_are_not_backfilled_and_pause_retries_are_bounded() {
    let directory = TempDir::new().unwrap();
    let mut agent = live_agent(&directory.path().join("agent.sqlite3"));
    agent.disconnect();
    assert!(agent
        .begin_observation(LocalObservation {
            bytes: b"offline".to_vec(),
            sensitive: false,
        })
        .is_none());
    agent.administratively_paused();
    assert_eq!(agent.state(), AgentState::AdministrativelyPaused);
    assert!(AgentCore::reconnect_delay_ms(0, 10_000) <= 500);
    assert!(AgentCore::reconnect_delay_ms(63, u64::MAX) <= 30_000);
    agent.set_session(session()).unwrap();
    assert_eq!(agent.state(), AgentState::ActiveUnlockedConnecting);
    agent.finish_resume(NOW);
    assert_eq!(agent.state(), AgentState::ActiveUnlockedLive);
    assert!(agent.snapshot().unwrap().outbox.is_empty());
}

#[test]
fn sequence_exhaustion_is_local_and_allocates_no_outbox_item() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("agent.sqlite3");
    {
        let agent = live_agent(&path);
        drop(agent);
    }
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'highest_source_seq'",
            [u64::MAX.to_string()],
        )
        .unwrap();
    drop(connection);
    let mut agent = live_agent(&path);
    let token = agent
        .begin_observation(LocalObservation {
            bytes: b"cannot allocate".to_vec(),
            sensitive: false,
        })
        .unwrap();
    assert_eq!(
        agent.commit_observation(token, NOW).unwrap_err(),
        clipmesh_agent_core::CoreError::DeviceSequenceExhausted
    );
    assert!(agent.snapshot().unwrap().outbox.is_empty());
}

#[test]
fn local_state_is_explicit_stamped_and_owner_only() {
    let directory = TempDir::new().unwrap();
    let missing = directory.path().join("missing.sqlite3");
    assert_eq!(
        AgentCore::open(&missing, uuid(LOCAL_DEVICE)).err().unwrap(),
        clipmesh_agent_core::CoreError::LocalStateUnavailable
    );

    let state = directory.path().join("state.sqlite3");
    drop(AgentCore::initialize(&state, uuid(LOCAL_DEVICE)).unwrap());
    let connection = Connection::open(&state).unwrap();
    connection.pragma_update(None, "user_version", 9).unwrap();
    drop(connection);
    assert_eq!(
        AgentCore::open(&state, uuid(LOCAL_DEVICE)).err().unwrap(),
        clipmesh_agent_core::CoreError::LocalStateUnavailable
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let insecure = directory.path().join("insecure.sqlite3");
        drop(AgentCore::initialize(&insecure, uuid(LOCAL_DEVICE)).unwrap());
        std::fs::set_permissions(&insecure, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            AgentCore::open(&insecure, uuid(LOCAL_DEVICE))
                .err()
                .unwrap(),
            clipmesh_agent_core::CoreError::LocalStateUnavailable
        );
    }
}

#[test]
fn expired_outbox_releases_capacity_and_long_live_resets_backoff() {
    let directory = TempDir::new().unwrap();
    let mut agent = live_agent(&directory.path().join("agent.sqlite3"));
    for index in 0..20 {
        queue(&mut agent, format!("value {index}").as_bytes(), NOW + index);
    }
    let token = agent
        .begin_observation(LocalObservation {
            bytes: b"overflow".to_vec(),
            sensitive: false,
        })
        .unwrap();
    assert_eq!(
        agent.commit_observation(token, NOW + 21).unwrap(),
        ObservationResult::Suppressed(ObservationSuppression::OutboxFull)
    );
    assert_eq!(agent.expire_outbox(NOW + 14_400_100).unwrap(), 20);
    assert_eq!(agent.state(), AgentState::ActiveUnlockedLive);

    let mut backoff = ReconnectBackoff::default();
    backoff.next_delay_ms(0);
    backoff.next_delay_ms(0);
    assert_eq!(backoff.attempt(), 2);
    backoff.entered_live(NOW);
    backoff.disconnected(NOW + 29_999);
    assert_eq!(backoff.attempt(), 2);
    backoff.entered_live(NOW);
    backoff.disconnected(NOW + 30_000);
    assert_eq!(backoff.attempt(), 0);
}

fn connecting_agent(path: &Path) -> AgentCore {
    let mut agent = if path.exists() {
        AgentCore::open(path, uuid(LOCAL_DEVICE)).unwrap()
    } else {
        AgentCore::initialize(path, uuid(LOCAL_DEVICE)).unwrap()
    };
    agent.start_unlocked();
    agent.set_session(session()).unwrap();
    agent
}

fn live_agent(path: &Path) -> AgentCore {
    let mut agent = connecting_agent(path);
    agent.finish_resume(NOW);
    agent
}

fn session() -> SessionParameters {
    SessionParameters {
        history_epoch: uuid(EPOCH),
        limits: serde_json::from_value(serde_json::json!({
            "max_payload_bytes": 262144,
            "retention_seconds": 14400,
            "history_max_entries": 20,
            "max_clock_skew_ms": 120000,
            "max_websocket_message_bytes": 524288
        }))
        .unwrap(),
        server_time_offset_ms: 0,
    }
}

fn queue(agent: &mut AgentCore, bytes: &[u8], now: i64) -> ClipboardEventV1 {
    let token = agent
        .begin_observation(LocalObservation {
            bytes: bytes.to_vec(),
            sensitive: false,
        })
        .unwrap();
    match agent.commit_observation(token, now).unwrap() {
        ObservationResult::Queued(item) => item.event,
        result => panic!("expected queued observation, got {result:?}"),
    }
}

fn received(cursor: u64, delivery: Delivery, source_device: &str, bytes: &[u8]) -> ReceivedEvent {
    let hash: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let event = serde_json::from_value(serde_json::json!({
        "message_id": format!("00000000-0000-4000-8000-{cursor:012}"),
        "source_device_id": source_device,
        "source_seq": cursor.to_string(),
        "created_at_ms": NOW,
        "expires_at_ms": NOW + 14_400_000,
        "content_type": "text/plain",
        "payload_bytes": bytes.len(),
        "content_sha256": hash,
        "payload_b64": URL_SAFE_NO_PAD.encode(bytes),
    }))
    .unwrap();
    ReceivedEvent {
        history_epoch: uuid(EPOCH),
        cursor: cursor.to_string().parse().unwrap(),
        delivery,
        event,
    }
}

fn uuid(value: &str) -> UuidV4 {
    serde_json::from_value(serde_json::Value::String(value.to_owned())).unwrap()
}
