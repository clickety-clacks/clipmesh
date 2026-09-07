//! File transport for the native desktop adapters. No clipboard operations or
//! caller-selected URLs enter this boundary; AgentConfig is already validated.
use crate::{websocket_request, AgentConfig, AgentError};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use clipmesh_protocol::files::{FileFailureCode, FileReply, FileRequest, FILE_WEBSOCKET_PROTOCOL};
use clipmesh_protocol::{
    files::{FileDescriptor, FileHistoryEntry, FileManifest, FILE_CHUNK_BYTES, MAX_FILE_BYTES},
    UuidV4,
};
use sha2::{Digest, Sha256};
use std::{
    net::TcpStream,
    time::{Duration, Instant},
};
use tungstenite::{
    client::client,
    http::{header::SEC_WEBSOCKET_PROTOCOL, HeaderValue},
    Message, WebSocket,
};

pub struct FileTransport {
    socket: WebSocket<TcpStream>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Process-lifetime file arrival tracking. Initial history is a baseline, not
/// permission to replace the clipboard with an old file selection.
#[derive(Default)]
pub struct FileArrivals {
    baseline: Option<std::collections::HashSet<UuidV4>>,
    local: std::collections::VecDeque<UuidV4>,
}

impl FileArrivals {
    pub fn note_local_publish(&mut self, id: UuidV4) {
        self.local.push_back(id);
        while self.local.len() > 500 {
            self.local.pop_front();
        }
    }

    pub fn reset_connection(&mut self) {
        self.baseline = None;
    }

    pub fn observe(&mut self, clips: &[FileHistoryEntry]) -> Option<FileHistoryEntry> {
        let retained = clips.iter().map(|clip| clip.clip_id.clone()).collect();
        let previous = self.baseline.replace(retained)?;
        // Coalesce a polling interval to its newest entry. If that is a local
        // upload, do not overwrite it with an older remote arrival.
        let newest = clips.iter().max_by(|a, b| {
            a.accepted_at
                .cmp(&b.accepted_at)
                .then_with(|| a.clip_id.get().cmp(&b.clip_id.get()))
        })?;
        if previous.contains(&newest.clip_id) || self.local.contains(&newest.clip_id) {
            return None;
        }
        Some(newest.clone())
    }
}

/// Owns a private received selection. Keep this alive while the native
/// clipboard references its paths; dropping it removes only this selection.
pub struct ReceivedSelection {
    directory: std::path::PathBuf,
    paths: Vec<std::path::PathBuf>,
}

impl ReceivedSelection {
    pub fn stage(
        root: &std::path::Path,
        files: &[(FileDescriptor, Vec<u8>)],
    ) -> Result<Self, AgentError> {
        use std::{
            fs,
            io::Write,
            os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        };
        let metadata = fs::symlink_metadata(root).map_err(|_| AgentError::AdapterUnavailable)?;
        if !root.is_absolute()
            || !metadata.is_dir()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(AgentError::AdapterUnavailable);
        }
        FileManifest {
            files: files.iter().map(|(file, _)| file.clone()).collect(),
        }
        .validate()
        .map_err(|_| AgentError::ProtocolSchemaInvalid)?;
        // Validate the whole selection before creating any destination files.
        for (file, bytes) in files {
            if file.size_bytes != bytes.len() as u64
                || file.sha256 != format!("{:x}", Sha256::digest(bytes))
            {
                return Err(AgentError::ProtocolSchemaInvalid);
            }
        }
        let directory = root.join(format!("received-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| AgentError::AdapterUnavailable)?;
        let mut selection = Self {
            directory,
            paths: Vec::new(),
        };
        for (file, bytes) in files {
            let path = selection.directory.join(&file.name);
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .map_err(|_| AgentError::AdapterUnavailable)?;
            output
                .write_all(bytes)
                .map_err(|_| AgentError::AdapterUnavailable)?;
            output
                .sync_all()
                .map_err(|_| AgentError::AdapterUnavailable)?;
            selection.paths.push(path);
        }
        Ok(selection)
    }

    pub fn paths(&self) -> &[std::path::PathBuf] {
        &self.paths
    }
}

impl Drop for ReceivedSelection {
    fn drop(&mut self) {
        // This UUID directory was created exclusively by stage, never supplied
        // by a peer or a clipboard path. Partial writes are removed here too.
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Read an explicit native clipboard file selection, never URL text from an
/// ordinary text clipboard. Non-regular files and symlinks are rejected.
pub fn read_selection(
    paths: &[std::path::PathBuf],
) -> Result<Vec<(FileDescriptor, Vec<u8>)>, AgentError> {
    use clipmesh_protocol::files::{MAX_FILES_PER_CLIP, MAX_SELECTION_BYTES};
    use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt};
    if paths.is_empty() || paths.len() > MAX_FILES_PER_CLIP {
        return Err(AgentError::ProtocolSchemaInvalid);
    }
    let mut files = Vec::new();
    let mut total = 0u64;
    for path in paths {
        if !path.is_absolute() {
            return Err(AgentError::ProtocolSchemaInvalid);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| AgentError::TransportUnavailable)?;
        let metadata = file
            .metadata()
            .map_err(|_| AgentError::TransportUnavailable)?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            return Err(AgentError::ProtocolSchemaInvalid);
        }
        total = total
            .checked_add(metadata.len())
            .ok_or(AgentError::ProtocolSchemaInvalid)?;
        if total > MAX_SELECTION_BYTES {
            return Err(AgentError::ProtocolSchemaInvalid);
        }
        let mut bytes = Vec::new();
        file.take(metadata.len() + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| AgentError::TransportUnavailable)?;
        if bytes.len() as u64 != metadata.len() {
            return Err(AgentError::ProtocolSchemaInvalid);
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(AgentError::ProtocolSchemaInvalid)?;
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mime = match extension.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "heic" => "image/heic",
            "webp" => "image/webp",
            "tif" | "tiff" => "image/tiff",
            "mp4" | "m4v" => "video/mp4",
            "mov" => "video/quicktime",
            "webm" => "video/webm",
            "pdf" => "application/pdf",
            "zip" => "application/zip",
            "txt" => "text/plain",
            _ => "application/octet-stream",
        };
        let descriptor = FileDescriptor {
            name: name.into(),
            media_type: mime.into(),
            size_bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        files.push((descriptor, bytes));
    }
    FileManifest {
        files: files.iter().map(|(file, _)| file.clone()).collect(),
    }
    .validate()
    .map_err(|_| AgentError::ProtocolSchemaInvalid)?;
    Ok(files)
}

impl FileTransport {
    pub fn history(&mut self) -> Result<Vec<FileHistoryEntry>, AgentError> {
        match self.exchange(&FileRequest::History {
            request_id: UuidV4::new(),
        })? {
            FileReply::History { clips, .. } if clips.len() <= 500 => {
                for clip in &clips {
                    clip.manifest
                        .validate()
                        .map_err(|_| AgentError::ProtocolSchemaInvalid)?;
                }
                Ok(clips)
            }
            _ => Err(AgentError::ProtocolSchemaInvalid),
        }
    }

    pub fn publish(
        &mut self,
        clip_id: UuidV4,
        files: &[(FileDescriptor, Vec<u8>)],
    ) -> Result<(), AgentError> {
        let manifest = FileManifest {
            files: files.iter().map(|(file, _)| file.clone()).collect(),
        };
        manifest
            .validate()
            .map_err(|_| AgentError::ProtocolSchemaInvalid)?;
        // Validate the entire selection before creating any remote upload.
        for (file, bytes) in files {
            if file.size_bytes != bytes.len() as u64
                || format!("{:x}", Sha256::digest(bytes)) != file.sha256
            {
                return Err(AgentError::ProtocolSchemaInvalid);
            }
        }
        let mut uploads = Vec::new();
        for (file, bytes) in files {
            let upload_id = match self.exchange(&FileRequest::Begin {
                request_id: UuidV4::new(),
                file: file.clone(),
            })? {
                FileReply::Ready {
                    upload_id,
                    offset: 0,
                    ..
                } => upload_id,
                _ => return Err(AgentError::ProtocolSchemaInvalid),
            };
            let mut offset = 0u64;
            for chunk in bytes.chunks(FILE_CHUNK_BYTES) {
                let end = offset + chunk.len() as u64;
                match self.exchange(&FileRequest::Chunk {
                    request_id: UuidV4::new(),
                    upload_id: upload_id.clone(),
                    offset,
                    payload_b64: URL_SAFE_NO_PAD.encode(chunk),
                })? {
                    FileReply::Ready {
                        upload_id: received,
                        offset: accepted,
                        ..
                    } if received == upload_id && accepted == end => {}
                    _ => return Err(AgentError::ProtocolSchemaInvalid),
                }
                offset = end;
            }
            match self.exchange(&FileRequest::Finish {
                request_id: UuidV4::new(),
                upload_id: upload_id.clone(),
            })? {
                FileReply::Complete {
                    upload_id: received,
                    ..
                } if received == upload_id => {}
                _ => return Err(AgentError::ProtocolSchemaInvalid),
            }
            uploads.push(upload_id);
        }
        match self.exchange(&FileRequest::Publish {
            request_id: UuidV4::new(),
            clip_id: clip_id.clone(),
            manifest,
            uploads,
        })? {
            FileReply::Published {
                clip_id: received, ..
            } if received == clip_id => Ok(()),
            _ => Err(AgentError::ProtocolSchemaInvalid),
        }
    }

    pub fn download(
        &mut self,
        clip: &FileHistoryEntry,
        index: usize,
    ) -> Result<Vec<u8>, AgentError> {
        clip.manifest
            .validate()
            .map_err(|_| AgentError::ProtocolSchemaInvalid)?;
        let file = clip
            .manifest
            .files
            .get(index)
            .ok_or(AgentError::ProtocolSchemaInvalid)?;
        if file.size_bytes > MAX_FILE_BYTES {
            return Err(AgentError::ProtocolSchemaInvalid);
        }
        let mut bytes = Vec::new();
        loop {
            let offset = bytes.len() as u64;
            match self.exchange(&FileRequest::Download {
                request_id: UuidV4::new(),
                clip_id: clip.clip_id.clone(),
                file_index: index as u32,
                offset,
            })? {
                FileReply::Data {
                    offset: received,
                    payload_b64,
                    complete,
                    ..
                } if received == offset => {
                    if payload_b64.len() > (FILE_CHUNK_BYTES * 4).div_ceil(3) {
                        return Err(AgentError::ProtocolSchemaInvalid);
                    }
                    let chunk = URL_SAFE_NO_PAD
                        .decode(&payload_b64)
                        .map_err(|_| AgentError::ProtocolSchemaInvalid)?;
                    if chunk.len() > FILE_CHUNK_BYTES
                        || URL_SAFE_NO_PAD.encode(&chunk) != payload_b64
                        || offset + chunk.len() as u64 > file.size_bytes
                        || (chunk.is_empty() && !complete)
                    {
                        return Err(AgentError::ProtocolSchemaInvalid);
                    }
                    bytes.extend(chunk);
                    if complete {
                        break;
                    }
                }
                _ => return Err(AgentError::ProtocolSchemaInvalid),
            }
        }
        if bytes.len() as u64 != file.size_bytes
            || format!("{:x}", Sha256::digest(&bytes)) != file.sha256
        {
            return Err(AgentError::ProtocolSchemaInvalid);
        }
        Ok(bytes)
    }

    /// Download and verify every member before returning clipboard-ready paths.
    /// No partial selection is exposed to a caller on failure or cancellation.
    pub fn receive_selection(
        &mut self,
        clip: &FileHistoryEntry,
        root: &std::path::Path,
    ) -> Result<ReceivedSelection, AgentError> {
        clip.manifest
            .validate()
            .map_err(|_| AgentError::ProtocolSchemaInvalid)?;
        let mut files = Vec::with_capacity(clip.manifest.files.len());
        for (index, descriptor) in clip.manifest.files.iter().enumerate() {
            files.push((descriptor.clone(), self.download(clip, index)?));
        }
        if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AgentError::TransportUnavailable);
        }
        ReceivedSelection::stage(root, &files)
    }

    pub fn connect(config: &AgentConfig) -> Result<Self, AgentError> {
        let stream = TcpStream::connect_timeout(&config.endpoint, Duration::from_secs(5))
            .map_err(|_| AgentError::TransportUnavailable)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|_| AgentError::TransportUnavailable)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .map_err(|_| AgentError::TransportUnavailable)?;
        let mut request = websocket_request(config)?;
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(FILE_WEBSOCKET_PROTOCOL),
        );
        let (mut socket, response) =
            client(request, stream).map_err(|_| AgentError::TransportUnavailable)?;
        if response.headers().get(SEC_WEBSOCKET_PROTOCOL)
            != Some(&HeaderValue::from_static(FILE_WEBSOCKET_PROTOCOL))
            || response.headers().contains_key("sec-websocket-extensions")
        {
            return Err(AgentError::ProtocolSchemaInvalid);
        }
        socket.set_config(|config| {
            config.max_message_size = Some(16 * 1024 * 1024);
            config.max_frame_size = Some(16 * 1024 * 1024);
        });
        Ok(Self {
            socket,
            cancelled: Default::default(),
        })
    }

    pub fn set_cancellation(&mut self, cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.cancelled = cancelled;
    }

    pub fn exchange(&mut self, request: &FileRequest) -> Result<FileReply, AgentError> {
        let encoded = serde_json::to_vec(request).map_err(|_| AgentError::ProtocolSchemaInvalid)?;
        FileRequest::decode(&encoded).map_err(|_| AgentError::ProtocolSchemaInvalid)?;
        let text = String::from_utf8(encoded).map_err(|_| AgentError::ProtocolSchemaInvalid)?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return Err(AgentError::TransportUnavailable);
            }
            if Instant::now() >= deadline {
                return Err(AgentError::TransportUnavailable);
            }
            self.socket
                .send(Message::Text(text.clone().into()))
                .map_err(|_| AgentError::TransportUnavailable)?;
            let reply = loop {
                if self.cancelled.load(std::sync::atomic::Ordering::Acquire)
                    || Instant::now() >= deadline
                {
                    return Err(AgentError::TransportUnavailable);
                }
                match self
                    .socket
                    .read()
                    .map_err(|_| AgentError::TransportUnavailable)?
                {
                    Message::Text(text) => {
                        break serde_json::from_str::<FileReply>(&text)
                            .map_err(|_| AgentError::ProtocolSchemaInvalid)?
                    }
                    Message::Ping(_) => self
                        .socket
                        .flush()
                        .map_err(|_| AgentError::TransportUnavailable)?,
                    Message::Pong(_) => {}
                    _ => return Err(AgentError::ProtocolSchemaInvalid),
                }
            };
            if reply.request_id() != request.request_id() {
                return Err(AgentError::ProtocolSchemaInvalid);
            }
            if matches!(
                reply,
                FileReply::Rejected {
                    code: FileFailureCode::RateLimited,
                    ..
                }
            ) {
                std::thread::sleep(Duration::from_millis(550));
                continue;
            }
            return Ok(reply);
        }
    }
}

impl Drop for FileTransport {
    fn drop(&mut self) {
        let _ = self.socket.close(None);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn file_arrivals_skip_history_and_source_echo_and_coalesce_new_arrivals() {
        use super::*;
        let clip = |accepted_at| FileHistoryEntry {
            clip_id: UuidV4::new(),
            accepted_at,
            manifest: FileManifest { files: vec![] },
        };
        let old = clip(1);
        let remote = clip(2);
        let local = clip(3);
        let mut arrivals = FileArrivals::default();
        assert!(arrivals.observe(&[old.clone()]).is_none());
        assert_eq!(
            arrivals
                .observe(&[remote.clone(), old.clone()])
                .unwrap()
                .clip_id,
            remote.clip_id
        );
        assert!(arrivals.observe(&[old.clone(), remote.clone()]).is_none());
        arrivals.note_local_publish(local.clip_id.clone());
        assert!(arrivals
            .observe(&[local.clone(), remote.clone(), old])
            .is_none());
        arrivals.reset_connection();
        let later = clip(4);
        assert!(arrivals.observe(&[later.clone(), local]).is_none());
        let live = clip(5);
        assert_eq!(
            arrivals.observe(&[later, live.clone()]).unwrap().clip_id,
            live.clip_id
        );
    }
    #[test]
    fn received_selection_is_private_verified_and_owned() {
        use super::*;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let bytes = vec![0, 255, 10, 128];
        let descriptor = FileDescriptor {
            name: "a file.bin".into(),
            media_type: "application/octet-stream".into(),
            size_bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        let selection =
            ReceivedSelection::stage(root.path(), &[(descriptor.clone(), bytes.clone())]).unwrap();
        let path = selection.paths()[0].clone();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        drop(selection);
        assert!(!path.parent().unwrap().exists());
        assert!(
            ReceivedSelection::stage(root.path(), &[(descriptor.clone(), vec![1, 2, 3, 4])])
                .is_err()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ReceivedSelection::stage(root.path(), &[(descriptor, bytes)]).is_err());
    }
    use super::*;
    #[test]
    fn native_selection_reads_contents_and_rejects_special_or_ambiguous_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive.zip");
        std::fs::write(&path, [0, 255, 128, 1]).unwrap();
        let files = read_selection(&[path.clone()]).unwrap();
        assert_eq!(files[0].0.name, "archive.zip");
        assert_eq!(files[0].1, [0, 255, 128, 1]);
        assert_eq!(files[0].0.media_type, "application/zip");
        assert!(read_selection(&[directory.path().to_owned()]).is_err());
        assert!(read_selection(&[path.clone(), path.clone()]).is_err());
        let link = directory.path().join("link.zip");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_selection(&[link]).is_err());
        let large = directory.path().join("large.bin");
        std::fs::File::create(&large)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(read_selection(&[large]).is_err());
    }

    #[test]
    #[ignore = "requires CLIPMESH_TEST_HUB_URL pointing to an isolated hub"]
    fn desktop_binary_file_round_trip() {
        let endpoint = std::env::var("CLIPMESH_TEST_HUB_URL").unwrap();
        let config = AgentConfig::parse_toml(&format!(
            "config_version=1\nhub_url={endpoint:?}\nplatform=\"linux-wayland\"\nstate_path=\"/tmp/clipmesh-file-test/state\"\ncontrol_socket=\"/tmp/clipmesh-file-test/control.sock\""
        )).unwrap();
        let bytes = (0..(1024 * 1024 + 7))
            .map(|i| (i % 256) as u8)
            .collect::<Vec<_>>();
        let file = FileDescriptor {
            name: "desktop-fixture.bin".into(),
            media_type: "application/octet-stream".into(),
            size_bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        let id = UuidV4::new();
        let mut sender = FileTransport::connect(&config).unwrap();
        sender
            .publish(id.clone(), &[(file, bytes.clone())])
            .unwrap();
        let mut receiver = FileTransport::connect(&config).unwrap();
        let clip = receiver
            .history()
            .unwrap()
            .into_iter()
            .find(|clip| clip.clip_id == id)
            .unwrap();
        assert_eq!(receiver.download(&clip, 0).unwrap(), bytes);
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let received = receiver.receive_selection(&clip, root.path()).unwrap();
        assert_eq!(received.paths().len(), 1);
        assert_eq!(std::fs::read(&received.paths()[0]).unwrap(), bytes);
        // Native publication consumes real local files, not a remote URL or
        // the text of a filename. The adapter input preserves verified bytes.
        let native = read_selection(received.paths()).unwrap();
        assert_eq!(native[0].0, clip.manifest.files[0]);
        assert_eq!(native[0].1, bytes);
        #[cfg(target_os = "macos")]
        {
            use clipmesh_agent_core::ClipboardAdapter;
            let mut pasteboard = clipmesh_agent_macos::MacPasteboard::unique_for_capture().unwrap();
            let before = pasteboard
                .write_text(b"synthetic clipboard before download")
                .unwrap();
            let applied = pasteboard
                .write_files_if_current(received.paths(), &before)
                .unwrap()
                .unwrap();
            let (paths, revision) = pasteboard.observe_files().unwrap().unwrap();
            assert_eq!(paths, received.paths());
            assert_eq!(revision, applied);
            assert_eq!(std::fs::read(&paths[0]).unwrap(), bytes);
            assert!(pasteboard.observe_text().unwrap().is_none());
            pasteboard.write_text(b"newer local clipboard").unwrap();
            assert!(pasteboard
                .write_files_if_current(received.paths(), &applied)
                .unwrap()
                .is_none());
            assert_eq!(
                pasteboard.observe_text().unwrap().unwrap().bytes,
                b"newer local clipboard"
            );
        }
        drop(received);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        receiver.set_cancellation(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        )));
        assert!(receiver.receive_selection(&clip, root.path()).is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
