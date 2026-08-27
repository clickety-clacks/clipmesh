//! Native macOS pasteboard, lock-state, and owner-only local-control adapters.
//!
//! This crate opens no network listener and starts no service. Raw pasteboard
//! metadata remains inside this boundary; callers receive only exact UTF-8
//! bytes, a process-lifetime change-count revision, and a checked hint class.

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

    pub fn require_known(self) -> Result<Self, MacAdapterError> {
        match self {
            Self::Unknown => Err(MacAdapterError::LockStateUnknown),
            known => Ok(known),
        }
    }
}

pub trait LockStateSource {
    fn current_lock_state(&mut self) -> LockState;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MacSessionLockState;

impl LockStateSource for MacSessionLockState {
    fn current_lock_state(&mut self) -> LockState {
        platform::current_lock_state()
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
pub enum MacAdapterError {
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
    fn parse(bytes: &[u8]) -> Result<Self, MacAdapterError> {
        match bytes.strip_suffix(b"\n").unwrap_or(bytes) {
            b"status" => Ok(Self::Status),
            b"pause" => Ok(Self::Pause),
            b"resume" => Ok(Self::Resume),
            b"clear-local-history" => Ok(Self::ClearLocalHistory),
            b"shared-clear" => Ok(Self::SharedClear),
            b"local-only-next" => Ok(Self::LocalOnlyNext),
            _ => Err(MacAdapterError::ControlRequestInvalid),
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
    pub fn bind(path: &Path) -> Result<Self, MacAdapterError> {
        let parent = path.parent().ok_or(MacAdapterError::StatePathInsecure)?;
        let parent_metadata =
            fs::symlink_metadata(parent).map_err(|_| MacAdapterError::StatePathInsecure)?;
        let owner_uid = unsafe { libc::geteuid() };
        if parent_metadata.file_type().is_symlink()
            || !parent_metadata.is_dir()
            || parent_metadata.uid() != owner_uid
            || parent_metadata.permissions().mode() & 0o077 != 0
        {
            return Err(MacAdapterError::StatePathInsecure);
        }
        if fs::symlink_metadata(path).is_ok() {
            return Err(MacAdapterError::StatePathInsecure);
        }

        let listener =
            UnixListener::bind(path).map_err(|_| MacAdapterError::LocalStateUnavailable)?;
        if fs::set_permissions(path, fs::Permissions::from_mode(0o600)).is_err() {
            let _ = fs::remove_file(path);
            return Err(MacAdapterError::LocalStateUnavailable);
        }
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(_) => {
                let _ = fs::remove_file(path);
                return Err(MacAdapterError::LocalStateUnavailable);
            }
        };
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_socket()
            || metadata.uid() != owner_uid
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            let _ = fs::remove_file(path);
            return Err(MacAdapterError::StatePathInsecure);
        }
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            owner_uid,
        })
    }

    pub fn accept_command(&self) -> Result<(UnixStream, ControlCommand), MacAdapterError> {
        let (mut stream, _) = self
            .listener
            .accept()
            .map_err(|_| MacAdapterError::LocalStateUnavailable)?;
        if peer_uid(&stream)? != self.owner_uid {
            return Err(MacAdapterError::StatePathInsecure);
        }
        let command = read_control_command(&mut stream)?;
        Ok((stream, command))
    }

    pub fn respond(mut stream: UnixStream, outcome: ControlOutcome) -> Result<(), MacAdapterError> {
        let response = match outcome {
            ControlOutcome::Status(status) => format!(
                "status state={:?} outbox_events={} hinted_suppressions={}\n",
                status.state, status.outbox_events, status.hinted_suppressions
            ),
            ControlOutcome::SharedClearRequested => "shared-clear-requested\n".to_owned(),
        };
        stream
            .write_all(response.as_bytes())
            .map_err(|_| MacAdapterError::LocalStateUnavailable)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn read_control_command(reader: &mut impl Read) -> Result<ControlCommand, MacAdapterError> {
    // This one-request connection is complete only at write-side EOF; stream writes are not frames.
    let mut bytes = [0_u8; MAX_CONTROL_BYTES + 1];
    let mut count = 0;
    loop {
        let read = reader
            .read(&mut bytes[count..])
            .map_err(|_| MacAdapterError::ControlRequestInvalid)?;
        if read == 0 {
            break;
        }
        count += read;
        if count > MAX_CONTROL_BYTES {
            return Err(MacAdapterError::ControlRequestInvalid);
        }
    }
    if count == 0 {
        return Err(MacAdapterError::ControlRequestInvalid);
    }
    ControlCommand::parse(&bytes[..count])
}

impl Drop for OwnerControlSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn peer_uid(stream: &UnixStream) -> Result<libc::uid_t, MacAdapterError> {
    use std::os::fd::AsRawFd;

    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        let mut uid = 0;
        let mut gid = 0;
        let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
        (result == 0)
            .then_some(uid)
            .ok_or(MacAdapterError::StatePathInsecure)
    }

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
            .ok_or(MacAdapterError::StatePathInsecure)
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        target_os = "linux"
    )))]
    {
        let _ = stream.as_raw_fd();
        Err(MacAdapterError::StatePathInsecure)
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
    declared_type: String,
    classification: RegistryClassification,
    evidence: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum RegistryClassification {
    Confidential,
    Transient,
}

fn checked_in_registry() -> Result<Registry, MacAdapterError> {
    let registry: Registry = serde_json::from_str(CHECKED_IN_HINT_REGISTRY)
        .map_err(|_| MacAdapterError::AdapterUnavailable)?;
    if registry.version != 1
        || registry.entries.iter().any(|entry| {
            entry.declared_type.is_empty()
                || !entry.evidence.starts_with("fixtures/platform/macos/")
        })
    {
        return Err(MacAdapterError::AdapterUnavailable);
    }
    Ok(registry)
}

fn classify_declared_types(types: &[String]) -> Result<HintClassification, MacAdapterError> {
    let registry = checked_in_registry()?;
    let mut matched = registry
        .entries
        .iter()
        .filter(|entry| types.iter().any(|value| value == &entry.declared_type))
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

pub struct MacPasteboard {
    inner: platform::Pasteboard,
}

impl MacPasteboard {
    pub fn general() -> Result<Self, MacAdapterError> {
        platform::Pasteboard::general().map(|inner| Self { inner })
    }

    pub fn observe_text(&self) -> Result<Option<LocalObservation>, MacAdapterError> {
        let snapshot = self.inner.read_snapshot()?;
        let Some(bytes) = snapshot.bytes else {
            return Ok(None);
        };
        let hint = classify_declared_types(&snapshot.declared_types)?;
        Ok(Some(LocalObservation {
            bytes,
            revision: snapshot.revision,
            hint,
        }))
    }

    #[cfg(target_os = "macos")]
    pub fn unique_for_capture() -> Result<Self, MacAdapterError> {
        platform::Pasteboard::unique().map(|inner| Self { inner })
    }

    #[cfg(target_os = "macos")]
    pub fn capture_seed_text(
        &self,
        text: &str,
        declared_type: &str,
    ) -> Result<(), MacAdapterError> {
        self.inner.seed_text(text, declared_type)
    }

    #[cfg(target_os = "macos")]
    pub fn current_change_count(&self) -> Result<i64, MacAdapterError> {
        self.inner.change_count()
    }

    /// Returns declared types only to the isolated capture tool. Runtime
    /// observations keep this metadata inside the adapter boundary.
    #[cfg(target_os = "macos")]
    pub fn capture_declared_types(&self) -> Result<Vec<String>, MacAdapterError> {
        self.inner.declared_types()
    }
}

impl ClipboardAdapter for MacPasteboard {
    fn is_current(&mut self, revision: &PlatformRevision) -> Result<bool, AdapterError> {
        self.inner.is_current(revision).map_err(|_| AdapterError)
    }

    fn write_text(&mut self, bytes: &[u8]) -> Result<PlatformRevision, AdapterError> {
        self.inner.write_text(bytes).map_err(|_| AdapterError)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{LockState, MacAdapterError};
    use clipmesh_agent_core::PlatformRevision;
    use core_foundation::{
        base::{CFType, TCFType},
        boolean::CFBoolean,
        dictionary::CFDictionary,
        string::CFString,
    };
    use core_foundation_sys::dictionary::CFDictionaryRef;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSPasteboard, NSPasteboardType, NSPasteboardTypeString};
    use objc2_foundation::{NSArray, NSString};

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
    }

    pub(super) struct Snapshot {
        pub(super) bytes: Option<Vec<u8>>,
        pub(super) revision: PlatformRevision,
        pub(super) declared_types: Vec<String>,
    }

    pub(super) struct Pasteboard {
        pasteboard: Retained<NSPasteboard>,
        release_when_dropped: bool,
    }

    impl Pasteboard {
        pub(super) fn general() -> Result<Self, MacAdapterError> {
            Ok(Self {
                pasteboard: NSPasteboard::generalPasteboard(),
                release_when_dropped: false,
            })
        }

        pub(super) fn unique() -> Result<Self, MacAdapterError> {
            Ok(Self {
                pasteboard: NSPasteboard::pasteboardWithUniqueName(),
                release_when_dropped: true,
            })
        }

        pub(super) fn read_snapshot(&self) -> Result<Snapshot, MacAdapterError> {
            let before = self.pasteboard.changeCount();
            let declared_types = self
                .pasteboard
                .types()
                .map(|types| {
                    types
                        .to_vec()
                        .into_iter()
                        .map(|value| value.to_string())
                        .collect()
                })
                .unwrap_or_default();
            let bytes = self
                .pasteboard
                .stringForType(unsafe { NSPasteboardTypeString })
                .map(|value| value.to_string().into_bytes());
            let after = self.pasteboard.changeCount();
            if before != after {
                return Err(MacAdapterError::AdapterUnavailable);
            }
            Ok(Snapshot {
                bytes,
                revision: revision(after)?,
                declared_types,
            })
        }

        pub(super) fn is_current(
            &self,
            expected: &PlatformRevision,
        ) -> Result<bool, MacAdapterError> {
            Ok(revision(self.pasteboard.changeCount())? == *expected)
        }

        pub(super) fn write_text(&self, bytes: &[u8]) -> Result<PlatformRevision, MacAdapterError> {
            let text =
                std::str::from_utf8(bytes).map_err(|_| MacAdapterError::AdapterUnavailable)?;
            self.pasteboard.clearContents();
            if !self
                .pasteboard
                .setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString })
            {
                return Err(MacAdapterError::AdapterUnavailable);
            }
            revision(self.pasteboard.changeCount())
        }

        pub(super) fn seed_text(
            &self,
            text: &str,
            declared_type: &str,
        ) -> Result<(), MacAdapterError> {
            self.pasteboard.clearContents();
            let custom = NSString::from_str(declared_type);
            let types: Retained<NSArray<NSPasteboardType>> =
                NSArray::from_slice(&[unsafe { NSPasteboardTypeString }, &custom]);
            unsafe { self.pasteboard.declareTypes_owner(&types, None) };
            if !self
                .pasteboard
                .setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString })
            {
                return Err(MacAdapterError::AdapterUnavailable);
            }
            Ok(())
        }

        pub(super) fn change_count(&self) -> Result<i64, MacAdapterError> {
            i64::try_from(self.pasteboard.changeCount())
                .map_err(|_| MacAdapterError::AdapterUnavailable)
        }

        pub(super) fn declared_types(&self) -> Result<Vec<String>, MacAdapterError> {
            Ok(self
                .pasteboard
                .types()
                .map(|types| {
                    types
                        .to_vec()
                        .into_iter()
                        .map(|value| value.to_string())
                        .collect()
                })
                .unwrap_or_default())
        }
    }

    impl Drop for Pasteboard {
        fn drop(&mut self) {
            if self.release_when_dropped {
                self.pasteboard.clearContents();
            }
        }
    }

    fn revision(change_count: isize) -> Result<PlatformRevision, MacAdapterError> {
        let count = i64::try_from(change_count).map_err(|_| MacAdapterError::AdapterUnavailable)?;
        Ok(PlatformRevision::synthetic(format!(
            "macos-change-count:{count}"
        )))
    }

    pub(super) fn current_lock_state() -> LockState {
        let raw = unsafe { CGSessionCopyCurrentDictionary() };
        if raw.is_null() {
            return LockState::Unknown;
        }
        let dictionary: CFDictionary<CFString, CFType> =
            unsafe { TCFType::wrap_under_create_rule(raw) };
        let key = CFString::new("CGSSessionScreenIsLocked");
        let Some(value) = dictionary.find(&key) else {
            return LockState::Unknown;
        };
        let Some(value) = value.downcast::<CFBoolean>() else {
            return LockState::Unknown;
        };
        if bool::from(value) {
            LockState::Locked
        } else {
            LockState::Unlocked
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{LockState, MacAdapterError};
    use clipmesh_agent_core::PlatformRevision;

    pub(super) struct Snapshot {
        pub(super) bytes: Option<Vec<u8>>,
        pub(super) revision: PlatformRevision,
        pub(super) declared_types: Vec<String>,
    }

    pub(super) struct Pasteboard;

    impl Pasteboard {
        pub(super) fn general() -> Result<Self, MacAdapterError> {
            Err(MacAdapterError::AdapterUnavailable)
        }
        pub(super) fn read_snapshot(&self) -> Result<Snapshot, MacAdapterError> {
            Err(MacAdapterError::AdapterUnavailable)
        }
        pub(super) fn is_current(
            &self,
            _revision: &PlatformRevision,
        ) -> Result<bool, MacAdapterError> {
            Err(MacAdapterError::AdapterUnavailable)
        }
        pub(super) fn write_text(
            &self,
            _bytes: &[u8],
        ) -> Result<PlatformRevision, MacAdapterError> {
            Err(MacAdapterError::AdapterUnavailable)
        }
    }

    pub(super) fn current_lock_state() -> LockState {
        LockState::Unknown
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
            Err(MacAdapterError::LockStateUnknown)
        );
        assert_eq!(monitor.poll_transition(), Some(LockState::Unlocked));
        assert!(!monitor.current().acts_locked());
        assert_eq!(monitor.poll_transition(), Some(LockState::Locked));
        assert!(monitor.current().acts_locked());
    }

    #[test]
    fn empty_checked_in_registry_keeps_every_declared_type_ordinary() {
        assert_eq!(
            classify_declared_types(&[
                "com.example.unverified".to_owned(),
                "com.example.source-name-only".to_owned(),
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
            Err(MacAdapterError::ControlRequestInvalid)
        );
        assert_eq!(
            ControlCommand::parse(b"status extra\n"),
            Err(MacAdapterError::ControlRequestInvalid)
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
            Err(MacAdapterError::ControlRequestInvalid)
        );
    }

    #[test]
    fn control_request_rejects_an_oversized_frame() {
        let mut reader = ChunkedReader(VecDeque::from([vec![b'x'; MAX_CONTROL_BYTES + 1]]));
        assert_eq!(
            read_control_command(&mut reader),
            Err(MacAdapterError::ControlRequestInvalid)
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
            Err(MacAdapterError::StatePathInsecure)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_unique_pasteboard_preserves_utf8_and_change_count_revisions() {
        let mut pasteboard = MacPasteboard::unique_for_capture().unwrap();
        pasteboard
            .capture_seed_text("local λ\nline", "com.example.unverified")
            .unwrap();
        let local = pasteboard.observe_text().unwrap().unwrap();
        assert_eq!(local.bytes, "local λ\nline".as_bytes());
        assert_eq!(local.hint, HintClassification::Ordinary);
        assert!(pasteboard.is_current(&local.revision).unwrap());
        assert!(pasteboard.is_current(&local.revision).unwrap());

        let remote = pasteboard.write_text("remote 🧷\nline".as_bytes()).unwrap();
        assert!(!pasteboard.is_current(&local.revision).unwrap());
        assert!(pasteboard.is_current(&remote).unwrap());
        let observed_remote = pasteboard.observe_text().unwrap().unwrap();
        assert_eq!(observed_remote.bytes, "remote 🧷\nline".as_bytes());
        assert_eq!(observed_remote.revision, remote);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn invalid_utf8_remote_write_fails_before_native_pasteboard_mutation() {
        let mut pasteboard = MacPasteboard::unique_for_capture().unwrap();
        pasteboard
            .capture_seed_text("preserved", "com.example.unverified")
            .unwrap();
        let before = pasteboard.observe_text().unwrap().unwrap();
        assert_eq!(pasteboard.write_text(&[0xff]), Err(AdapterError));
        assert!(pasteboard.is_current(&before.revision).unwrap());
        assert_eq!(
            pasteboard.observe_text().unwrap().unwrap().bytes,
            b"preserved"
        );
    }
}
