//! Linux Wayland clipboard, logind lock-state, and owner-only local-control adapters.
//!
//! This crate opens no TCP listener and starts no service. Raw Wayland MIME
//! metadata remains inside this boundary; callers receive only exact UTF-8
//! bytes, an opaque process-lifetime selection revision, and a checked hint
//! class.

use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
};

use clipmesh_agent_core::{
    AdapterError, AgentCore, ClipboardAdapter, CoreError, HintClassification, LocalControl,
    LocalObservation, PlatformRevision, Status,
};
use serde::Deserialize;
use thiserror::Error;

const MAX_CONTROL_BYTES: usize = 64;
const CHECKED_IN_HINT_REGISTRY: &str = include_str!("../hints/registry-v1.json");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockState {
    Locked,
    Unlocked,
    Unknown,
}

impl LockState {
    pub fn acts_locked(self) -> bool {
        self != Self::Unlocked
    }

    pub fn require_known(self) -> Result<Self, LinuxAdapterError> {
        match self {
            Self::Unknown => Err(LinuxAdapterError::LockStateUnknown),
            known => Ok(known),
        }
    }
}

pub trait LockStateSource {
    fn current_lock_state(&mut self) -> LockState;
}

#[derive(Debug)]
pub struct LinuxSessionLockState {
    inner: platform_lock::SessionLockState,
}

impl LinuxSessionLockState {
    pub fn for_current_process() -> Self {
        Self {
            inner: platform_lock::SessionLockState::for_current_process(),
        }
    }
}

impl Default for LinuxSessionLockState {
    fn default() -> Self {
        Self::for_current_process()
    }
}

impl LockStateSource for LinuxSessionLockState {
    fn current_lock_state(&mut self) -> LockState {
        self.inner.current_lock_state()
    }
}

#[derive(Debug)]
pub struct LockStateMonitor<S> {
    source: S,
    last: LockState,
}

impl<S: LockStateSource> LockStateMonitor<S> {
    pub fn new(mut source: S) -> Self {
        let last = source.current_lock_state();
        Self { source, last }
    }

    pub fn current(&self) -> LockState {
        self.last
    }

    pub fn poll_transition(&mut self) -> Option<LockState> {
        let next = self.source.current_lock_state();
        if next == self.last {
            return None;
        }
        self.last = next;
        Some(next)
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum LinuxAdapterError {
    #[error("adapter_unavailable")]
    AdapterUnavailable,
    #[error("lock_state_unknown")]
    LockStateUnknown,
    #[error("state_path_insecure")]
    StatePathInsecure,
    #[error("local_state_unavailable")]
    LocalStateUnavailable,
    #[error("control_request_invalid")]
    ControlRequestInvalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlCommand {
    Status,
    Pause,
    Resume,
    ClearLocalHistory,
    SharedClear,
    LocalOnlyNext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlOutcome {
    Status(Status),
    SharedClearRequested,
}

impl ControlCommand {
    fn parse(bytes: &[u8]) -> Result<Self, LinuxAdapterError> {
        match bytes.strip_suffix(b"\n").unwrap_or(bytes) {
            b"status" => Ok(Self::Status),
            b"pause" => Ok(Self::Pause),
            b"resume" => Ok(Self::Resume),
            b"clear-local-history" => Ok(Self::ClearLocalHistory),
            b"shared-clear" => Ok(Self::SharedClear),
            b"local-only-next" => Ok(Self::LocalOnlyNext),
            _ => Err(LinuxAdapterError::ControlRequestInvalid),
        }
    }
}

pub fn apply_control(
    agent: &mut AgentCore,
    command: ControlCommand,
) -> Result<ControlOutcome, CoreError> {
    let local = match command {
        ControlCommand::Status => LocalControl::Status,
        ControlCommand::Pause => LocalControl::Pause,
        ControlCommand::Resume => LocalControl::Resume,
        ControlCommand::ClearLocalHistory => LocalControl::ClearLocalHistory,
        ControlCommand::LocalOnlyNext => LocalControl::LocalOnlyNext,
        ControlCommand::SharedClear => return Ok(ControlOutcome::SharedClearRequested),
    };
    agent.local_control(local).map(ControlOutcome::Status)
}

pub struct OwnerControlSocket {
    listener: UnixListener,
    path: PathBuf,
    owner_uid: libc::uid_t,
}

impl OwnerControlSocket {
    pub fn bind(path: &Path) -> Result<Self, LinuxAdapterError> {
        let parent = path.parent().ok_or(LinuxAdapterError::StatePathInsecure)?;
        let parent_metadata =
            fs::symlink_metadata(parent).map_err(|_| LinuxAdapterError::StatePathInsecure)?;
        let owner_uid = unsafe { libc::geteuid() };
        if parent_metadata.file_type().is_symlink()
            || !parent_metadata.is_dir()
            || parent_metadata.uid() != owner_uid
            || parent_metadata.permissions().mode() & 0o077 != 0
        {
            return Err(LinuxAdapterError::StatePathInsecure);
        }
        if fs::symlink_metadata(path).is_ok() {
            return Err(LinuxAdapterError::StatePathInsecure);
        }

        let listener =
            UnixListener::bind(path).map_err(|_| LinuxAdapterError::LocalStateUnavailable)?;
        if fs::set_permissions(path, fs::Permissions::from_mode(0o600)).is_err() {
            let _ = fs::remove_file(path);
            return Err(LinuxAdapterError::LocalStateUnavailable);
        }
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(_) => {
                let _ = fs::remove_file(path);
                return Err(LinuxAdapterError::LocalStateUnavailable);
            }
        };
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_socket()
            || metadata.uid() != owner_uid
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            let _ = fs::remove_file(path);
            return Err(LinuxAdapterError::StatePathInsecure);
        }
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            owner_uid,
        })
    }

    pub fn accept_command(&self) -> Result<(UnixStream, ControlCommand), LinuxAdapterError> {
        let (mut stream, _) = self
            .listener
            .accept()
            .map_err(|_| LinuxAdapterError::LocalStateUnavailable)?;
        if peer_uid(&stream)? != self.owner_uid {
            return Err(LinuxAdapterError::StatePathInsecure);
        }
        let command = read_control_command(&mut stream)?;
        Ok((stream, command))
    }

    pub fn respond(
        mut stream: UnixStream,
        outcome: ControlOutcome,
    ) -> Result<(), LinuxAdapterError> {
        let response = match outcome {
            ControlOutcome::Status(status) => format!(
                "status state={:?} outbox_events={} hinted_suppressions={}\n",
                status.state, status.outbox_events, status.hinted_suppressions
            ),
            ControlOutcome::SharedClearRequested => "shared-clear-requested\n".to_owned(),
        };
        stream
            .write_all(response.as_bytes())
            .map_err(|_| LinuxAdapterError::LocalStateUnavailable)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn read_control_command(reader: &mut impl Read) -> Result<ControlCommand, LinuxAdapterError> {
    // This one-request connection is complete only at write-side EOF; stream writes are not frames.
    let mut bytes = [0_u8; MAX_CONTROL_BYTES + 1];
    let mut count = 0;
    loop {
        let read = reader
            .read(&mut bytes[count..])
            .map_err(|_| LinuxAdapterError::ControlRequestInvalid)?;
        if read == 0 {
            break;
        }
        count += read;
        if count > MAX_CONTROL_BYTES {
            return Err(LinuxAdapterError::ControlRequestInvalid);
        }
    }
    if count == 0 {
        return Err(LinuxAdapterError::ControlRequestInvalid);
    }
    ControlCommand::parse(&bytes[..count])
}

impl Drop for OwnerControlSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn peer_uid(stream: &UnixStream) -> Result<libc::uid_t, LinuxAdapterError> {
    use std::os::fd::AsRawFd;

    #[cfg(target_os = "linux")]
    {
        let mut credentials = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        (result == 0 && length as usize == std::mem::size_of::<libc::ucred>())
            .then_some(credentials.uid)
            .ok_or(LinuxAdapterError::StatePathInsecure)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream.as_raw_fd();
        Err(LinuxAdapterError::StatePathInsecure)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    version: u8,
    entries: Vec<RegistryEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryEntry {
    mime_type: String,
    classification: RegistryClassification,
    evidence: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum RegistryClassification {
    Confidential,
    Transient,
}

fn checked_in_registry() -> Result<Registry, LinuxAdapterError> {
    let registry: Registry = serde_json::from_str(CHECKED_IN_HINT_REGISTRY)
        .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
    if registry.version != 1
        || registry.entries.iter().any(|entry| {
            entry.mime_type.is_empty() || !entry.evidence.starts_with("fixtures/platform/linux/")
        })
    {
        return Err(LinuxAdapterError::AdapterUnavailable);
    }
    Ok(registry)
}

fn classify_mime_types(types: &[String]) -> Result<HintClassification, LinuxAdapterError> {
    let registry = checked_in_registry()?;
    let mut matched = registry
        .entries
        .iter()
        .filter(|entry| types.iter().any(|value| value == &entry.mime_type))
        .map(|entry| entry.classification);
    let Some(first) = matched.next() else {
        return Ok(HintClassification::Ordinary);
    };
    if matched.any(|classification| classification != first) {
        return Ok(HintClassification::Ordinary);
    }
    Ok(match first {
        RegistryClassification::Confidential => HintClassification::Confidential,
        RegistryClassification::Transient => HintClassification::Transient,
    })
}

pub struct WaylandClipboard {
    inner: platform_wayland::Clipboard,
}

impl WaylandClipboard {
    pub fn connect() -> Result<Self, LinuxAdapterError> {
        platform_wayland::Clipboard::connect().map(|inner| Self { inner })
    }

    pub fn observe_text(&self) -> Result<Option<LocalObservation>, LinuxAdapterError> {
        self.inner.observe_text()
    }

    pub fn next_observation(&self) -> Result<LocalObservation, LinuxAdapterError> {
        self.inner.next_observation()
    }

    #[cfg(target_os = "linux")]
    pub fn capture_mime_types() -> Result<Vec<String>, LinuxAdapterError> {
        platform_wayland::capture_mime_types()
    }
}

impl ClipboardAdapter for WaylandClipboard {
    fn is_current(&mut self, revision: &PlatformRevision) -> Result<bool, AdapterError> {
        self.inner.is_current(revision).map_err(|_| AdapterError)
    }

    fn write_text(&mut self, bytes: &[u8]) -> Result<PlatformRevision, AdapterError> {
        self.inner.write_text(bytes).map_err(|_| AdapterError)
    }
}

#[cfg(target_os = "linux")]
mod platform_lock {
    use super::LockState;
    use zbus::{blocking::Proxy, zvariant::OwnedObjectPath};

    #[derive(Debug)]
    pub(super) struct SessionLockState {
        connection: Option<zbus::blocking::Connection>,
        session_path: Option<OwnedObjectPath>,
    }

    impl SessionLockState {
        pub(super) fn for_current_process() -> Self {
            let result = (|| {
                let connection = zbus::blocking::Connection::system().ok()?;
                let manager = Proxy::new(
                    &connection,
                    "org.freedesktop.login1",
                    "/org/freedesktop/login1",
                    "org.freedesktop.login1.Manager",
                )
                .ok()?;
                let session_path = manager
                    .call::<_, _, OwnedObjectPath>("GetSessionByPID", &(std::process::id(),))
                    .ok()?;
                Some((connection, session_path))
            })();
            match result {
                Some((connection, session_path)) => Self {
                    connection: Some(connection),
                    session_path: Some(session_path),
                },
                None => Self {
                    connection: None,
                    session_path: None,
                },
            }
        }

        pub(super) fn current_lock_state(&self) -> LockState {
            let (Some(connection), Some(session_path)) =
                (&self.connection, self.session_path.as_ref())
            else {
                return LockState::Unknown;
            };
            let Ok(session) = Proxy::new(
                connection,
                "org.freedesktop.login1",
                session_path.as_str(),
                "org.freedesktop.login1.Session",
            ) else {
                return LockState::Unknown;
            };
            match session.get_property::<bool>("LockedHint") {
                Ok(true) => LockState::Locked,
                Ok(false) => LockState::Unlocked,
                Err(_) => LockState::Unknown,
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform_lock {
    use super::LockState;

    #[derive(Debug)]
    pub(super) struct SessionLockState;

    impl SessionLockState {
        pub(super) fn for_current_process() -> Self {
            Self
        }

        pub(super) fn current_lock_state(&self) -> LockState {
            LockState::Unknown
        }
    }
}

#[cfg(target_os = "linux")]
mod platform_wayland {
    use super::{classify_mime_types, LinuxAdapterError};
    use clipmesh_agent_core::{LocalObservation, PlatformRevision};
    use os_pipe::pipe;
    use std::{
        collections::HashMap,
        io::Read,
        os::fd::AsFd,
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc::{self, Receiver, SyncSender},
            Arc, Condvar, Mutex,
        },
        thread,
    };
    use wayland_client::{
        event_created_child,
        globals::{registry_queue_init, GlobalListContents},
        protocol::{wl_registry::WlRegistry, wl_seat::WlSeat},
        Connection, Dispatch, Proxy, QueueHandle,
    };
    use wayland_protocols_wlr::data_control::v1::client::{
        zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
        zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
        zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    };
    use wl_clipboard_rs::{
        copy::{MimeSource, MimeType as CopyMimeType, Options as CopyOptions, Source},
        paste::{get_mime_types_ordered, ClipboardType, Seat},
    };

    const HARD_MAX_CAPTURE_BYTES: u64 = 1_048_577;
    static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

    #[derive(Clone)]
    enum SelectionSnapshot {
        Pending,
        Empty,
        Ready(LocalObservation),
        Unavailable,
    }

    struct WatchState {
        instance: u64,
        generation: u64,
        current_revision: Option<PlatformRevision>,
        snapshot: SelectionSnapshot,
        pending_write_marker: Option<String>,
        current_matches_pending_write: bool,
        write_nonce: u64,
        failed: bool,
    }

    impl WatchState {
        fn new(instance: u64) -> Self {
            Self {
                instance,
                generation: 0,
                current_revision: None,
                snapshot: SelectionSnapshot::Empty,
                pending_write_marker: None,
                current_matches_pending_write: false,
                write_nonce: 0,
                failed: false,
            }
        }

        fn begin_selection(
            &mut self,
            has_offer: bool,
            mime_types: &[String],
        ) -> (u64, PlatformRevision) {
            self.generation = self.generation.wrapping_add(1);
            let revision = PlatformRevision::synthetic(format!(
                "wayland-selection:{}:{}",
                self.instance, self.generation
            ));
            self.current_revision = Some(revision.clone());
            self.snapshot = if has_offer {
                SelectionSnapshot::Pending
            } else {
                SelectionSnapshot::Empty
            };
            self.current_matches_pending_write = self
                .pending_write_marker
                .as_ref()
                .is_some_and(|marker| mime_types.iter().any(|value| value == marker));
            (self.generation, revision)
        }

        fn begin_write(&mut self) -> String {
            self.write_nonce = self.write_nonce.wrapping_add(1);
            let marker = format!(
                "application/x-clipmesh-write-marker-{}-{}",
                self.instance, self.write_nonce
            );
            self.pending_write_marker = Some(marker.clone());
            marker
        }
    }

    type SharedWatch = Arc<(Mutex<WatchState>, Condvar)>;

    pub(super) struct Clipboard {
        shared: SharedWatch,
        observations: Receiver<LocalObservation>,
    }

    impl Clipboard {
        pub(super) fn connect() -> Result<Self, LinuxAdapterError> {
            let instance = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
            let shared = Arc::new((Mutex::new(WatchState::new(instance)), Condvar::new()));
            let (observation_tx, observations) = mpsc::sync_channel(16);
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let thread_shared = Arc::clone(&shared);
            thread::spawn(move || event_thread(thread_shared, observation_tx, ready_tx));
            ready_rx
                .recv()
                .map_err(|_| LinuxAdapterError::AdapterUnavailable)??;
            Ok(Self {
                shared,
                observations,
            })
        }

        pub(super) fn observe_text(&self) -> Result<Option<LocalObservation>, LinuxAdapterError> {
            let (mutex, condition) = &*self.shared;
            let mut state = mutex
                .lock()
                .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
            loop {
                if state.failed {
                    return Err(LinuxAdapterError::AdapterUnavailable);
                }
                match &state.snapshot {
                    SelectionSnapshot::Ready(observation) => {
                        return Ok(Some(observation.clone()));
                    }
                    SelectionSnapshot::Empty => return Ok(None),
                    SelectionSnapshot::Unavailable => {
                        return Err(LinuxAdapterError::AdapterUnavailable);
                    }
                    SelectionSnapshot::Pending => {
                        state = condition
                            .wait(state)
                            .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
                    }
                }
            }
        }

        pub(super) fn next_observation(&self) -> Result<LocalObservation, LinuxAdapterError> {
            self.observations
                .recv()
                .map_err(|_| LinuxAdapterError::AdapterUnavailable)
        }

        pub(super) fn is_current(
            &self,
            revision: &PlatformRevision,
        ) -> Result<bool, LinuxAdapterError> {
            let (mutex, _) = &*self.shared;
            let state = mutex
                .lock()
                .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
            if state.failed {
                return Err(LinuxAdapterError::AdapterUnavailable);
            }
            Ok(state.current_revision.as_ref() == Some(revision))
        }

        pub(super) fn write_text(
            &self,
            bytes: &[u8],
        ) -> Result<PlatformRevision, LinuxAdapterError> {
            std::str::from_utf8(bytes).map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
            let (mutex, condition) = &*self.shared;
            let (before, write_marker) = {
                let mut state = mutex
                    .lock()
                    .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
                (state.generation, state.begin_write())
            };

            if CopyOptions::new()
                .copy_multi(vec![
                    MimeSource {
                        source: Source::Bytes(bytes.to_vec().into_boxed_slice()),
                        mime_type: CopyMimeType::Text,
                    },
                    MimeSource {
                        source: Source::Bytes(Vec::new().into_boxed_slice()),
                        mime_type: CopyMimeType::Specific(write_marker),
                    },
                ])
                .is_err()
            {
                if let Ok(mut state) = mutex.lock() {
                    state.pending_write_marker = None;
                }
                return Err(LinuxAdapterError::AdapterUnavailable);
            }

            let mut state = mutex
                .lock()
                .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
            loop {
                if state.failed {
                    return Err(LinuxAdapterError::AdapterUnavailable);
                }
                if state.generation > before {
                    match &state.snapshot {
                        SelectionSnapshot::Pending => {}
                        SelectionSnapshot::Ready(observation)
                            if state.current_matches_pending_write
                                && observation.bytes == bytes =>
                        {
                            let revision = observation.revision.clone();
                            state.pending_write_marker = None;
                            return Ok(revision);
                        }
                        SelectionSnapshot::Ready(_)
                        | SelectionSnapshot::Empty
                        | SelectionSnapshot::Unavailable => {
                            state.pending_write_marker = None;
                            return Err(LinuxAdapterError::AdapterUnavailable);
                        }
                    }
                }
                state = condition
                    .wait(state)
                    .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
            }
        }
    }

    struct EventState {
        shared: SharedWatch,
        observations: SyncSender<LocalObservation>,
        offers: HashMap<ZwlrDataControlOfferV1, Vec<String>>,
        current_offer: Option<ZwlrDataControlOfferV1>,
    }

    impl EventState {
        fn fail(&self) {
            let (mutex, condition) = &*self.shared;
            if let Ok(mut state) = mutex.lock() {
                state.failed = true;
                state.snapshot = SelectionSnapshot::Unavailable;
                condition.notify_all();
            }
        }

        fn selection_changed(
            &mut self,
            offer: Option<ZwlrDataControlOfferV1>,
            connection: &Connection,
        ) {
            if let Some(previous) = self.current_offer.take() {
                self.offers.remove(&previous);
                previous.destroy();
            }

            let mime_types = offer
                .as_ref()
                .and_then(|offer| self.offers.get(offer).cloned())
                .unwrap_or_default();
            let has_offer = offer.is_some();
            let (generation, revision) = {
                let (mutex, condition) = &*self.shared;
                let Ok(mut state) = mutex.lock() else {
                    return self.fail();
                };
                let result = state.begin_selection(has_offer, &mime_types);
                condition.notify_all();
                result
            };

            let Some(offer) = offer else {
                return;
            };
            let Some(mime_type) = preferred_text_mime(&mime_types) else {
                publish_empty(&self.shared, generation);
                self.current_offer = Some(offer);
                return;
            };
            let Ok((reader, writer)) = pipe() else {
                publish_unavailable(&self.shared, generation);
                self.current_offer = Some(offer);
                return;
            };
            offer.receive(mime_type, writer.as_fd());
            drop(writer);
            if connection.flush().is_err() {
                publish_unavailable(&self.shared, generation);
                self.current_offer = Some(offer);
                return;
            }

            let shared = Arc::clone(&self.shared);
            let observations = self.observations.clone();
            thread::spawn(move || {
                let mut bytes = Vec::new();
                let read = reader.take(HARD_MAX_CAPTURE_BYTES).read_to_end(&mut bytes);
                let observation = match read {
                    Ok(_) => classify_mime_types(&mime_types).map(|hint| LocalObservation {
                        bytes,
                        revision,
                        hint,
                    }),
                    Err(_) => Err(LinuxAdapterError::AdapterUnavailable),
                };
                match observation {
                    Ok(observation) => {
                        if publish_ready(&shared, generation, &observation) {
                            let _ = observations.send(observation);
                        }
                    }
                    Err(_) => publish_unavailable(&shared, generation),
                }
            });
            self.current_offer = Some(offer);
        }
    }

    fn event_thread(
        shared: SharedWatch,
        observations: SyncSender<LocalObservation>,
        ready: SyncSender<Result<(), LinuxAdapterError>>,
    ) {
        let result = run_event_loop(Arc::clone(&shared), observations, &ready);
        if result.is_err() {
            let (mutex, condition) = &*shared;
            if let Ok(mut state) = mutex.lock() {
                state.failed = true;
                state.snapshot = SelectionSnapshot::Unavailable;
                condition.notify_all();
            }
        }
    }

    fn run_event_loop(
        shared: SharedWatch,
        observations: SyncSender<LocalObservation>,
        ready: &SyncSender<Result<(), LinuxAdapterError>>,
    ) -> Result<(), LinuxAdapterError> {
        let connection =
            Connection::connect_to_env().map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
        let (globals, mut queue) = registry_queue_init::<EventState>(&connection)
            .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
        let handle = queue.handle();
        let manager: ZwlrDataControlManagerV1 = globals
            .bind(&handle, 1..=1, ())
            .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
        let seat: WlSeat = globals
            .bind(&handle, 1..=9, ())
            .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
        let _device = manager.get_data_device(&seat, &handle, ());
        let mut state = EventState {
            shared,
            observations,
            offers: HashMap::new(),
            current_offer: None,
        };
        queue
            .roundtrip(&mut state)
            .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
        let _ = ready.send(Ok(()));
        loop {
            queue
                .blocking_dispatch(&mut state)
                .map_err(|_| LinuxAdapterError::AdapterUnavailable)?;
        }
    }

    fn preferred_text_mime(types: &[String]) -> Option<String> {
        [
            "text/plain;charset=utf-8",
            "UTF8_STRING",
            "text/plain",
            "STRING",
            "TEXT",
        ]
        .iter()
        .find(|candidate| types.iter().any(|value| value == **candidate))
        .map(|value| (*value).to_owned())
    }

    fn publish_ready(
        shared: &SharedWatch,
        generation: u64,
        observation: &LocalObservation,
    ) -> bool {
        let (mutex, condition) = &**shared;
        let Ok(mut state) = mutex.lock() else {
            return false;
        };
        if state.failed || state.generation != generation {
            return false;
        }
        state.snapshot = SelectionSnapshot::Ready(observation.clone());
        condition.notify_all();
        true
    }

    fn publish_unavailable(shared: &SharedWatch, generation: u64) {
        let (mutex, condition) = &**shared;
        if let Ok(mut state) = mutex.lock() {
            if !state.failed && state.generation == generation {
                state.snapshot = SelectionSnapshot::Unavailable;
                condition.notify_all();
            }
        }
    }

    fn publish_empty(shared: &SharedWatch, generation: u64) {
        let (mutex, condition) = &**shared;
        if let Ok(mut state) = mutex.lock() {
            if !state.failed && state.generation == generation {
                state.snapshot = SelectionSnapshot::Empty;
                condition.notify_all();
            }
        }
    }

    impl Dispatch<WlRegistry, GlobalListContents> for EventState {
        fn event(
            _state: &mut Self,
            _proxy: &WlRegistry,
            _event: <WlRegistry as Proxy>::Event,
            _data: &GlobalListContents,
            _connection: &Connection,
            _handle: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<WlSeat, ()> for EventState {
        fn event(
            _state: &mut Self,
            _proxy: &WlSeat,
            _event: <WlSeat as Proxy>::Event,
            _data: &(),
            _connection: &Connection,
            _handle: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwlrDataControlManagerV1, ()> for EventState {
        fn event(
            _state: &mut Self,
            _proxy: &ZwlrDataControlManagerV1,
            _event: <ZwlrDataControlManagerV1 as Proxy>::Event,
            _data: &(),
            _connection: &Connection,
            _handle: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwlrDataControlDeviceV1, ()> for EventState {
        fn event(
            state: &mut Self,
            _proxy: &ZwlrDataControlDeviceV1,
            event: zwlr_data_control_device_v1::Event,
            _data: &(),
            connection: &Connection,
            _handle: &QueueHandle<Self>,
        ) {
            match event {
                zwlr_data_control_device_v1::Event::DataOffer { id } => {
                    state.offers.insert(id, Vec::new());
                }
                zwlr_data_control_device_v1::Event::Selection { id } => {
                    state.selection_changed(id, connection);
                }
                zwlr_data_control_device_v1::Event::Finished => state.fail(),
                _ => {}
            }
        }

        event_created_child!(EventState, ZwlrDataControlDeviceV1, [
            zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
        ]);
    }

    impl Dispatch<ZwlrDataControlOfferV1, ()> for EventState {
        fn event(
            state: &mut Self,
            proxy: &ZwlrDataControlOfferV1,
            event: zwlr_data_control_offer_v1::Event,
            _data: &(),
            _connection: &Connection,
            _handle: &QueueHandle<Self>,
        ) {
            if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
                if let Some(types) = state.offers.get_mut(proxy) {
                    types.push(mime_type);
                }
            }
        }
    }

    pub(super) fn capture_mime_types() -> Result<Vec<String>, LinuxAdapterError> {
        get_mime_types_ordered(ClipboardType::Regular, Seat::Unspecified)
            .map_err(|_| LinuxAdapterError::AdapterUnavailable)
    }
}

#[cfg(not(target_os = "linux"))]
mod platform_wayland {
    use super::LinuxAdapterError;
    use clipmesh_agent_core::{LocalObservation, PlatformRevision};

    pub(super) struct Clipboard;

    impl Clipboard {
        pub(super) fn connect() -> Result<Self, LinuxAdapterError> {
            Err(LinuxAdapterError::AdapterUnavailable)
        }
        pub(super) fn observe_text(&self) -> Result<Option<LocalObservation>, LinuxAdapterError> {
            Err(LinuxAdapterError::AdapterUnavailable)
        }
        pub(super) fn next_observation(&self) -> Result<LocalObservation, LinuxAdapterError> {
            Err(LinuxAdapterError::AdapterUnavailable)
        }
        pub(super) fn is_current(
            &self,
            _revision: &PlatformRevision,
        ) -> Result<bool, LinuxAdapterError> {
            Err(LinuxAdapterError::AdapterUnavailable)
        }
        pub(super) fn write_text(
            &self,
            _bytes: &[u8],
        ) -> Result<PlatformRevision, LinuxAdapterError> {
            Err(LinuxAdapterError::AdapterUnavailable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, io, net::Shutdown, os::unix::net::UnixStream, thread};
    use tempfile::TempDir;

    struct ChunkedReader(VecDeque<Vec<u8>>);

    impl Read for ChunkedReader {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let Some(chunk) = self.0.pop_front() else {
                return Ok(0);
            };
            assert!(chunk.len() <= output.len());
            output[..chunk.len()].copy_from_slice(&chunk);
            Ok(chunk.len())
        }
    }

    struct SequenceLockState(VecDeque<LockState>);

    impl LockStateSource for SequenceLockState {
        fn current_lock_state(&mut self) -> LockState {
            self.0.pop_front().unwrap_or(LockState::Unknown)
        }
    }

    #[test]
    fn unknown_lock_state_acts_locked_and_transitions_are_observable() {
        let mut monitor = LockStateMonitor::new(SequenceLockState(VecDeque::from([
            LockState::Unknown,
            LockState::Unlocked,
            LockState::Locked,
        ])));
        assert!(monitor.current().acts_locked());
        assert_eq!(
            monitor.current().require_known(),
            Err(LinuxAdapterError::LockStateUnknown)
        );
        assert_eq!(monitor.poll_transition(), Some(LockState::Unlocked));
        assert!(!monitor.current().acts_locked());
        assert_eq!(monitor.poll_transition(), Some(LockState::Locked));
        assert!(monitor.current().acts_locked());
    }

    #[test]
    fn empty_checked_in_registry_keeps_every_mime_type_ordinary() {
        assert_eq!(
            classify_mime_types(&[
                "application/x-clipmesh-unverified".to_owned(),
                "text/plain;charset=utf-8".to_owned(),
            ]),
            Ok(HintClassification::Ordinary)
        );
    }

    #[test]
    fn local_control_command_set_is_closed() {
        for (wire, expected) in [
            (b"status\n".as_slice(), ControlCommand::Status),
            (b"pause\n".as_slice(), ControlCommand::Pause),
            (b"resume\n".as_slice(), ControlCommand::Resume),
            (
                b"clear-local-history\n".as_slice(),
                ControlCommand::ClearLocalHistory,
            ),
            (b"shared-clear\n".as_slice(), ControlCommand::SharedClear),
            (
                b"local-only-next\n".as_slice(),
                ControlCommand::LocalOnlyNext,
            ),
        ] {
            assert_eq!(ControlCommand::parse(wire), Ok(expected));
        }
        assert_eq!(
            ControlCommand::parse(b"clear\n"),
            Err(LinuxAdapterError::ControlRequestInvalid)
        );
        assert_eq!(
            ControlCommand::parse(b"status extra\n"),
            Err(LinuxAdapterError::ControlRequestInvalid)
        );
    }

    #[test]
    fn control_request_accepts_one_complete_command_split_across_reads() {
        let mut reader = ChunkedReader(VecDeque::from([
            b"local-".to_vec(),
            b"only-".to_vec(),
            b"next\n".to_vec(),
        ]));
        assert_eq!(
            read_control_command(&mut reader),
            Ok(ControlCommand::LocalOnlyNext)
        );
    }

    #[test]
    fn control_request_rejects_trailing_bytes_after_a_valid_prefix() {
        let mut reader = ChunkedReader(VecDeque::from([
            b"pause\n".to_vec(),
            b"trailing\n".to_vec(),
        ]));
        assert_eq!(
            read_control_command(&mut reader),
            Err(LinuxAdapterError::ControlRequestInvalid)
        );
    }

    #[test]
    fn control_request_rejects_an_oversized_frame() {
        let mut reader = ChunkedReader(VecDeque::from([vec![b'x'; MAX_CONTROL_BYTES + 1]]));
        assert_eq!(
            read_control_command(&mut reader),
            Err(LinuxAdapterError::ControlRequestInvalid)
        );
    }

    #[test]
    fn owner_control_socket_accepts_only_closed_commands() {
        let directory = TempDir::new().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("control.sock");
        let server = OwnerControlSocket::bind(&path).unwrap();
        let client_path = path.clone();
        let client = thread::spawn(move || {
            let mut stream = UnixStream::connect(client_path).unwrap();
            stream.write_all(b"shared-clear\n").unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        });
        let (stream, command) = server.accept_command().unwrap();
        assert_eq!(command, ControlCommand::SharedClear);
        OwnerControlSocket::respond(stream, ControlOutcome::SharedClearRequested).unwrap();
        assert_eq!(client.join().unwrap(), "shared-clear-requested\n");
        let metadata = fs::metadata(server.path()).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    }

    #[test]
    fn broad_control_parent_is_refused() {
        let directory = TempDir::new().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            OwnerControlSocket::bind(&directory.path().join("control.sock")),
            Err(LinuxAdapterError::StatePathInsecure)
        ));
    }
}
