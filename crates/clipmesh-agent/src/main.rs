use std::{
    env, fs,
    process::ExitCode,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clipmesh_agent::{
    drive_server_once, establish_live, send_observation, send_shared_clear, AgentConfig,
    AgentError, Platform, WebSocketTransport,
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
    fn file_revision(&mut self) -> Result<Option<PlatformRevision>, AgentError> {
        Ok(None)
    }
    fn apply_files(
        &mut self,
        _paths: &[std::path::PathBuf],
        _revision: &PlatformRevision,
    ) -> Result<Option<PlatformRevision>, AgentError> {
        Ok(None)
    }
    fn file_observation(
        &mut self,
    ) -> Result<Option<(Vec<std::path::PathBuf>, PlatformRevision)>, AgentError> {
        Ok(None)
    }
    fn locked(&mut self) -> bool;
    fn observation(&mut self) -> Result<Option<LocalObservation>, AgentError>;
    fn service_control(&mut self, core: &mut AgentCore) -> Result<bool, AgentError>;
}

fn run_desktop<D: Desktop>(
    config: AgentConfig,
    mut core: AgentCore,
    mut desktop: D,
) -> Result<(), AgentError> {
    if desktop.locked() {
        core.set_locked(true);
    } else {
        core.start_unlocked();
    }
    let mut transport = None;
    let mut backoff = ReconnectBackoff::default();
    let mut last_revision: Option<PlatformRevision> = None;
    let mut shared_clear_pending = false;
    let mut last_file_revision = None;
    let mut file_arrivals = clipmesh_agent::files::FileArrivals::default();
    // Retain ownership while the pasteboard points at these local files.
    let mut _received_files = None;
    enum FileResult {
        History(Vec<clipmesh_protocol::files::FileHistoryEntry>),
        Download(clipmesh_agent::files::ReceivedSelection),
    }
    let mut file_job: Option<(
        thread::JoinHandle<Result<FileResult, AgentError>>,
        PlatformRevision,
        Option<uuid::Uuid>,
        Option<u64>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    )> = None;
    let mut next_file_poll = std::time::Instant::now();
    let mut file_upload: Option<(
        thread::JoinHandle<()>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    )> = None;

    loop {
        let now_ms = unix_ms()?;
        shared_clear_pending |= desktop.service_control(&mut core)?;
        let locked = desktop.locked();
        core.set_locked(locked);
        if locked || core.state() != AgentState::ActiveUnlockedLive || shared_clear_pending {
            file_arrivals.reset_connection();
            if let Some((_, _, _, _, cancelled)) = &file_job {
                cancelled.store(true, std::sync::atomic::Ordering::Release);
            }
        }
        if file_job
            .as_ref()
            .is_some_and(|(job, _, _, _, _)| job.is_finished())
        {
            let (job, revision, epoch, generation, cancelled) = file_job.take().unwrap();
            let snapshot = core
                .snapshot()
                .map_err(|_| AgentError::AdapterUnavailable)?;
            let valid = !cancelled.load(std::sync::atomic::Ordering::Acquire)
                && !locked
                && core.state() == AgentState::ActiveUnlockedLive
                && !shared_clear_pending
                && snapshot.history_epoch == epoch
                && snapshot.clear_generation == generation;
            if let Ok(Ok(result)) = job.join() {
                if valid {
                    match result {
                        FileResult::History(clips) => {
                            if let Some(clip) = file_arrivals.observe(&clips) {
                                if desktop.is_current(&revision).unwrap_or(false) {
                                    let config = config.clone();
                                    let cancel = cancelled.clone();
                                    let job = thread::spawn(move || {
                                        let mut client =
                                            clipmesh_agent::files::FileTransport::connect(&config)?;
                                        client.set_cancellation(cancel);
                                        let root = config
                                            .state_path
                                            .parent()
                                            .ok_or(AgentError::AdapterUnavailable)?;
                                        client
                                            .receive_selection(&clip, root)
                                            .map(FileResult::Download)
                                    });
                                    file_job = Some((job, revision, epoch, generation, cancelled));
                                }
                            }
                        }
                        FileResult::Download(selection) => {
                            if let Some(applied) =
                                desktop.apply_files(selection.paths(), &revision)?
                            {
                                last_file_revision = Some(applied);
                                _received_files = Some(selection);
                            }
                        }
                    }
                }
            }
        }
        let file_clipboard_changed = file_upload.is_some()
            && last_file_revision
                .as_ref()
                .is_some_and(|revision| !desktop.is_current(revision).unwrap_or(false));
        if locked
            || core.state() != AgentState::ActiveUnlockedLive
            || shared_clear_pending
            || file_clipboard_changed
        {
            if let Some((_, cancelled)) = &file_upload {
                cancelled.store(true, std::sync::atomic::Ordering::Release);
            }
        }
        if file_upload
            .as_ref()
            .is_some_and(|(worker, _)| worker.is_finished())
        {
            if let Some((worker, _)) = file_upload.take() {
                let _ = worker.join();
            }
        }
        if locked {
            transport = None;
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        if core.state() == AgentState::LocallyPaused {
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
                    // Files copied before startup, unlock, or reconnection
                    // are a baseline, not fresh publish intent.
                    last_file_revision = desktop.file_revision()?;
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
        if shared_clear_pending {
            if let Some(connection) = transport.as_mut() {
                if send_shared_clear(&core, connection).is_err() {
                    core.disconnect();
                    backoff.disconnected(now_ms);
                    transport = None;
                    thread::sleep(Duration::from_millis(
                        backoff.next_delay_ms(random_sample()),
                    ));
                    continue;
                }
                shared_clear_pending = false;
            }
        }
        if let Some(connection) = transport.as_mut() {
            if file_job.is_none() && std::time::Instant::now() >= next_file_poll {
                next_file_poll = std::time::Instant::now() + Duration::from_secs(3);
                if let Some(revision) = desktop.file_revision()? {
                    let snapshot = core
                        .snapshot()
                        .map_err(|_| AgentError::AdapterUnavailable)?;
                    let file_config = config.clone();
                    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let cancel = cancelled.clone();
                    let job = thread::spawn(move || {
                        let mut client =
                            clipmesh_agent::files::FileTransport::connect(&file_config)?;
                        client.set_cancellation(cancel);
                        client.history().map(FileResult::History)
                    });
                    file_job = Some((
                        job,
                        revision,
                        snapshot.history_epoch,
                        snapshot.clear_generation,
                        cancelled,
                    ));
                }
            }
            if file_upload.is_none() && core.state() == AgentState::ActiveUnlockedLive {
                if let Some((paths, revision)) = desktop.file_observation()? {
                    if last_file_revision.as_ref() != Some(&revision)
                        && desktop.is_current(&revision).unwrap_or(false)
                    {
                        last_file_revision = Some(revision);
                        if core.allow_file_observation() {
                            let cancelled =
                                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                            let worker_cancelled = cancelled.clone();
                            let file_config = config.clone();
                            let clip_id = clipmesh_protocol::UuidV4::new();
                            file_arrivals.note_local_publish(clip_id.clone());
                            let worker = thread::spawn(move || {
                                let result = (|| {
                                    if worker_cancelled.load(std::sync::atomic::Ordering::Acquire) {
                                        return Ok(());
                                    }
                                    let selection = clipmesh_agent::files::read_selection(&paths)?;
                                    if worker_cancelled.load(std::sync::atomic::Ordering::Acquire) {
                                        return Ok(());
                                    }
                                    let mut client = clipmesh_agent::files::FileTransport::connect(
                                        &file_config,
                                    )?;
                                    client.set_cancellation(worker_cancelled);
                                    client.publish(clip_id, &selection)
                                })();
                                if let Err(error) = result {
                                    eprintln!("file_upload_failed: {error}");
                                }
                            });
                            file_upload = Some((worker, cancelled));
                        }
                    }
                }
            }
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
    use clipmesh_agent_linux::{
        apply_control, ControlOutcome, LinuxAdapterError, LinuxSessionLockState, LockStateMonitor,
        OwnerControlSocket, WaylandClipboard,
    };

    if config.platform != Platform::LinuxWayland {
        return Err(AgentError::ConfigValueInvalid);
    }
    struct LinuxDesktop {
        clipboard: WaylandClipboard,
        lock: LockStateMonitor<LinuxSessionLockState>,
        control: OwnerControlSocket,
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
        fn file_revision(&mut self) -> Result<Option<PlatformRevision>, AgentError> {
            self.clipboard
                .current_revision()
                .map_err(|_| AgentError::AdapterUnavailable)
        }
        fn file_observation(
            &mut self,
        ) -> Result<Option<(Vec<std::path::PathBuf>, PlatformRevision)>, AgentError> {
            self.clipboard
                .observe_files()
                .map_err(|_| AgentError::AdapterUnavailable)
        }
        fn apply_files(
            &mut self,
            paths: &[std::path::PathBuf],
            revision: &PlatformRevision,
        ) -> Result<Option<PlatformRevision>, AgentError> {
            self.clipboard
                .write_files_if_current(paths, revision)
                .map_err(|_| AgentError::AdapterUnavailable)
        }
        fn locked(&mut self) -> bool {
            self.lock.poll_transition();
            self.lock.current().acts_locked()
        }
        fn observation(&mut self) -> Result<Option<LocalObservation>, AgentError> {
            self.clipboard
                .observe_text()
                .map_err(|_| AgentError::AdapterUnavailable)
        }
        fn service_control(&mut self, core: &mut AgentCore) -> Result<bool, AgentError> {
            let accepted = match self.control.try_accept_command() {
                Ok(accepted) => accepted,
                Err(
                    LinuxAdapterError::ControlRequestInvalid | LinuxAdapterError::StatePathInsecure,
                ) => return Ok(false),
                Err(LinuxAdapterError::LocalStateUnavailable) => {
                    return Err(AgentError::LocalStateUnavailable)
                }
                Err(_) => return Err(AgentError::AdapterUnavailable),
            };
            let Some((stream, command)) = accepted else {
                return Ok(false);
            };
            let outcome = apply_control(core, command)?;
            let shared_clear = outcome == ControlOutcome::SharedClearRequested;
            let _ = OwnerControlSocket::respond(stream, outcome);
            Ok(shared_clear)
        }
    }

    let core = AgentCore::open(&config.state_path)?;
    let control =
        OwnerControlSocket::bind(&config.control_socket).map_err(|error| match error {
            LinuxAdapterError::StatePathInsecure => AgentError::StatePathInsecure,
            LinuxAdapterError::LocalStateUnavailable => AgentError::LocalStateUnavailable,
            _ => AgentError::AdapterUnavailable,
        })?;
    control
        .set_nonblocking(true)
        .map_err(|_| AgentError::LocalStateUnavailable)?;
    run_desktop(
        config,
        core,
        LinuxDesktop {
            clipboard: WaylandClipboard::connect().map_err(|_| AgentError::AdapterUnavailable)?,
            lock: LockStateMonitor::new(LinuxSessionLockState::for_current_process()),
            control,
        },
    )
}

#[cfg(target_os = "macos")]
fn run_platform(config: AgentConfig) -> Result<(), AgentError> {
    use clipmesh_agent_macos::{
        apply_control, ControlOutcome, LockStateMonitor, MacAdapterError, MacPasteboard,
        MacSessionLockState, OwnerControlSocket,
    };

    if config.platform != Platform::Macos {
        return Err(AgentError::ConfigValueInvalid);
    }
    struct MacDesktop {
        clipboard: MacPasteboard,
        lock: LockStateMonitor<MacSessionLockState>,
        control: OwnerControlSocket,
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
        fn file_revision(&mut self) -> Result<Option<PlatformRevision>, AgentError> {
            self.clipboard
                .current_revision()
                .map(Some)
                .map_err(|_| AgentError::AdapterUnavailable)
        }
        fn apply_files(
            &mut self,
            paths: &[std::path::PathBuf],
            revision: &PlatformRevision,
        ) -> Result<Option<PlatformRevision>, AgentError> {
            self.clipboard
                .write_files_if_current(paths, revision)
                .map_err(|_| AgentError::AdapterUnavailable)
        }
        fn file_observation(
            &mut self,
        ) -> Result<Option<(Vec<std::path::PathBuf>, PlatformRevision)>, AgentError> {
            self.clipboard
                .observe_files()
                .map_err(|_| AgentError::AdapterUnavailable)
        }
        fn locked(&mut self) -> bool {
            self.lock.poll_transition();
            self.lock.current().acts_locked()
        }
        fn observation(&mut self) -> Result<Option<LocalObservation>, AgentError> {
            self.clipboard
                .observe_text()
                .map_err(|_| AgentError::AdapterUnavailable)
        }
        fn service_control(&mut self, core: &mut AgentCore) -> Result<bool, AgentError> {
            let accepted = match self.control.try_accept_command() {
                Ok(accepted) => accepted,
                Err(
                    MacAdapterError::ControlRequestInvalid | MacAdapterError::StatePathInsecure,
                ) => return Ok(false),
                Err(MacAdapterError::LocalStateUnavailable) => {
                    return Err(AgentError::LocalStateUnavailable)
                }
                Err(_) => return Err(AgentError::AdapterUnavailable),
            };
            let Some((stream, command)) = accepted else {
                return Ok(false);
            };
            let outcome = apply_control(core, command)?;
            let shared_clear = outcome == ControlOutcome::SharedClearRequested;
            let _ = OwnerControlSocket::respond(stream, outcome);
            Ok(shared_clear)
        }
    }

    let core = AgentCore::open(&config.state_path)?;
    let control =
        OwnerControlSocket::bind(&config.control_socket).map_err(|error| match error {
            MacAdapterError::StatePathInsecure => AgentError::StatePathInsecure,
            MacAdapterError::LocalStateUnavailable => AgentError::LocalStateUnavailable,
            _ => AgentError::AdapterUnavailable,
        })?;
    control
        .set_nonblocking(true)
        .map_err(|_| AgentError::LocalStateUnavailable)?;
    run_desktop(
        config,
        core,
        MacDesktop {
            clipboard: MacPasteboard::general().map_err(|_| AgentError::AdapterUnavailable)?,
            lock: LockStateMonitor::new(MacSessionLockState),
            control,
        },
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn run_platform(_: AgentConfig) -> Result<(), AgentError> {
    Err(AgentError::ConfigValueInvalid)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod file_loop_tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use clipmesh_agent_linux::WaylandClipboard as MacPasteboard;
    #[cfg(target_os = "macos")]
    use clipmesh_agent_macos::MacPasteboard;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct IsolatedDesktop {
        clipboard: MacPasteboard,
        delivered: Arc<AtomicBool>,
        deadline: std::time::Instant,
    }
    impl ClipboardAdapter for IsolatedDesktop {
        fn is_current(&mut self, revision: &PlatformRevision) -> Result<bool, AdapterError> {
            self.clipboard.is_current(revision)
        }
        fn write_text(&mut self, bytes: &[u8]) -> Result<PlatformRevision, AdapterError> {
            self.clipboard.write_text(bytes)
        }
    }
    impl Desktop for IsolatedDesktop {
        fn locked(&mut self) -> bool {
            false
        }
        fn observation(&mut self) -> Result<Option<LocalObservation>, AgentError> {
            Ok(None)
        }
        fn file_revision(&mut self) -> Result<Option<PlatformRevision>, AgentError> {
            #[cfg(target_os = "macos")]
            {
                self.clipboard
                    .current_revision()
                    .map(Some)
                    .map_err(|_| AgentError::AdapterUnavailable)
            }
            #[cfg(target_os = "linux")]
            self.clipboard
                .current_revision()
                .map_err(|_| AgentError::AdapterUnavailable)
        }
        fn apply_files(
            &mut self,
            paths: &[std::path::PathBuf],
            revision: &PlatformRevision,
        ) -> Result<Option<PlatformRevision>, AgentError> {
            self.clipboard
                .write_files_if_current(paths, revision)
                .map_err(|_| AgentError::AdapterUnavailable)
        }
        fn service_control(&mut self, _: &mut AgentCore) -> Result<bool, AgentError> {
            if let Some((paths, _)) = self.clipboard.observe_files().unwrap() {
                assert_eq!(paths.len(), 1);
                assert_eq!(std::fs::read(&paths[0]).unwrap(), [0, 255, 128, 7]);
                self.delivered.store(true, Ordering::Release);
                return Err(AgentError::AdapterUnavailable); // End the real loop after verification.
            }
            if std::time::Instant::now() >= self.deadline {
                return Err(AgentError::AdapterUnavailable);
            }
            Ok(false)
        }
    }

    #[test]
    #[ignore = "requires CLIPMESH_TEST_HUB_URL pointing to an isolated hub"]
    fn running_agent_delivers_new_file_to_isolated_clipboard() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = env::var("CLIPMESH_TEST_HUB_URL").unwrap();
        #[cfg(target_os = "macos")]
        let platform = "macos";
        #[cfg(target_os = "linux")]
        let platform = {
            assert_eq!(env::var("CLIPMESH_ISOLATED_WAYLAND").unwrap(), "1");
            "linux-wayland"
        };
        let config = AgentConfig::parse_toml(&format!(
            "config_version=1\nhub_url={endpoint:?}\nplatform={platform:?}\nstate_path={:?}\ncontrol_socket={:?}\n",
            root.path().join("state.sqlite"), root.path().join("control.sock"))).unwrap();
        let core = AgentCore::open(&config.state_path).unwrap();
        let delivered = Arc::new(AtomicBool::new(false));
        #[cfg(target_os = "macos")]
        let clipboard = MacPasteboard::unique_for_capture().unwrap();
        #[cfg(target_os = "linux")]
        let clipboard = {
            let mut clipboard = MacPasteboard::connect().unwrap();
            clipboard.write_text(b"synthetic loop baseline").unwrap();
            clipboard
        };
        let desktop = IsolatedDesktop {
            clipboard,
            delivered: delivered.clone(),
            deadline: std::time::Instant::now() + Duration::from_secs(25),
        };
        let sender_config = config.clone();
        let fixture = root.path().join("loop-fixture.bin");
        fs::write(&fixture, [0, 255, 128, 7]).unwrap();
        let sender = thread::spawn(move || {
            // Let the first history poll establish its no-replay baseline.
            thread::sleep(Duration::from_secs(6));
            let selection = clipmesh_agent::files::read_selection(&[fixture]).unwrap();
            clipmesh_agent::files::FileTransport::connect(&sender_config)
                .unwrap()
                .publish(clipmesh_protocol::UuidV4::new(), &selection)
                .unwrap();
        });
        let _ = run_desktop(config, core, desktop);
        sender.join().unwrap();
        assert!(
            delivered.load(Ordering::Acquire),
            "running agent did not deliver the new file"
        );
    }

    #[cfg(target_os = "linux")]
    struct LinuxImageDesktop {
        clipboard: MacPasteboard,
        received_root: std::path::PathBuf,
        received_name: String,
        delivered: Arc<AtomicBool>,
        deadline: std::time::Instant,
        stop_at: Option<std::time::Instant>,
    }

    #[cfg(target_os = "linux")]
    impl ClipboardAdapter for LinuxImageDesktop {
        fn is_current(&mut self, revision: &PlatformRevision) -> Result<bool, AdapterError> {
            self.clipboard.is_current(revision)
        }

        fn write_text(&mut self, bytes: &[u8]) -> Result<PlatformRevision, AdapterError> {
            self.clipboard.write_text(bytes)
        }
    }

    #[cfg(target_os = "linux")]
    impl Desktop for LinuxImageDesktop {
        fn file_revision(&mut self) -> Result<Option<PlatformRevision>, AgentError> {
            self.clipboard
                .current_revision()
                .map_err(|_| AgentError::AdapterUnavailable)
        }

        fn file_observation(
            &mut self,
        ) -> Result<Option<(Vec<std::path::PathBuf>, PlatformRevision)>, AgentError> {
            self.clipboard
                .observe_files()
                .map_err(|_| AgentError::AdapterUnavailable)
        }

        fn apply_files(
            &mut self,
            paths: &[std::path::PathBuf],
            revision: &PlatformRevision,
        ) -> Result<Option<PlatformRevision>, AgentError> {
            self.clipboard
                .write_files_if_current(paths, revision)
                .map_err(|_| AgentError::AdapterUnavailable)
        }

        fn locked(&mut self) -> bool {
            false
        }

        fn observation(&mut self) -> Result<Option<LocalObservation>, AgentError> {
            Ok(None)
        }

        fn service_control(&mut self, _: &mut AgentCore) -> Result<bool, AgentError> {
            if let Some((paths, _)) = self.clipboard.observe_files().unwrap() {
                if paths.len() == 1
                    && paths[0].starts_with(&self.received_root)
                    && paths[0].file_name().and_then(|name| name.to_str())
                        == Some(self.received_name.as_str())
                {
                    if !self.delivered.swap(true, Ordering::AcqRel) {
                        assert_eq!(std::fs::read(&paths[0]).unwrap(), ONE_BY_ONE_PNG);
                        assert!(self.clipboard.observe_text().unwrap().is_none());
                        let mimes = MacPasteboard::capture_mime_types().unwrap();
                        assert!(mimes.iter().any(|mime| mime == "image/png"));
                        assert!(mimes.iter().any(|mime| mime == "text/uri-list"));
                        assert!(mimes.iter().any(|mime| {
                            mime.starts_with("application/x-clipmesh-write-marker-")
                        }));
                        // Leave the loop alive for two file-poll intervals so
                        // a remote selection cannot immediately echo back.
                        self.stop_at = Some(std::time::Instant::now() + Duration::from_secs(7));
                    }
                }
            }
            if self
                .stop_at
                .is_some_and(|stop_at| std::time::Instant::now() >= stop_at)
            {
                return Err(AgentError::AdapterUnavailable);
            }
            if std::time::Instant::now() >= self.deadline {
                return Err(AgentError::AdapterUnavailable);
            }
            Ok(false)
        }
    }

    #[cfg(target_os = "linux")]
    const ONE_BY_ONE_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5,
        0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64,
        0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[cfg(target_os = "linux")]
    fn publish_raw_png(bytes: &[u8]) {
        use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};

        Options::new()
            .copy_multi(vec![MimeSource {
                source: Source::Bytes(bytes.to_vec().into_boxed_slice()),
                mime_type: MimeType::Specific("image/png".to_owned()),
            }])
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires CLIPMESH_TEST_HUB_URL, CLIPMESH_ISOLATED_WAYLAND=1, and a private compositor"]
    fn running_agent_uploads_and_receives_png_without_echo() {
        use clipmesh_protocol::files::FileDescriptor;
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(env::var("CLIPMESH_ISOLATED_WAYLAND").unwrap(), "1");
        let endpoint = env::var("CLIPMESH_TEST_HUB_URL").unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let config = AgentConfig::parse_toml(&format!(
            "config_version=1\nhub_url={endpoint:?}\nplatform=\"linux-wayland\"\nstate_path={:?}\ncontrol_socket={:?}",
            root.path().join("state.sqlite"),
            root.path().join("control.sock")
        ))
        .unwrap();
        let core = AgentCore::open(&config.state_path).unwrap();
        let delivered = Arc::new(AtomicBool::new(false));
        let remote_id = clipmesh_protocol::UuidV4::new();
        let remote_name = format!("remote-{}.png", remote_id.get());
        let mut clipboard = MacPasteboard::connect().unwrap();
        clipboard.write_text(b"synthetic loop baseline").unwrap();
        let desktop = LinuxImageDesktop {
            clipboard,
            received_root: root.path().to_owned(),
            received_name: remote_name.clone(),
            delivered: delivered.clone(),
            deadline: std::time::Instant::now() + Duration::from_secs(35),
            stop_at: None,
        };

        let sender_config = config.clone();
        let sender_delivered = delivered.clone();
        let sender = thread::spawn(move || -> Result<(), String> {
            thread::sleep(Duration::from_secs(6));
            let _source = MacPasteboard::connect().map_err(|_| "source connect".to_owned())?;
            let mut baseline_client = clipmesh_agent::files::FileTransport::connect(&sender_config)
                .map_err(|_| "baseline connect".to_owned())?;
            let baseline_ids = baseline_client
                .history()
                .map_err(|_| "baseline history".to_owned())?
                .into_iter()
                .map(|clip| clip.clip_id)
                .collect::<std::collections::HashSet<_>>();
            drop(baseline_client);
            publish_raw_png(ONE_BY_ONE_PNG);
            let local_deadline = std::time::Instant::now() + Duration::from_secs(15);
            let mut local_verified = false;
            while std::time::Instant::now() < local_deadline {
                if let Ok(mut client) =
                    clipmesh_agent::files::FileTransport::connect(&sender_config)
                {
                    if let Ok(clips) = client.history() {
                        for clip in clips {
                            if clip.manifest.files.len() == 1
                                && !baseline_ids.contains(&clip.clip_id)
                                && clip.manifest.files[0].name == "Screenshot.png"
                                && clip.manifest.files[0].media_type == "image/png"
                                && client.download(&clip, 0).ok().as_deref() == Some(ONE_BY_ONE_PNG)
                            {
                                local_verified = true;
                                break;
                            }
                        }
                    }
                }
                if local_verified {
                    break;
                }
                thread::sleep(Duration::from_millis(200));
            }
            if !local_verified {
                return Err("local PNG upload was not observed in hub history".to_owned());
            }

            let remote = FileDescriptor {
                name: remote_name.clone(),
                media_type: "image/png".to_owned(),
                size_bytes: ONE_BY_ONE_PNG.len() as u64,
                sha256: format!("{:x}", Sha256::digest(ONE_BY_ONE_PNG)),
            };
            let mut client = clipmesh_agent::files::FileTransport::connect(&sender_config)
                .map_err(|_| "remote connect".to_owned())?;
            client
                .publish(remote_id.clone(), &[(remote, ONE_BY_ONE_PNG.to_vec())])
                .map_err(|_| "remote publish".to_owned())?;

            let delivery_deadline = std::time::Instant::now() + Duration::from_secs(15);
            while !sender_delivered.load(Ordering::Acquire)
                && std::time::Instant::now() < delivery_deadline
            {
                thread::sleep(Duration::from_millis(50));
            }
            if !sender_delivered.load(Ordering::Acquire) {
                return Err("remote PNG was not applied to the clipboard".to_owned());
            }
            // The desktop loop remains alive for seven seconds after the
            // remote selection arrives. Check after that window closes.
            thread::sleep(Duration::from_secs(8));
            let mut client = clipmesh_agent::files::FileTransport::connect(&sender_config)
                .map_err(|_| "echo-check connect".to_owned())?;
            let matching_clips = client
                .history()
                .map_err(|_| "echo-check history".to_owned())?
                .into_iter()
                .filter(|clip| {
                    !baseline_ids.contains(&clip.clip_id)
                        && clip.manifest.files.len() == 1
                        && clip.manifest.files[0].name == remote_name
                        && clip.manifest.files[0].media_type == "image/png"
                })
                .collect::<Vec<_>>();
            if matching_clips.len() != 1
                || matching_clips[0].clip_id != remote_id
                || client.download(&matching_clips[0], 0).ok().as_deref() != Some(ONE_BY_ONE_PNG)
            {
                return Err(format!(
                    "remote PNG history delta was invalid: {} matching clips",
                    matching_clips.len()
                ));
            }
            Ok(())
        });

        let _ = run_desktop(config, core, desktop);
        assert!(delivered.load(Ordering::Acquire));
        sender.join().unwrap().unwrap();
    }
}
