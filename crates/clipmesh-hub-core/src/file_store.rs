//! Bounded, durable staging for file transfers. The admitted peer owns an
//! upload; no caller-supplied filename is ever used as a filesystem path.
use clipmesh_protocol::files::{FileDescriptor, FileManifest, FILE_CHUNK_BYTES};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::path::Path;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum TransferError {
    #[error("file_transfer_invalid")]
    Invalid,
    #[error("file_transfer_unavailable")]
    Unavailable,
    #[error("file_storage_full")]
    Full,
    #[error("file_hash_mismatch")]
    HashMismatch,
    #[error("file_storage_failed")]
    Storage,
}
impl From<rusqlite::Error> for TransferError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}

pub struct FileStore {
    db: Connection,
    quota: u64,
}

/// Metadata only. Bytes remain in bounded chunk storage.
#[derive(Clone, Debug)]
pub struct PublishedFiles {
    pub clip_id: Uuid,
    pub accepted_at: i64,
    pub manifest: FileManifest,
}

impl FileStore {
    /// Dispatch validated transfer messages for a transport-admitted mesh peer.
    /// Admission and connection lifecycle remain the transport's responsibility.
    pub fn handle(
        &mut self,
        peer: &str,
        request: clipmesh_protocol::files::FileRequest,
        now: i64,
        expires: i64,
    ) -> clipmesh_protocol::files::FileReply {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        use clipmesh_protocol::{files::*, UuidV4};
        let request_id = match &request {
            FileRequest::Begin { request_id, .. }
            | FileRequest::Resume { request_id, .. }
            | FileRequest::Chunk { request_id, .. }
            | FileRequest::Finish { request_id, .. }
            | FileRequest::Publish { request_id, .. }
            | FileRequest::History { request_id }
            | FileRequest::Download { request_id, .. } => request_id.clone(),
        };
        let result = (|| -> Result<FileReply, TransferError> {
            if peer.is_empty() {
                return Err(TransferError::Unavailable);
            }
            Ok(match request {
                FileRequest::Begin { file, .. } => {
                    let upload_id = UuidV4::from_uuid(self.begin(peer, &file, now, expires)?)
                        .map_err(|_| TransferError::Storage)?;
                    FileReply::Ready {
                        request_id: request_id.clone(),
                        upload_id,
                        offset: 0,
                    }
                }
                FileRequest::Resume { upload_id, .. } => {
                    let offset = self.offset(peer, upload_id.get(), now)?;
                    FileReply::Ready {
                        request_id: request_id.clone(),
                        upload_id,
                        offset,
                    }
                }
                FileRequest::Chunk {
                    upload_id,
                    offset,
                    payload_b64,
                    ..
                } => {
                    let bytes = decode_chunk(&payload_b64).map_err(|_| TransferError::Invalid)?;
                    let offset = self.append(peer, upload_id.get(), offset, &bytes, now)?;
                    FileReply::Ready {
                        request_id: request_id.clone(),
                        upload_id,
                        offset,
                    }
                }
                FileRequest::Finish { upload_id, .. } => {
                    self.finish(peer, upload_id.get(), now)?;
                    FileReply::Complete {
                        request_id: request_id.clone(),
                        upload_id,
                    }
                }
                FileRequest::Publish {
                    clip_id,
                    manifest,
                    uploads,
                    ..
                } => {
                    let uploads = uploads.iter().map(UuidV4::get).collect::<Vec<_>>();
                    self.publish(peer, clip_id.get(), &manifest, &uploads, now, expires)?;
                    FileReply::Published {
                        request_id: request_id.clone(),
                        clip_id,
                    }
                }
                FileRequest::History { .. } => {
                    let clips = self
                        .history(now)?
                        .into_iter()
                        .map(|clip| {
                            Ok(FileHistoryEntry {
                                clip_id: UuidV4::from_uuid(clip.clip_id)
                                    .map_err(|_| TransferError::Storage)?,
                                accepted_at: clip.accepted_at,
                                manifest: clip.manifest,
                            })
                        })
                        .collect::<Result<Vec<_>, TransferError>>()?;
                    FileReply::History {
                        request_id: request_id.clone(),
                        clips,
                    }
                }
                FileRequest::Download {
                    clip_id,
                    file_index,
                    offset,
                    ..
                } => {
                    let bytes = self.published_chunk(clip_id.get(), file_index, offset, now)?;
                    // Full-sized terminal chunks require one final empty read.
                    let complete = bytes.len() < FILE_CHUNK_BYTES;
                    FileReply::Data {
                        request_id: request_id.clone(),
                        offset,
                        payload_b64: URL_SAFE_NO_PAD.encode(bytes),
                        complete,
                    }
                }
            })
        })();
        result.unwrap_or_else(|error| FileReply::Rejected {
            request_id,
            code: match error {
                TransferError::Invalid => FileFailureCode::Invalid,
                TransferError::Unavailable => FileFailureCode::Unavailable,
                TransferError::Full => FileFailureCode::StorageFull,
                TransferError::HashMismatch => FileFailureCode::HashMismatch,
                TransferError::Storage => FileFailureCode::StorageFailed,
            },
        })
    }

    /// The caller supplies a private, owner-only database location.
    pub fn open(path: &Path, quota: u64) -> Result<Self, TransferError> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON;
            CREATE TABLE IF NOT EXISTS file_generation(singleton INTEGER PRIMARY KEY CHECK(singleton=1), generation TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS file_uploads(
              id TEXT PRIMARY KEY, peer TEXT NOT NULL, size INTEGER NOT NULL,
              hash TEXT NOT NULL, received INTEGER NOT NULL DEFAULT 0,
              complete INTEGER NOT NULL DEFAULT 0, expires INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS file_chunks(
              upload TEXT NOT NULL REFERENCES file_uploads(id) ON DELETE CASCADE,
              offset INTEGER NOT NULL, bytes BLOB NOT NULL, PRIMARY KEY(upload,offset));
            CREATE TABLE IF NOT EXISTS file_clips(
              id TEXT PRIMARY KEY, peer TEXT NOT NULL, accepted INTEGER NOT NULL,
              expires INTEGER NOT NULL, manifest TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS file_clip_members(
              clip TEXT NOT NULL REFERENCES file_clips(id) ON DELETE CASCADE,
              position INTEGER NOT NULL, upload TEXT NOT NULL REFERENCES file_uploads(id),
              PRIMARY KEY(clip,position));",
        )?;
        Ok(Self { db, quota })
    }

    pub fn begin(
        &mut self,
        peer: &str,
        file: &FileDescriptor,
        now: i64,
        expires: i64,
    ) -> Result<Uuid, TransferError> {
        file.validate().map_err(|_| TransferError::Invalid)?;
        if peer.is_empty() || expires <= now {
            return Err(TransferError::Invalid);
        }
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM file_clips WHERE expires<=?1", [now])?;
        tx.execute("DELETE FROM file_uploads WHERE expires<=?1", [now])?;
        let reserved: u64 =
            tx.query_row("SELECT COALESCE(SUM(size),0) FROM file_uploads", [], |r| {
                r.get(0)
            })?;
        let count: u64 = tx.query_row("SELECT COUNT(*) FROM file_uploads", [], |r| r.get(0))?;
        if reserved
            .checked_add(file.size_bytes)
            .is_none_or(|n| n > self.quota)
            || count >= 4096
        {
            return Err(TransferError::Full);
        }
        let id = Uuid::new_v4();
        tx.execute(
            "INSERT INTO file_uploads(id,peer,size,hash,expires) VALUES(?1,?2,?3,?4,?5)",
            params![id.to_string(), peer, file.size_bytes, file.sha256, expires],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn offset(&self, peer: &str, id: Uuid, now: i64) -> Result<u64, TransferError> {
        self.db
            .query_row(
                "SELECT received FROM file_uploads WHERE id=?1 AND peer=?2 AND expires>?3",
                params![id.to_string(), peer, now],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(TransferError::Unavailable)
    }

    /// Retransmitting an identical chunk is idempotent. Overlaps and reordered
    /// chunks are rejected; the client resumes at the stored offset.
    pub fn append(
        &mut self,
        peer: &str,
        id: Uuid,
        offset: u64,
        bytes: &[u8],
        now: i64,
    ) -> Result<u64, TransferError> {
        if bytes.is_empty() || bytes.len() > FILE_CHUNK_BYTES {
            return Err(TransferError::Invalid);
        }
        let tx = self.db.transaction()?;
        let row: Option<(u64,u64,bool)> = tx.query_row(
            "SELECT size,received,complete FROM file_uploads WHERE id=?1 AND peer=?2 AND expires>?3",
            params![id.to_string(),peer,now], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let (size, received, complete) = row.ok_or(TransferError::Unavailable)?;
        if offset < received {
            let old: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT bytes FROM file_chunks WHERE upload=?1 AND offset=?2",
                    params![id.to_string(), offset],
                    |r| r.get(0),
                )
                .optional()?;
            return if old.as_deref() == Some(bytes) {
                Ok(received)
            } else {
                Err(TransferError::Invalid)
            };
        }
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or(TransferError::Invalid)?;
        if complete || offset != received || end > size {
            return Err(TransferError::Invalid);
        }
        // Bound row overhead as well as payload bytes. A peer must not fill
        // the database with millions of one-byte chunks under the byte quota.
        let chunks: u64 = tx.query_row("SELECT COUNT(*) FROM file_chunks", [], |r| r.get(0))?;
        if chunks
            >= self
                .quota
                .div_ceil(FILE_CHUNK_BYTES as u64)
                .saturating_mul(4)
                .saturating_add(4096)
        {
            return Err(TransferError::Full);
        }
        tx.execute(
            "INSERT INTO file_chunks VALUES(?1,?2,?3)",
            params![id.to_string(), offset, bytes],
        )?;
        tx.execute(
            "UPDATE file_uploads SET received=?2 WHERE id=?1",
            params![id.to_string(), end],
        )?;
        tx.commit()?;
        Ok(end)
    }

    pub fn finish(&mut self, peer: &str, id: Uuid, now: i64) -> Result<(), TransferError> {
        let tx = self.db.transaction()?;
        let row: Option<(u64,u64,String)> = tx.query_row(
            "SELECT size,received,hash FROM file_uploads WHERE id=?1 AND peer=?2 AND expires>?3",
            params![id.to_string(),peer,now], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let (size, received, expected) = row.ok_or(TransferError::Unavailable)?;
        if size != received {
            return Err(TransferError::Invalid);
        }
        let mut digest = Sha256::new();
        {
            let mut stmt =
                tx.prepare("SELECT bytes FROM file_chunks WHERE upload=?1 ORDER BY offset")?;
            let chunks = stmt.query_map([id.to_string()], |r| r.get::<_, Vec<u8>>(0))?;
            for chunk in chunks {
                digest.update(chunk?);
            }
        }
        if format!("{:x}", digest.finalize()) != expected {
            return Err(TransferError::HashMismatch);
        }
        tx.execute(
            "UPDATE file_uploads SET complete=1 WHERE id=?1",
            [id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Only completed files are readable. The transport must first check the
    /// recipient's access to the published clipping that references this ID.
    pub fn chunk(&self, id: Uuid, offset: u64, now: i64) -> Result<Vec<u8>, TransferError> {
        let size: u64 = self
            .db
            .query_row(
                "SELECT size FROM file_uploads WHERE id=?1 AND complete=1 AND expires>?2",
                params![id.to_string(), now],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(TransferError::Unavailable)?;
        if offset > size {
            return Err(TransferError::Invalid);
        }
        // Downloads may resume at any byte, independently of upload boundaries.
        // EOF is a successful empty read, including for zero-byte files.
        let end = size.min(offset.saturating_add(FILE_CHUNK_BYTES as u64));
        let mut result = Vec::with_capacity((end - offset) as usize);
        let mut stmt = self.db.prepare(
            "SELECT offset,bytes FROM file_chunks WHERE upload=?1
             AND offset<?3 AND offset+length(bytes)>?2 ORDER BY offset",
        )?;
        let rows = stmt.query_map(params![id.to_string(), offset, end], |r| {
            Ok((r.get::<_, u64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for row in rows {
            let (start, bytes) = row?;
            let from = offset.saturating_sub(start) as usize;
            let to = bytes.len().min((end - start) as usize);
            result.extend_from_slice(&bytes[from..to]);
        }
        if result.len() as u64 != end - offset {
            return Err(TransferError::Storage);
        }
        Ok(result)
    }

    pub fn sync_generation(&mut self, generation: u64) -> Result<(), TransferError> {
        let tx = self.db.transaction()?;
        let stored: Option<String> = tx
            .query_row(
                "SELECT generation FROM file_generation WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let current = generation.to_string();
        if stored.as_deref() != Some(current.as_str()) {
            tx.execute("DELETE FROM file_clips", [])?;
            tx.execute("DELETE FROM file_uploads", [])?;
            tx.execute("INSERT INTO file_generation VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET generation=excluded.generation", [current])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Runs independently of client traffic. Deleting manifests first releases
    /// their foreign-key references before expired upload chunks are removed.
    pub fn expire(&mut self, now: i64) -> Result<(), TransferError> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM file_clips WHERE expires<=?1", [now])?;
        tx.execute("DELETE FROM file_uploads WHERE expires<=?1", [now])?;
        tx.commit()?;
        self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    pub fn clear(&mut self) -> Result<(), TransferError> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM file_clips", [])?;
        tx.execute("DELETE FROM file_uploads", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Commit a whole selection atomically. The authenticated transport supplies
    /// peer identity. A retry must name the exact same selection and metadata.
    pub fn publish(
        &mut self,
        peer: &str,
        clip_id: Uuid,
        manifest: &FileManifest,
        uploads: &[Uuid],
        now: i64,
        expires: i64,
    ) -> Result<(), TransferError> {
        manifest.validate().map_err(|_| TransferError::Invalid)?;
        if uploads.len() != manifest.files.len() || expires <= now || peer.is_empty() {
            return Err(TransferError::Invalid);
        }
        let encoded = serde_json::to_string(manifest).map_err(|_| TransferError::Invalid)?;
        let tx = self.db.transaction()?;
        let prior: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT peer,manifest,expires FROM file_clips WHERE id=?1",
                [clip_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((owner, metadata, expiry)) = prior {
            if owner != peer || metadata != encoded || expiry <= now {
                return Err(TransferError::Unavailable);
            }
            let mut stmt =
                tx.prepare("SELECT upload FROM file_clip_members WHERE clip=?1 ORDER BY position")?;
            let stored = stmt
                .query_map([clip_id.to_string()], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            return if stored == uploads.iter().map(Uuid::to_string).collect::<Vec<_>>() {
                Ok(())
            } else {
                Err(TransferError::Invalid)
            };
        }
        let count: u64 = tx.query_row("SELECT COUNT(*) FROM file_clips", [], |r| r.get(0))?;
        if count >= 500 {
            return Err(TransferError::Full);
        }
        for (upload, file) in uploads.iter().zip(&manifest.files) {
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM file_uploads WHERE id=?1 AND peer=?2
                 AND complete=1 AND size=?3 AND hash=?4 AND expires>?5)",
                params![upload.to_string(), peer, file.size_bytes, file.sha256, now],
                |r| r.get(0),
            )?;
            if !valid {
                return Err(TransferError::Unavailable);
            }
        }
        tx.execute(
            "INSERT INTO file_clips VALUES(?1,?2,?3,?4,?5)",
            params![clip_id.to_string(), peer, now, expires, encoded],
        )?;
        for (position, upload) in uploads.iter().enumerate() {
            tx.execute(
                "INSERT INTO file_clip_members VALUES(?1,?2,?3)",
                params![clip_id.to_string(), position as u32, upload.to_string()],
            )?;
            tx.execute(
                "UPDATE file_uploads SET expires=MAX(expires,?2) WHERE id=?1",
                params![upload.to_string(), expires],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Called only for an admitted mesh member. Unpublished upload IDs are not
    /// download capabilities: recipients must name a retained clipping.
    pub fn published_chunk(
        &self,
        clip: Uuid,
        index: u32,
        offset: u64,
        now: i64,
    ) -> Result<Vec<u8>, TransferError> {
        let upload: String = self
            .db
            .query_row(
                "SELECT m.upload FROM file_clip_members m JOIN file_clips c ON c.id=m.clip
             WHERE c.id=?1 AND m.position=?2 AND c.expires>?3",
                params![clip.to_string(), index, now],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(TransferError::Unavailable)?;
        self.chunk(
            Uuid::parse_str(&upload).map_err(|_| TransferError::Storage)?,
            offset,
            now,
        )
    }

    pub fn history(&self, now: i64) -> Result<Vec<PublishedFiles>, TransferError> {
        let mut stmt = self.db.prepare(
            "SELECT id,accepted,manifest FROM file_clips WHERE expires>?1 ORDER BY accepted DESC,id DESC LIMIT 500")?;
        let rows = stmt.query_map([now], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (id, accepted_at, encoded) = row?;
            let manifest: FileManifest =
                serde_json::from_str(&encoded).map_err(|_| TransferError::Storage)?;
            manifest.validate().map_err(|_| TransferError::Storage)?;
            Ok(PublishedFiles {
                clip_id: Uuid::parse_str(&id).map_err(|_| TransferError::Storage)?,
                accepted_at,
                manifest,
            })
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_expiry_removes_bytes_and_releases_reserved_quota() {
        let mut store = FileStore::open(Path::new(":memory:"), 3).unwrap();
        let file = descriptor(b"abc");
        let upload = store.begin("p", &file, 0, 10).unwrap();
        store.append("p", upload, 0, b"abc", 1).unwrap();
        store.finish("p", upload, 1).unwrap();
        store
            .publish(
                "p",
                Uuid::new_v4(),
                &FileManifest {
                    files: vec![file.clone()],
                },
                &[upload],
                1,
                10,
            )
            .unwrap();
        store.expire(9).unwrap();
        assert_eq!(store.history(9).unwrap().len(), 1);
        store.expire(10).unwrap();
        let chunks: u64 = store
            .db
            .query_row("SELECT COUNT(*) FROM file_chunks", [], |r| r.get(0))
            .unwrap();
        let uploads: u64 = store
            .db
            .query_row("SELECT COUNT(*) FROM file_uploads", [], |r| r.get(0))
            .unwrap();
        assert_eq!((chunks, uploads), (0, 0));
        assert!(store.history(10).unwrap().is_empty());
        assert!(store.begin("p", &file, 10, 20).is_ok());
    }
    #[test]
    fn clear_generation_reconciles_after_restart_and_revokes_uploads() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("files.sqlite");
        let mut store = FileStore::open(&path, 1024).unwrap();
        store.sync_generation(1).unwrap();
        let file = descriptor(b"abc");
        let upload = store.begin("p", &file, 0, 100).unwrap();
        store.append("p", upload, 0, b"abc", 1).unwrap();
        store.finish("p", upload, 1).unwrap();
        let clip = Uuid::new_v4();
        store
            .publish(
                "p",
                clip,
                &FileManifest {
                    files: vec![file.clone()],
                },
                &[upload],
                1,
                100,
            )
            .unwrap();
        store.sync_generation(1).unwrap();
        assert_eq!(store.history(2).unwrap().len(), 1);
        drop(store);
        // Core committed generation 2 before the process died. Reconcile
        // before exposing either history or downloads after restart.
        let mut store = FileStore::open(&path, 1024).unwrap();
        store.sync_generation(2).unwrap();
        assert!(store.history(2).unwrap().is_empty());
        assert!(store.published_chunk(clip, 0, 0, 2).is_err());
        assert!(store.offset("p", upload, 2).is_err());
        assert!(store
            .publish(
                "p",
                clip,
                &FileManifest { files: vec![file] },
                &[upload],
                2,
                100
            )
            .is_err());
    }
    #[test]
    fn wire_requests_transfer_binary_content_between_admitted_peers() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        use clipmesh_protocol::{files::*, UuidV4};
        let mut store = FileStore::open(Path::new(":memory:"), 1024).unwrap();
        let bytes = [0, 255, 128, 1];
        let file = descriptor(&bytes);
        let request_id = UuidV4::new();
        let call = |store: &mut FileStore, peer: &str, request: FileRequest| {
            let wire = serde_json::to_vec(&request).unwrap();
            let reply = store.handle(peer, FileRequest::decode(&wire).unwrap(), 1, 100);
            serde_json::from_slice::<FileReply>(&serde_json::to_vec(&reply).unwrap()).unwrap()
        };
        let FileReply::Ready { upload_id, .. } = call(
            &mut store,
            "sender",
            FileRequest::Begin {
                request_id: request_id.clone(),
                file: file.clone(),
            },
        ) else {
            panic!("begin rejected");
        };
        assert!(matches!(
            call(
                &mut store,
                "other",
                FileRequest::Resume {
                    request_id: request_id.clone(),
                    upload_id: upload_id.clone()
                }
            ),
            FileReply::Rejected {
                code: FileFailureCode::Unavailable,
                ..
            }
        ));
        assert!(matches!(
            call(
                &mut store,
                "sender",
                FileRequest::Chunk {
                    request_id: request_id.clone(),
                    upload_id: upload_id.clone(),
                    offset: 0,
                    payload_b64: URL_SAFE_NO_PAD.encode(bytes)
                }
            ),
            FileReply::Ready { offset: 4, .. }
        ));
        assert!(matches!(
            call(
                &mut store,
                "sender",
                FileRequest::Finish {
                    request_id: request_id.clone(),
                    upload_id: upload_id.clone()
                }
            ),
            FileReply::Complete { .. }
        ));
        let clip_id = UuidV4::new();
        assert!(matches!(
            call(
                &mut store,
                "sender",
                FileRequest::Publish {
                    request_id: request_id.clone(),
                    clip_id: clip_id.clone(),
                    manifest: FileManifest { files: vec![file] },
                    uploads: vec![upload_id]
                }
            ),
            FileReply::Published { .. }
        ));
        let FileReply::History { clips, .. } = call(
            &mut store,
            "recipient",
            FileRequest::History {
                request_id: request_id.clone(),
            },
        ) else {
            panic!("history rejected");
        };
        assert_eq!(clips.len(), 1);
        let FileReply::Data {
            payload_b64,
            complete,
            ..
        } = call(
            &mut store,
            "recipient",
            FileRequest::Download {
                request_id,
                clip_id,
                file_index: 0,
                offset: 0,
            },
        )
        else {
            panic!("download rejected");
        };
        assert!(complete);
        assert_eq!(URL_SAFE_NO_PAD.decode(payload_b64).unwrap(), bytes);
    }
    #[test]
    fn selection_publish_is_atomic_owned_idempotent_and_retained() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("files.sqlite");
        let mut store = FileStore::open(&path, 1024).unwrap();
        let first = descriptor(b"abc");
        let mut second = descriptor(b"def");
        second.name = "second.zip".into();
        let a = store.begin("sender", &first, 0, 10).unwrap();
        let b = store.begin("sender", &second, 0, 10).unwrap();
        let manifest = FileManifest {
            files: vec![first, second],
        };
        let clip = Uuid::new_v4();
        store.append("sender", a, 0, b"abc", 1).unwrap();
        store.finish("sender", a, 1).unwrap();
        assert!(store
            .publish("sender", clip, &manifest, &[a, b], 2, 100)
            .is_err());
        assert!(store.history(2).unwrap().is_empty());
        assert!(store.published_chunk(clip, 0, 0, 2).is_err());
        store.append("sender", b, 0, b"def", 2).unwrap();
        store.finish("sender", b, 2).unwrap();
        assert!(store
            .publish("other", clip, &manifest, &[a, b], 2, 100)
            .is_err());
        store
            .publish("sender", clip, &manifest, &[a, b], 2, 100)
            .unwrap();
        store
            .publish("sender", clip, &manifest, &[a, b], 3, 200)
            .unwrap();
        assert!(store
            .publish("sender", clip, &manifest, &[b, a], 3, 100)
            .is_err());
        assert_eq!(store.history(3).unwrap().len(), 1);
        drop(store);
        let mut store = FileStore::open(&path, 1024).unwrap();
        assert_eq!(store.history(11).unwrap()[0].manifest, manifest);
        assert_eq!(store.published_chunk(clip, 1, 1, 11).unwrap(), b"ef");
        assert!(store.published_chunk(clip, 2, 0, 11).is_err());
        assert!(store.published_chunk(clip, 0, 0, 100).is_err());
        assert!(store.history(100).unwrap().is_empty());
        store.clear().unwrap();
        assert!(store.published_chunk(clip, 0, 0, 11).is_err());
    }
    fn descriptor(bytes: &[u8]) -> FileDescriptor {
        FileDescriptor {
            name: "test.bin".into(),
            media_type: "application/octet-stream".into(),
            size_bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        }
    }
    #[test]
    fn interrupted_upload_resumes_and_is_verified_before_download() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("files.sqlite");
        let mut store = FileStore::open(&path, 1024).unwrap();
        let id = store
            .begin("sender", &descriptor(b"abcdef"), 0, 100)
            .unwrap();
        assert_eq!(store.append("sender", id, 0, b"abc", 1).unwrap(), 3);
        assert!(store.chunk(id, 0, 1).is_err());
        drop(store);
        let mut store = FileStore::open(&path, 1024).unwrap();
        assert_eq!(store.offset("sender", id, 2).unwrap(), 3);
        assert_eq!(store.append("sender", id, 0, b"abc", 2).unwrap(), 3);
        assert!(store.append("other", id, 3, b"def", 2).is_err());
        assert!(store.append("sender", id, 0, b"BAD", 2).is_err());
        store.append("sender", id, 3, b"def", 2).unwrap();
        store.finish("sender", id, 2).unwrap();
        assert_eq!(store.chunk(id, 3, 2).unwrap(), b"def");
        assert_eq!(store.chunk(id, 1, 2).unwrap(), b"bcdef");
        assert!(store.chunk(id, 6, 2).unwrap().is_empty());
        assert!(matches!(store.chunk(id, 7, 2), Err(TransferError::Invalid)));
        assert!(store.chunk(id, 0, 100).is_err());
        store.clear().unwrap();
        assert!(store.chunk(id, 0, 3).is_err());
    }
    #[test]
    fn quota_and_hash_failures_do_not_expose_file() {
        let mut store = FileStore::open(Path::new(":memory:"), 3).unwrap();
        let id = store.begin("p", &descriptor(b"abc"), 0, 10).unwrap();
        assert!(matches!(
            store.begin("p", &descriptor(b"x"), 0, 10),
            Err(TransferError::Full)
        ));
        store.append("p", id, 0, b"bad", 1).unwrap();
        assert!(matches!(
            store.finish("p", id, 1),
            Err(TransferError::HashMismatch)
        ));
        assert!(store.chunk(id, 0, 1).is_err());
        assert!(store.begin("p", &descriptor(b"x"), 10, 20).is_ok());
    }
    #[test]
    fn zero_byte_file_completes_without_chunks() {
        let mut store = FileStore::open(Path::new(":memory:"), 3).unwrap();
        let id = store.begin("p", &descriptor(b""), 0, 10).unwrap();
        store.finish("p", id, 1).unwrap();
        assert_eq!(store.offset("p", id, 1).unwrap(), 0);
        assert!(store.chunk(id, 0, 1).unwrap().is_empty());
    }
}
