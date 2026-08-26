use clipmesh_hub_core::{ClipContentV1, HubCore, PublishInput, RetentionLimits, StablePeerId};
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
}
