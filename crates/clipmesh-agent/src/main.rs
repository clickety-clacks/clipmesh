use std::{
    env, fs,
    process::ExitCode,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clipmesh_agent::{
    drive_server_once, establish_live, send_observation, AgentConfig, AgentError, Platform,
    WebSocketTransport,
};
use clipmesh_agent_core::{
    AdapterError, AgentCore, AgentState, ClipboardAdapter, LocalObservation, PlatformRevision,
    ReconnectBackoff,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), AgentError> {
    let mut arguments = env::args_os().skip(1);
    if arguments.next().as_deref() != Some("--config".as_ref()) {
        return Err(AgentError::ConfigMissingRequired);
    }
    let path = arguments.next().ok_or(AgentError::ConfigMissingRequired)?;
    if arguments.next().is_some() {
        return Err(AgentError::ConfigUnknownField);
    }
    let text = fs::read_to_string(path).map_err(|_| AgentError::ConfigParseFailed)?;
    let config = AgentConfig::parse_toml(&text)?;
    run_platform(config)
}

trait Desktop: ClipboardAdapter {
    fn locked(&mut self) -> bool;
    fn observation(&mut self) -> Result<Option<LocalObservation>, AgentError>;
}

fn run_desktop<D: Desktop>(config: AgentConfig, mut desktop: D) -> Result<(), AgentError> {
    let mut core = AgentCore::open(&config.state_path)?;
    if desktop.locked() {
        core.set_locked(true);
    } else {
        core.start_unlocked();
    }
    let mut transport = None;
    let mut backoff = ReconnectBackoff::default();
    let mut last_revision: Option<PlatformRevision> = None;

    loop {
        let now_ms = unix_ms()?;
        let locked = desktop.locked();
        core.set_locked(locked);
        if locked {
            transport = None;
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        if core.state() == AgentState::ActiveUnlockedConnecting && transport.is_none() {
            let connection = WebSocketTransport::connect(&config).and_then(|mut connection| {
                establish_live(&mut core, &mut desktop, &mut connection, now_ms)?;
                Ok(connection)
            });
            match connection {
                Ok(connection) => {
                    backoff.entered_live(now_ms);
                    transport = Some(connection);
                }
                Err(_) => {
                    core.disconnect();
                    backoff.disconnected(now_ms);
                    thread::sleep(Duration::from_millis(
                        backoff.next_delay_ms(random_sample()),
                    ));
                    continue;
                }
            }
        }
        if let Some(connection) = transport.as_mut() {
            if let Some(observation) = desktop.observation()? {
                if last_revision.as_ref() != Some(&observation.revision) {
                    last_revision = Some(observation.revision.clone());
                    if send_observation(&mut core, &mut desktop, observation, connection, now_ms)
                        .is_err()
                    {
                        core.disconnect();
                        backoff.disconnected(now_ms);
                        transport = None;
                        thread::sleep(Duration::from_millis(
                            backoff.next_delay_ms(random_sample()),
                        ));
                        continue;
                    }
                }
            }
            if drive_server_once(&mut core, &mut desktop, connection, now_ms).is_err() {
                core.disconnect();
                backoff.disconnected(now_ms);
                transport = None;
                thread::sleep(Duration::from_millis(
                    backoff.next_delay_ms(random_sample()),
                ));
            }
        }
        if core.state() == AgentState::AdapterFailed {
            return Err(AgentError::AdapterUnavailable);
        }
    }
}

fn unix_ms() -> Result<i64, AgentError> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AgentError::LocalStateUnavailable)?
        .as_millis();
    value
        .try_into()
        .map_err(|_| AgentError::LocalStateUnavailable)
}

fn random_sample() -> u64 {
    uuid::Uuid::new_v4().as_u128() as u64
}

#[cfg(target_os = "linux")]
fn run_platform(config: AgentConfig) -> Result<(), AgentError> {
    use clipmesh_agent_linux::{LinuxSessionLockState, LockStateMonitor, WaylandClipboard};

    if config.platform != Platform::LinuxWayland {
        return Err(AgentError::ConfigValueInvalid);
    }
    struct LinuxDesktop {
        clipboard: WaylandClipboard,
        lock: LockStateMonitor<LinuxSessionLockState>,
    }
    impl ClipboardAdapter for LinuxDesktop {
        fn is_current(&mut self, revision: &PlatformRevision) -> Result<bool, AdapterError> {
            self.clipboard.is_current(revision)
        }
        fn write_text(&mut self, bytes: &[u8]) -> Result<PlatformRevision, AdapterError> {
            self.clipboard.write_text(bytes)
        }
    }
    impl Desktop for LinuxDesktop {
        fn locked(&mut self) -> bool {
            self.lock.poll_transition();
            self.lock.current().acts_locked()
        }
        fn observation(&mut self) -> Result<Option<LocalObservation>, AgentError> {
            self.clipboard
                .observe_text()
                .map_err(|_| AgentError::AdapterUnavailable)
        }
    }
    run_desktop(
        config,
        LinuxDesktop {
            clipboard: WaylandClipboard::connect().map_err(|_| AgentError::AdapterUnavailable)?,
            lock: LockStateMonitor::new(LinuxSessionLockState::for_current_process()),
        },
    )
}

#[cfg(target_os = "macos")]
fn run_platform(config: AgentConfig) -> Result<(), AgentError> {
    use clipmesh_agent_macos::{LockStateMonitor, MacPasteboard, MacSessionLockState};

    if config.platform != Platform::Macos {
        return Err(AgentError::ConfigValueInvalid);
    }
    struct MacDesktop {
        clipboard: MacPasteboard,
        lock: LockStateMonitor<MacSessionLockState>,
    }
    impl ClipboardAdapter for MacDesktop {
        fn is_current(&mut self, revision: &PlatformRevision) -> Result<bool, AdapterError> {
            self.clipboard.is_current(revision)
        }
        fn write_text(&mut self, bytes: &[u8]) -> Result<PlatformRevision, AdapterError> {
            self.clipboard.write_text(bytes)
        }
    }
    impl Desktop for MacDesktop {
        fn locked(&mut self) -> bool {
            self.lock.poll_transition();
            self.lock.current().acts_locked()
        }
        fn observation(&mut self) -> Result<Option<LocalObservation>, AgentError> {
            self.clipboard
                .observe_text()
                .map_err(|_| AgentError::AdapterUnavailable)
        }
    }
    run_desktop(
        config,
        MacDesktop {
            clipboard: MacPasteboard::general().map_err(|_| AgentError::AdapterUnavailable)?,
            lock: LockStateMonitor::new(MacSessionLockState),
        },
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn run_platform(_: AgentConfig) -> Result<(), AgentError> {
    Err(AgentError::ConfigValueInvalid)
}
