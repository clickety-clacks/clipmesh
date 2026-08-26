use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use clipmesh_hub_core::{HistoryMode, HubCore, RequestIdentity, RetentionLimits};
use clipmesh_protocol::{AdministratorCredential, DeviceDisplayName, Platform};
use tempfile::tempdir;
use uuid::Uuid;

#[test]
fn an_external_edge_can_construct_and_drive_the_state_core() {
    let directory = tempdir().unwrap();
    let administrator = AdministratorCredential::from_wire(&format!(
        "cm_admin_v1_{}",
        URL_SAFE_NO_PAD.encode([7_u8; 32])
    ))
    .unwrap();
    let core = HubCore::open(
        directory.path().join("hub.sqlite3"),
        HistoryMode::Sqlite,
        RetentionLimits::default(),
        &administrator,
        1_700_000_000_000,
    )
    .unwrap();
    let created = core
        .create_managed_device(
            &administrator,
            RequestIdentity::new(Uuid::new_v4(), [1_u8; 32]).unwrap(),
            serde_json::from_str::<DeviceDisplayName>("\"Synthetic desktop\"").unwrap(),
            Platform::LinuxWayland,
            1_700_000_000_000,
        )
        .unwrap();
    let (session_id, principal) = core.open_session(&created.credential).unwrap();
    assert_eq!(principal.device_id, created.record.device_id);
    assert!(core
        .begin_resume(session_id, None, None, 1_700_000_000_000)
        .unwrap()
        .events
        .is_empty());
}
