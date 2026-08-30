#[cfg(target_os = "linux")]
fn main() {
    use clipmesh_agent_linux::{
        LinuxSessionLockState, LockState, LockStateSource, WaylandClipboard,
    };
    use serde::Serialize;
    use zbus::{
        blocking::Proxy,
        zvariant::{OwnedFd, OwnedObjectPath, OwnedValue},
    };

    const BUS_ADDRESS: &str = "unix:path=/run/dbus/system_bus_socket";
    if std::env::var_os("CLIPMESH_LOGIND_CAPTURE_ISOLATED").as_deref()
        != Some(std::ffi::OsStr::new("1"))
        || std::env::var_os("CLIPMESH_WAYLAND_CAPTURE_ISOLATED").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        || !matches!(
            std::env::var("DBUS_SYSTEM_BUS_ADDRESS").as_deref(),
            Ok(BUS_ADDRESS)
        )
        || !matches!(
            std::fs::read_to_string("/run/clipmesh-r4-private-logind").as_deref(),
            Ok("isolated\n")
        )
    {
        eprintln!("adapter_unavailable");
        std::process::exit(1);
    }

    let _wayland = WaylandClipboard::connect().expect("adapter_unavailable");
    let connection = zbus::blocking::Connection::system().expect("adapter_unavailable");
    let manager = Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .expect("adapter_unavailable");
    let properties: Vec<(String, OwnedValue)> = Vec::new();
    let (session_id, session_path, _runtime_path, session_fifo, _uid, _seat, _vtnr, _existing): (
        String,
        OwnedObjectPath,
        String,
        OwnedFd,
        u32,
        String,
        u32,
        bool,
    ) = manager
        .call(
            "CreateSession",
            &(
                0u32,
                std::process::id(),
                "clipmesh-r4-capture",
                "wayland",
                "user",
                "sway",
                "",
                0u32,
                "",
                "",
                false,
                "",
                "",
                properties,
            ),
        )
        .expect("adapter_unavailable");
    let session = Proxy::new(
        &connection,
        "org.freedesktop.login1",
        session_path.as_str(),
        "org.freedesktop.login1.Session",
    )
    .expect("adapter_unavailable");
    let mut lock = LinuxSessionLockState::for_current_process();
    let unlocked_before = lock.current_lock_state();
    session
        .call::<_, _, ()>("SetLockedHint", &(true,))
        .expect("adapter_unavailable");
    let locked = lock.current_lock_state();
    session
        .call::<_, _, ()>("SetLockedHint", &(false,))
        .expect("adapter_unavailable");
    let unlocked_after = lock.current_lock_state();

    #[derive(Serialize)]
    struct Capture {
        schema_version: u8,
        platform: &'static str,
        compositor_protocol: &'static str,
        provider: &'static str,
        session_kind: &'static str,
        states: [String; 3],
        locked_acts_locked: bool,
        unlocked_acts_locked: bool,
    }

    let capture = Capture {
        schema_version: 1,
        platform: "linux-wayland",
        compositor_protocol: "wlr-data-control-v1",
        provider: "systemd-logind",
        session_kind: "isolated_private_bus",
        states: [
            format!("{unlocked_before:?}"),
            format!("{locked:?}"),
            format!("{unlocked_after:?}"),
        ],
        locked_acts_locked: locked.acts_locked(),
        unlocked_acts_locked: unlocked_after.acts_locked(),
    };
    assert_eq!(unlocked_before, LockState::Unlocked);
    assert_eq!(locked, LockState::Locked);
    assert_eq!(unlocked_after, LockState::Unlocked);
    println!("{}", serde_json::to_string(&capture).unwrap());

    manager
        .call::<_, _, ()>("ReleaseSession", &(session_id,))
        .expect("adapter_unavailable");
    drop(session_fifo);
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("adapter_unavailable");
    std::process::exit(1);
}
