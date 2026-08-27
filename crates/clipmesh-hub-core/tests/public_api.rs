use clipmesh_hub_core::{
    ClipContentV1, HubCore, PublishInput, RetentionLimits, SessionEvent, StablePeerId,
};
use std::{
    sync::{mpsc, Arc, Barrier},
    thread,
    time::Duration,
};
use tempfile::tempdir;
use uuid::Uuid;

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
fn a_transport_edge_can_supply_a_stable_peer_without_application_identity() {
    let directory = tempdir().unwrap();
    let core = HubCore::open(
        directory.path().join("hub.sqlite3"),
        RetentionLimits::default(),
    )
    .unwrap();
    let hello = core.open_session(StablePeerId::from_boundary("peer-synthetic").unwrap());
    assert_eq!(hello.self_peer_id.as_boundary_value(), "peer-synthetic");
    core.begin_resume(hello.session_id, None, None, None, 1_700_000_000_000)
        .unwrap();
    core.complete_resume(hello.session_id).unwrap();
    let accepted = core
        .publish(
            hello.session_id,
            PublishInput {
                message_id: Uuid::new_v4(),
                clear_generation: hello.clear_generation,
                created_at_ms: 1_700_000_000_000,
                content: ClipContentV1::from_platform(b"synthetic text", 262_144).unwrap(),
            },
            1_700_000_000_000,
        )
        .unwrap();
    assert_eq!(accepted.cursor, 1);
    let mut saw_started = false;
    let mut saw_complete = false;
    let mut saw_live = false;
    consume_session_events(&core, hello.session_id, |event| {
        saw_started |= matches!(event, SessionEvent::ResumeStarted(_));
        saw_complete |= matches!(event, SessionEvent::ResumeComplete(_));
        saw_live |= matches!(event, SessionEvent::Live(clip) if clip.cursor == 1);
    });
    assert!(saw_started && saw_complete && saw_live);
}

#[test]
fn public_diagnostics_are_content_free_and_periodic_retention_is_callable() {
    let content = ClipContentV1::from_platform(b"diagnostic canary", 262_144).unwrap();
    let wire = content.to_wire();
    let diagnostic = format!("{wire:?}");
    assert_eq!(diagnostic, "WireContentV1([redacted])");
    assert!(!diagnostic.contains("diagnostic canary"));
    assert!(!diagnostic.contains(&wire.payload_b64));
    assert!(!diagnostic.contains(&wire.content_sha256));

    let directory = tempdir().unwrap();
    let core = HubCore::open(
        directory.path().join("hub.sqlite3"),
        RetentionLimits::default(),
    )
    .unwrap();
    core.run_periodic_retention(1_700_000_060_000).unwrap();
}

#[test]
fn a_transport_edge_can_lease_shared_clear_events() {
    let directory = tempdir().unwrap();
    let core = HubCore::open(
        directory.path().join("hub.sqlite3"),
        RetentionLimits::default(),
    )
    .unwrap();
    let left = core.open_session(StablePeerId::from_boundary("peer-left").unwrap());
    let right = core.open_session(StablePeerId::from_boundary("peer-right").unwrap());
    for session_id in [left.session_id, right.session_id] {
        core.begin_resume(session_id, None, None, None, 1_700_000_000_000)
            .unwrap();
        core.complete_resume(session_id).unwrap();
    }
    let request_id = Uuid::new_v4();
    core.clear_history(right.session_id, request_id, 1).unwrap();

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
    assert!(right_saw_accepted && right_saw_notice);
}

#[test]
fn live_event_handoff_is_ordered_before_a_later_clear_commit() {
    let directory = tempdir().unwrap();
    let core = Arc::new(
        HubCore::open(
            directory.path().join("hub.sqlite3"),
            RetentionLimits::default(),
        )
        .unwrap(),
    );
    let source = core.open_session(StablePeerId::from_boundary("peer-source").unwrap());
    let clearer = core.open_session(StablePeerId::from_boundary("peer-clearer").unwrap());
    for session_id in [source.session_id, clearer.session_id] {
        core.begin_resume(session_id, None, None, None, 1_700_000_000_000)
            .unwrap();
        core.complete_resume(session_id).unwrap();
        consume_session_events(&core, session_id, |_| {});
    }
    core.publish(
        source.session_id,
        PublishInput {
            message_id: Uuid::new_v4(),
            clear_generation: 1,
            created_at_ms: 1_700_000_000_000,
            content: ClipContentV1::from_platform(b"generation one", 262_144).unwrap(),
        },
        1_700_000_000_000,
    )
    .unwrap();

    let acceptance = core
        .lease_next_session_event(source.session_id)
        .unwrap()
        .unwrap();
    assert!(matches!(
        acceptance.event(),
        SessionEvent::PublishAccepted(_)
    ));
    acceptance.complete();
    let live = core
        .lease_next_session_event(source.session_id)
        .unwrap()
        .unwrap();
    assert!(matches!(
        live.event(),
        SessionEvent::Live(clip) if clip.clear_generation == 1
    ));

    let barrier = Arc::new(Barrier::new(2));
    let (completed_tx, completed_rx) = mpsc::channel();
    let clear_core = Arc::clone(&core);
    let clear_barrier = Arc::clone(&barrier);
    let clear_thread = thread::spawn(move || {
        clear_barrier.wait();
        let result = clear_core.clear_history(clearer.session_id, Uuid::new_v4(), 1);
        completed_tx.send(result).unwrap();
    });
    barrier.wait();
    assert!(completed_rx
        .recv_timeout(Duration::from_millis(50))
        .is_err());

    live.complete();
    let cleared = completed_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert_eq!(cleared.clear_generation, 2);
    clear_thread.join().unwrap();
}

#[test]
fn dropping_an_unfinished_live_lease_keeps_the_event_retractable() {
    let directory = tempdir().unwrap();
    let core = HubCore::open(
        directory.path().join("hub.sqlite3"),
        RetentionLimits::default(),
    )
    .unwrap();
    let source = core.open_session(StablePeerId::from_boundary("peer-source").unwrap());
    let clearer = core.open_session(StablePeerId::from_boundary("peer-clearer").unwrap());
    for session_id in [source.session_id, clearer.session_id] {
        core.begin_resume(session_id, None, None, None, 1_700_000_000_000)
            .unwrap();
        core.complete_resume(session_id).unwrap();
        consume_session_events(&core, session_id, |_| {});
    }
    core.publish(
        source.session_id,
        PublishInput {
            message_id: Uuid::new_v4(),
            clear_generation: 1,
            created_at_ms: 1_700_000_000_000,
            content: ClipContentV1::from_platform(b"retract me", 262_144).unwrap(),
        },
        1_700_000_000_000,
    )
    .unwrap();
    core.lease_next_session_event(source.session_id)
        .unwrap()
        .unwrap()
        .complete();
    let unfinished = core
        .lease_next_session_event(source.session_id)
        .unwrap()
        .unwrap();
    assert!(matches!(
        unfinished.event(),
        SessionEvent::Live(clip) if clip.clear_generation == 1
    ));
    drop(unfinished);

    let request_id = Uuid::new_v4();
    core.clear_history(clearer.session_id, request_id, 1)
        .unwrap();
    let mut saw_old_live = false;
    let mut saw_notice = false;
    consume_session_events(&core, source.session_id, |event| {
        saw_old_live |= matches!(
            event,
            SessionEvent::Live(clip) if clip.clear_generation == 1
        );
        saw_notice |= matches!(
            event,
            SessionEvent::ClearNotice(notice)
                if notice.request_id == request_id && notice.clear_generation == 2
        );
    });
    assert!(!saw_old_live);
    assert!(saw_notice);
}
