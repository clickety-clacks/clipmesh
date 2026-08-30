use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use tempfile::TempDir;

#[test]
fn startup_rejects_complete_config_mutation_matrix_before_network() {
    let fixture = TempDir::new().unwrap();
    let state = fixture.path().join("state.sqlite3");
    let control = fixture.path().join("control.sock");
    let valid = format!(
        "config_version = 1\nhub_url = 'ws://100.64.0.7:4357/v1/stream'\nplatform = 'linux-wayland'\nstate_path = '{}'\ncontrol_socket = '{}'\n",
        state.display(),
        control.display()
    );
    let mutations = [
        ("missing", String::new(), "config_missing_required"),
        (
            "unknown",
            format!("{valid}unexpected = true\n"),
            "config_unknown_field",
        ),
        (
            "syntax",
            valid.replace("config_version = 1", "config_version = ["),
            "config_parse_failed",
        ),
        (
            "duplicate",
            format!("{valid}config_version = 1\n"),
            "config_parse_failed",
        ),
        (
            "type",
            valid.replace("config_version = 1", "config_version = 'one'"),
            "config_value_invalid",
        ),
        (
            "version",
            valid.replace("config_version = 1", "config_version = 2"),
            "config_value_invalid",
        ),
        (
            "platform",
            valid.replace("linux-wayland", "other"),
            "config_value_invalid",
        ),
        (
            "secure-scheme",
            valid.replace("ws://", "wss://"),
            "config_value_invalid",
        ),
        (
            "http-scheme",
            valid.replacen("ws", "http", 1),
            "config_value_invalid",
        ),
        (
            "hostname",
            valid.replace("100.64.0.7", "hub.example.invalid"),
            "config_value_invalid",
        ),
        (
            "outside-tailnet",
            valid.replace("100.64.0.7", "192.0.2.7"),
            "config_value_invalid",
        ),
        (
            "missing-port",
            valid.replace(":4357", ""),
            "config_value_invalid",
        ),
        (
            "zero-port",
            valid.replace(":4357", ":0"),
            "config_value_invalid",
        ),
        (
            "large-port",
            valid.replace(":4357", ":65536"),
            "config_value_invalid",
        ),
        (
            "path",
            valid.replace("/v1/stream", "/v1/other"),
            "config_value_invalid",
        ),
        (
            "query",
            valid.replace("/v1/stream", "/v1/stream?mode=other"),
            "config_value_invalid",
        ),
        (
            "fragment",
            valid.replace("/v1/stream", "/v1/stream#other"),
            "config_value_invalid",
        ),
        (
            "user-info",
            valid.replace("100.64.0.7", "user@100.64.0.7"),
            "config_value_invalid",
        ),
        (
            "relative-state",
            valid.replace(&state.display().to_string(), "state.sqlite3"),
            "config_value_invalid",
        ),
        (
            "relative-control",
            valid.replace(&control.display().to_string(), "control.sock"),
            "config_value_invalid",
        ),
        (
            "same-local-path",
            valid.replace(&control.display().to_string(), &state.display().to_string()),
            "config_value_invalid",
        ),
    ];

    for (name, content, expected) in mutations {
        let config = fixture.path().join(format!("{name}.toml"));
        fs::write(&config, content).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_clipmesh-agent"))
            .args(["--config", config.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{name} unexpectedly succeeded");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap().trim(),
            expected,
            "{name} returned the wrong stable code"
        );
        assert!(
            !state.exists(),
            "{name} opened local state before rejection"
        );
        assert!(
            !control.exists(),
            "{name} opened local control before rejection"
        );
    }
}

#[test]
fn startup_rejects_insecure_state_before_platform_adapter() {
    let fixture = TempDir::new().unwrap();
    fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let state = fixture.path().join("state.sqlite3");
    let control = fixture.path().join("control.sock");
    let config = fixture.path().join("insecure-state.toml");
    fs::write(
        &config,
        format!(
            "config_version = 1\nhub_url = 'ws://100.64.0.7:4357/v1/stream'\nplatform = 'linux-wayland'\nstate_path = '{}'\ncontrol_socket = '{}'\n",
            state.display(),
            control.display()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_clipmesh-agent"))
        .args(["--config", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "state_path_insecure"
    );
    assert!(!state.exists());
    assert!(!control.exists());
}

#[test]
fn startup_opens_secure_state_and_control_before_platform_adapter() {
    let fixture = TempDir::new().unwrap();
    fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let state = fixture.path().join("state.sqlite3");
    let control = fixture.path().join("control.sock");
    let config = fixture.path().join("secure-state.toml");
    fs::write(
        &config,
        format!(
            "config_version = 1\nhub_url = 'ws://100.64.0.7:4357/v1/stream'\nplatform = 'linux-wayland'\nstate_path = '{}'\ncontrol_socket = '{}'\n",
            state.display(),
            control.display()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_clipmesh-agent"))
        .args(["--config", config.to_str().unwrap()])
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "adapter_unavailable"
    );
    assert!(state.exists());
    assert!(!control.exists(), "control socket must be removed at exit");
}
