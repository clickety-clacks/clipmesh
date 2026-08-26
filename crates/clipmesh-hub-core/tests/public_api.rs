use clipmesh_hub_core::{
    ClipContentV1, HubCore, PublishInput, RetentionLimits, SessionEvent, StablePeerId,
};
use tempfile::tempdir;
use uuid::Uuid;

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
    let events = core.drain_session_events(hello.session_id).unwrap();
    assert!(matches!(
        events.first(),
        Some(SessionEvent::ResumeStarted(_))
    ));
    assert!(events
        .iter()
        .any(|event| matches!(event, SessionEvent::ResumeComplete(_))));
    assert!(events
        .iter()
        .any(|event| matches!(event, SessionEvent::Live(clip) if clip.cursor == 1)));
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
fn a_transport_edge_can_drain_shared_clear_events() {
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

    let left_events = core.drain_session_events(left.session_id).unwrap();
    assert!(left_events.iter().any(|event| matches!(
        event,
        SessionEvent::ClearNotice(notice)
            if notice.request_id == request_id && notice.clear_generation == 2
    )));
    let right_events = core.drain_session_events(right.session_id).unwrap();
    assert!(right_events.iter().any(|event| matches!(
        event,
        SessionEvent::ClearAccepted(accepted)
            if accepted.request_id == request_id && accepted.clear_generation == 2
    )));
    assert!(right_events.iter().any(|event| matches!(
        event,
        SessionEvent::ClearNotice(notice)
            if notice.request_id == request_id && notice.clear_generation == 2
    )));
}
