#[cfg(target_os = "linux")]
fn main() {
    use clipmesh_agent_core::ClipboardAdapter;
    use clipmesh_agent_linux::{LinuxSessionLockState, LockStateSource, WaylandClipboard};
    use serde::Serialize;
    use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};

    if std::env::var_os("CLIPMESH_WAYLAND_CAPTURE_ISOLATED").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        eprintln!("adapter_unavailable");
        std::process::exit(1);
    }

    const LOCAL: &[u8] = b"clipmesh-local-capture";
    const REMOTE: &[u8] = b"clipmesh-remote-capture\nline-two";

    let mut clipboard = WaylandClipboard::connect().expect("adapter_unavailable");
    Options::new()
        .copy_multi(vec![
            MimeSource {
                source: Source::Bytes(LOCAL.into()),
                mime_type: MimeType::Text,
            },
            MimeSource {
                source: Source::Bytes(LOCAL.into()),
                mime_type: MimeType::Specific("application/x-clipmesh-unverified".to_owned()),
            },
        ])
        .expect("adapter_unavailable");
    let local = loop {
        let observation = clipboard.next_observation().expect("adapter_unavailable");
        if observation.bytes == LOCAL {
            break observation;
        }
    };
    let local_mime_types = WaylandClipboard::capture_mime_types().expect("adapter_unavailable");
    let local_revision_current_before_remote = clipboard.is_current(&local.revision).unwrap();
    let remote_revision = clipboard.write_text(REMOTE).expect("adapter_unavailable");
    let remote = loop {
        let observation = clipboard.next_observation().expect("adapter_unavailable");
        if observation.bytes == REMOTE {
            break observation;
        }
    };
    let invalid_utf8_write_preserved = clipboard.write_text(&[0xff]).is_err()
        && clipboard.is_current(&remote_revision).unwrap()
        && clipboard
            .observe_text()
            .expect("adapter_unavailable")
            .is_some_and(|observation| observation.bytes == REMOTE);
    let mut lock = LinuxSessionLockState::for_current_process();
    let lock_state = lock.current_lock_state();

    #[derive(Serialize)]
    struct Capture<'a> {
        schema_version: u8,
        compositor_protocol: &'a str,
        clipboard_kind: &'a str,
        local_bytes_utf8: &'a str,
        local_mime_types: Vec<String>,
        local_hint: String,
        local_revision_current_before_remote: bool,
        remote_bytes_utf8: &'a str,
        remote_hint: String,
        remote_revision_current: bool,
        invalid_utf8_write_preserved: bool,
        lock_state: String,
        lock_state_acts_locked: bool,
    }

    let capture = Capture {
        schema_version: 1,
        compositor_protocol: "wlr-data-control-v1",
        clipboard_kind: "isolated_headless_wayland",
        local_bytes_utf8: std::str::from_utf8(&local.bytes).unwrap(),
        local_mime_types,
        local_hint: format!("{:?}", local.hint),
        local_revision_current_before_remote,
        remote_bytes_utf8: std::str::from_utf8(&remote.bytes).unwrap(),
        remote_hint: format!("{:?}", remote.hint),
        remote_revision_current: clipboard.is_current(&remote_revision).unwrap(),
        invalid_utf8_write_preserved,
        lock_state: format!("{lock_state:?}"),
        lock_state_acts_locked: lock_state.acts_locked(),
    };
    println!("{}", serde_json::to_string(&capture).unwrap());
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("adapter_unavailable");
    std::process::exit(1);
}
