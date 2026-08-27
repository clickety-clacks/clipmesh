#[cfg(target_os = "macos")]
fn main() {
    use clipmesh_agent_core::ClipboardAdapter;
    use clipmesh_agent_macos::{LockStateSource, MacPasteboard, MacSessionLockState};

    let mut pasteboard = MacPasteboard::unique_for_capture().expect("adapter_unavailable");
    let before = pasteboard
        .current_change_count()
        .expect("adapter_unavailable");
    pasteboard
        .capture_seed_text("clipmesh-local-capture", "com.example.unverified")
        .expect("adapter_unavailable");
    let local = pasteboard
        .observe_text()
        .expect("adapter_unavailable")
        .expect("adapter_unavailable");
    let local_types = pasteboard
        .capture_declared_types()
        .expect("adapter_unavailable");
    let after_local = pasteboard
        .current_change_count()
        .expect("adapter_unavailable");
    let local_revision_current = pasteboard.is_current(&local.revision).unwrap();
    let remote_revision = pasteboard
        .write_text(b"clipmesh-remote-capture\nline-two")
        .expect("adapter_unavailable");
    let remote = pasteboard
        .observe_text()
        .expect("adapter_unavailable")
        .expect("adapter_unavailable");
    let after_remote = pasteboard
        .current_change_count()
        .expect("adapter_unavailable");
    let mut lock = MacSessionLockState;

    println!(
        "{{\"schema_version\":1,\"pasteboard_kind\":\"isolated_unique_native\",\"local_bytes_utf8\":{},\"local_declared_types\":{},\"local_hint\":\"{:?}\",\"local_revision_current_before_remote\":{},\"remote_bytes_utf8\":{},\"remote_revision_current\":{},\"change_count_monotonic\":{},\"lock_state\":\"{:?}\"}}",
        serde_json::to_string(std::str::from_utf8(&local.bytes).unwrap()).unwrap(),
        serde_json::to_string(&local_types).unwrap(),
        local.hint,
        local_revision_current,
        serde_json::to_string(std::str::from_utf8(&remote.bytes).unwrap()).unwrap(),
        pasteboard.is_current(&remote_revision).unwrap(),
        before < after_local && after_local < after_remote,
        lock.current_lock_state(),
    );
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("adapter_unavailable");
    std::process::exit(1);
}
