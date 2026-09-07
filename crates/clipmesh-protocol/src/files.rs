//! Portable file metadata. Names are display names, never destination paths.
//! File bytes travel separately from manifests so history can remain lightweight.

use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt};
use thiserror::Error;

pub const MAX_FILES_PER_CLIP: usize = 32;
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_SELECTION_BYTES: u64 = 500 * 1024 * 1024;
pub const FILE_CHUNK_BYTES: usize = 256 * 1024;

/// File transfer uses a separate, negotiated channel. Text-v1 peers must never
/// receive binary content disguised as a text clipboard event.
pub const FILE_WEBSOCKET_PROTOCOL: &str = "clipmesh.files.v1";
pub const MAX_FILE_MESSAGE_BYTES: usize = FILE_CHUNK_BYTES * 4 / 3 + 4096;

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileRequest {
    Begin {
        request_id: crate::UuidV4,
        file: FileDescriptor,
    },
    Resume {
        request_id: crate::UuidV4,
        upload_id: crate::UuidV4,
    },
    Chunk {
        request_id: crate::UuidV4,
        upload_id: crate::UuidV4,
        offset: u64,
        payload_b64: String,
    },
    Finish {
        request_id: crate::UuidV4,
        upload_id: crate::UuidV4,
    },
    Publish {
        request_id: crate::UuidV4,
        clip_id: crate::UuidV4,
        manifest: FileManifest,
        uploads: Vec<crate::UuidV4>,
    },
    History {
        request_id: crate::UuidV4,
    },
    Download {
        request_id: crate::UuidV4,
        clip_id: crate::UuidV4,
        file_index: u32,
        offset: u64,
    },
}

impl fmt::Debug for FileRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FileRequest([redacted])")
    }
}

impl FileRequest {
    pub fn request_id(&self) -> &crate::UuidV4 {
        match self {
            Self::Begin { request_id, .. }
            | Self::Resume { request_id, .. }
            | Self::Chunk { request_id, .. }
            | Self::Finish { request_id, .. }
            | Self::Publish { request_id, .. }
            | Self::History { request_id }
            | Self::Download { request_id, .. } => request_id,
        }
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, FileError> {
        if bytes.len() > MAX_FILE_MESSAGE_BYTES {
            return Err(FileError::LimitExceeded);
        }
        let request: Self =
            serde_json::from_slice(bytes).map_err(|_| FileError::InvalidManifest)?;
        match &request {
            Self::Begin { file, .. } => file.validate()?,
            Self::Publish {
                manifest, uploads, ..
            } => {
                manifest.validate()?;
                if uploads.len() != manifest.files.len() {
                    return Err(FileError::InvalidManifest);
                }
            }
            Self::Chunk {
                offset,
                payload_b64,
                ..
            } => {
                let bytes = decode_chunk(payload_b64)?;
                if offset
                    .checked_add(bytes.len() as u64)
                    .is_none_or(|n| n > MAX_FILE_BYTES)
                {
                    return Err(FileError::LimitExceeded);
                }
            }
            Self::Download {
                file_index, offset, ..
            } => {
                if *file_index as usize >= MAX_FILES_PER_CLIP || *offset > MAX_FILE_BYTES {
                    return Err(FileError::LimitExceeded);
                }
            }
            _ => {}
        }
        Ok(request)
    }
}

pub fn decode_chunk(encoded: &str) -> Result<Vec<u8>, FileError> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    if encoded.len() > (FILE_CHUNK_BYTES * 4).div_ceil(3) {
        return Err(FileError::LimitExceeded);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| FileError::InvalidManifest)?;
    if bytes.is_empty()
        || bytes.len() > FILE_CHUNK_BYTES
        || URL_SAFE_NO_PAD.encode(&bytes) != encoded
    {
        return Err(FileError::InvalidManifest);
    }
    Ok(bytes)
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileReply {
    Published {
        request_id: crate::UuidV4,
        clip_id: crate::UuidV4,
    },
    History {
        request_id: crate::UuidV4,
        clips: Vec<FileHistoryEntry>,
    },
    Ready {
        request_id: crate::UuidV4,
        upload_id: crate::UuidV4,
        offset: u64,
    },
    Complete {
        request_id: crate::UuidV4,
        upload_id: crate::UuidV4,
    },
    Data {
        request_id: crate::UuidV4,
        offset: u64,
        payload_b64: String,
        complete: bool,
    },
    Rejected {
        request_id: crate::UuidV4,
        code: FileFailureCode,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileHistoryEntry {
    pub clip_id: crate::UuidV4,
    pub accepted_at: i64,
    pub manifest: FileManifest,
}

impl fmt::Debug for FileReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FileReply([redacted])")
    }
}

impl FileReply {
    pub fn request_id(&self) -> &crate::UuidV4 {
        match self {
            Self::Published { request_id, .. }
            | Self::History { request_id, .. }
            | Self::Ready { request_id, .. }
            | Self::Complete { request_id, .. }
            | Self::Data { request_id, .. }
            | Self::Rejected { request_id, .. } => request_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFailureCode {
    RateLimited,
    Invalid,
    Unavailable,
    StorageFull,
    HashMismatch,
    StorageFailed,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDescriptor {
    pub name: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub sha256: String,
}

impl fmt::Debug for FileDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FileDescriptor([redacted])")
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileManifest {
    pub files: Vec<FileDescriptor>,
}

impl fmt::Debug for FileManifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FileManifest([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum FileError {
    #[error("file_manifest_invalid")]
    InvalidManifest,
    #[error("file_limit_exceeded")]
    LimitExceeded,
}

impl FileManifest {
    pub fn validate(&self) -> Result<(), FileError> {
        if self.files.is_empty() || self.files.len() > MAX_FILES_PER_CLIP {
            return Err(FileError::LimitExceeded);
        }
        let mut names = HashSet::new();
        let mut total = 0u64;
        for file in &self.files {
            file.validate()?;
            // Portable across case-insensitive filesystems. Export still uses
            // isolated, newly created directories and refuses existing paths.
            if !names.insert(file.name.to_lowercase()) {
                return Err(FileError::InvalidManifest);
            }
            total = total
                .checked_add(file.size_bytes)
                .ok_or(FileError::LimitExceeded)?;
        }
        if total > MAX_SELECTION_BYTES {
            return Err(FileError::LimitExceeded);
        }
        Ok(())
    }
}

impl FileDescriptor {
    pub fn validate(&self) -> Result<(), FileError> {
        if self.name.is_empty()
            || self.name.len() > 255
            || self.name == "."
            || self.name == ".."
            || self.name.ends_with(['.', ' '])
            || self
                .name
                .chars()
                .any(|c| c.is_control() || "/\\:".contains(c))
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(FileError::InvalidManifest);
        }
        let mut parts = self.media_type.split('/');
        let token = |part: &str| {
            !part.is_empty()
                && part.len() <= 127
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&c))
        };
        if !parts.next().is_some_and(token)
            || !parts.next().is_some_and(token)
            || parts.next().is_some()
        {
            return Err(FileError::InvalidManifest);
        }
        if self.size_bytes > MAX_FILE_BYTES {
            return Err(FileError::LimitExceeded);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_decoder_bounds_chunks_and_rejects_ambiguous_wire_values() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let id = "778153d0-7154-4ca4-a1bc-5483d389e6ae";
        let mut request = serde_json::json!({
            "type": "chunk", "request_id": id, "upload_id": id,
            "offset": 0, "payload_b64": URL_SAFE_NO_PAD.encode(vec![255; FILE_CHUNK_BYTES])
        });
        let decode =
            |value: &serde_json::Value| FileRequest::decode(&serde_json::to_vec(value).unwrap());
        assert!(decode(&request).is_ok());
        request["offset"] = serde_json::json!(MAX_FILE_BYTES);
        assert!(matches!(decode(&request), Err(FileError::LimitExceeded)));
        request["offset"] = serde_json::json!(0);
        for malformed in ["", "Zg==", "Zh", "+w", "Zg\n"] {
            request["payload_b64"] = serde_json::json!(malformed);
            assert!(decode(&request).is_err());
        }
        request["payload_b64"] = serde_json::json!("Zg");
        assert!(decode(&request).is_ok());
        request["path"] = serde_json::json!("/tmp/untrusted");
        assert!(decode(&request).is_err());
        assert!(matches!(
            FileRequest::decode(&vec![b' '; MAX_FILE_MESSAGE_BYTES + 1]),
            Err(FileError::LimitExceeded)
        ));
    }

    fn file(name: &str) -> FileDescriptor {
        FileDescriptor {
            name: name.into(),
            media_type: "application/octet-stream".into(),
            size_bytes: 0,
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        }
    }
    #[test]
    fn arbitrary_and_empty_files_are_valid() {
        let manifest = FileManifest {
            files: vec![file("report.pdf"), file("empty.bin")],
        };
        assert_eq!(manifest.validate(), Ok(()));
        assert_eq!(
            serde_json::from_str::<FileManifest>(&serde_json::to_string(&manifest).unwrap())
                .unwrap(),
            manifest
        );
        assert!(!format!("{manifest:?}").contains("report"));
    }
    #[test]
    fn paths_and_ambiguous_names_are_rejected() {
        for name in [
            "", ".", "..", "../file", "/file", "C:\\file", "a/b", "a\0b", "name.", "name ",
        ] {
            assert!(file(name).validate().is_err(), "{name:?}");
        }
        assert!(FileManifest {
            files: vec![file("A.pdf"), file("a.pdf")]
        }
        .validate()
        .is_err());
    }
    #[test]
    fn limits_are_checked_before_transfer() {
        let mut value = file("video.mov");
        value.size_bytes = MAX_FILE_BYTES + 1;
        assert_eq!(value.validate(), Err(FileError::LimitExceeded));
        value.size_bytes = MAX_FILE_BYTES;
        let files = (0..6)
            .map(|i| FileDescriptor {
                name: format!("{i}.mov"),
                ..value.clone()
            })
            .collect();
        assert_eq!(
            FileManifest { files }.validate(),
            Err(FileError::LimitExceeded)
        );
    }
    #[test]
    fn malformed_metadata_is_rejected() {
        for media in [
            "",
            "image",
            "image/png/extra",
            "text/plain; charset=utf-8",
            "text/\nplain",
        ] {
            let mut value = file("a");
            value.media_type = media.into();
            assert!(value.validate().is_err());
        }
        let mut value = file("a");
        value.sha256 = "A".repeat(64);
        assert!(value.validate().is_err());
        assert!(serde_json::from_str::<FileManifest>(r#"{"files":[],"path":"/tmp"}"#).is_err());
    }
}
