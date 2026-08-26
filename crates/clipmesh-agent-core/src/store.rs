use std::{
    fs::{self, OpenOptions},
    path::Path,
};

#[cfg(unix)]
use std::os::unix::{fs::MetadataExt, fs::OpenOptionsExt, fs::PermissionsExt};

use clipmesh_protocol::{ClipboardEventV1, U64Decimal, UuidV4};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::{CoreError, LoopMarker, OutboxItem, PersistentSnapshot};

pub(crate) struct StateStore {
    connection: Connection,
}

impl StateStore {
    pub(crate) fn initialize(path: &Path) -> Result<Self, CoreError> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        options
            .open(path)
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        validate_file(path)?;
        let connection = Connection::open(path).map_err(|_| CoreError::LocalStateUnavailable)?;
        connection
            .execute_batch(
                "PRAGMA secure_delete = ON;
                 PRAGMA journal_mode = DELETE;
                 PRAGMA synchronous = FULL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA user_version = 1;
                 CREATE TABLE metadata (
                     key TEXT PRIMARY KEY NOT NULL,
                     value TEXT NOT NULL
                 );
                 CREATE TABLE outbox (
                     message_id TEXT PRIMARY KEY NOT NULL,
                     source_seq TEXT NOT NULL,
                     expires_at_ms INTEGER NOT NULL,
                     payload_bytes INTEGER NOT NULL,
                     event_json TEXT NOT NULL
                 );
                 CREATE TABLE processed_message (
                     insertion_order INTEGER PRIMARY KEY AUTOINCREMENT,
                     message_id TEXT NOT NULL UNIQUE,
                     cursor TEXT NOT NULL
                 );
                 CREATE TABLE loop_marker (
                     singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                     message_id TEXT NOT NULL,
                     content_sha256 TEXT NOT NULL
                 );
                 INSERT INTO metadata(key, value) VALUES
                     ('highest_source_seq', '0');",
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        let store = Self { connection };
        store.snapshot()?;
        Ok(store)
    }

    pub(crate) fn open(path: &Path) -> Result<Self, CoreError> {
        validate_file(path)?;
        let connection = Connection::open(path).map_err(|_| CoreError::LocalStateUnavailable)?;
        connection
            .execute_batch(
                "PRAGMA secure_delete = ON;
                 PRAGMA journal_mode = DELETE;
                 PRAGMA synchronous = FULL;
                 PRAGMA foreign_keys = ON;",
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        if version != 1 {
            return Err(CoreError::LocalStateUnavailable);
        }
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        if integrity != "ok" {
            return Err(CoreError::LocalStateUnavailable);
        }
        let store = Self { connection };
        store.snapshot()?;
        Ok(store)
    }

    pub(crate) fn snapshot(&self) -> Result<PersistentSnapshot, CoreError> {
        let highest_source_seq = self
            .metadata("highest_source_seq")?
            .ok_or(CoreError::LocalStateUnavailable)?
            .parse::<u64>()
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        let history_epoch = self
            .metadata("history_epoch")?
            .map(|value| parse_uuid(&value))
            .transpose()?;
        let last_cursor = self
            .metadata("last_cursor")?
            .map(|value| {
                value
                    .parse::<U64Decimal>()
                    .map_err(|_| CoreError::LocalStateUnavailable)
            })
            .transpose()?;
        let loop_marker = self
            .connection
            .query_row(
                "SELECT message_id, content_sha256 FROM loop_marker WHERE singleton = 1",
                [],
                |row| {
                    Ok(LoopMarker {
                        message_id: parse_uuid_sql(row.get::<_, String>(0)?)?,
                        content_sha256: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(|_| CoreError::LocalStateUnavailable)?;

        Ok(PersistentSnapshot {
            highest_source_seq,
            history_epoch,
            last_cursor,
            outbox: self.outbox_items()?,
            loop_marker,
            processed_message_count: self.processed_count()?,
        })
    }

    pub(crate) fn outbox_items(&self) -> Result<Vec<OutboxItem>, CoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT event_json FROM outbox ORDER BY LENGTH(source_seq), source_seq")
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        let mut items = Vec::new();
        for row in rows {
            let json = row.map_err(|_| CoreError::LocalStateUnavailable)?;
            let event =
                serde_json::from_str(&json).map_err(|_| CoreError::LocalStateUnavailable)?;
            items.push(OutboxItem { event });
        }
        Ok(items)
    }

    pub(crate) fn remove_expired(&mut self, now_ms: i64) -> Result<usize, CoreError> {
        self.connection
            .execute("DELETE FROM outbox WHERE expires_at_ms <= ?1", [now_ms])
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn outbox_usage(&self) -> Result<(usize, usize), CoreError> {
        self.connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(payload_bytes), 0) FROM outbox",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as usize,
                        row.get::<_, i64>(1)? as usize,
                    ))
                },
            )
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn insert_outbox(
        &mut self,
        source_seq: u64,
        event: &ClipboardEventV1,
    ) -> Result<(), CoreError> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        set_metadata(&transaction, "highest_source_seq", &source_seq.to_string())?;
        transaction
            .execute(
                "INSERT INTO outbox(message_id, source_seq, expires_at_ms, payload_bytes, event_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    event.message_id.get().to_string(),
                    source_seq.to_string(),
                    event.expires_at_ms,
                    event.payload_bytes,
                    serde_json::to_string(event).map_err(|_| CoreError::LocalStateUnavailable)?,
                ],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        transaction
            .commit()
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn remove_outbox(&mut self, message_id: &UuidV4) -> Result<bool, CoreError> {
        self.connection
            .execute(
                "DELETE FROM outbox WHERE message_id = ?1",
                [message_id.get().to_string()],
            )
            .map(|count| count != 0)
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn has_processed(&self, message_id: &UuidV4) -> Result<bool, CoreError> {
        self.connection
            .query_row(
                "SELECT 1 FROM processed_message WHERE message_id = ?1",
                [message_id.get().to_string()],
                |_| Ok(()),
            )
            .optional()
            .map(|value| value.is_some())
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn record_received(
        &mut self,
        history_epoch: &UuidV4,
        cursor: U64Decimal,
        message_id: &UuidV4,
        loop_marker: Option<&LoopMarker>,
    ) -> Result<(), CoreError> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        set_metadata(
            &transaction,
            "history_epoch",
            &history_epoch.get().to_string(),
        )?;
        set_metadata(&transaction, "last_cursor", &cursor.get().to_string())?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO processed_message(message_id, cursor)
                 VALUES (?1, ?2)",
                params![message_id.get().to_string(), cursor.get().to_string()],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        transaction
            .execute(
                "DELETE FROM processed_message WHERE insertion_order NOT IN
                 (SELECT insertion_order FROM processed_message ORDER BY insertion_order DESC LIMIT 1024)",
                [],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        if let Some(marker) = loop_marker {
            transaction
                .execute(
                    "INSERT INTO loop_marker(singleton, message_id, content_sha256)
                     VALUES (1, ?1, ?2)
                     ON CONFLICT(singleton) DO UPDATE SET
                     message_id = excluded.message_id, content_sha256 = excluded.content_sha256",
                    params![marker.message_id.get().to_string(), marker.content_sha256],
                )
                .map_err(|_| CoreError::LocalStateUnavailable)?;
        }
        transaction
            .commit()
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn clear_loop_marker(&mut self) -> Result<(), CoreError> {
        self.connection
            .execute("DELETE FROM loop_marker", [])
            .map(|_| ())
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn clear_local_cache(&mut self) -> Result<(), CoreError> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        transaction
            .execute_batch(
                "DELETE FROM outbox;
                 DELETE FROM processed_message;
                 DELETE FROM loop_marker;",
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        transaction
            .commit()
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn apply_purge(
        &mut self,
        history_epoch: &UuidV4,
        cursor: Option<U64Decimal>,
    ) -> Result<(), CoreError> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        transaction
            .execute_batch(
                "DELETE FROM outbox;
                 DELETE FROM processed_message;
                 DELETE FROM loop_marker;",
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        set_metadata(
            &transaction,
            "history_epoch",
            &history_epoch.get().to_string(),
        )?;
        if let Some(cursor) = cursor {
            set_metadata(&transaction, "last_cursor", &cursor.get().to_string())?;
        } else {
            transaction
                .execute("DELETE FROM metadata WHERE key = 'last_cursor'", [])
                .map_err(|_| CoreError::LocalStateUnavailable)?;
        }
        transaction
            .commit()
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    fn metadata(&self, key: &str) -> Result<Option<String>, CoreError> {
        self.connection
            .query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    fn processed_count(&self) -> Result<usize, CoreError> {
        self.connection
            .query_row("SELECT COUNT(*) FROM processed_message", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|count| count as usize)
            .map_err(|_| CoreError::LocalStateUnavailable)
    }
}

fn validate_file(path: &Path) -> Result<(), CoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| CoreError::LocalStateUnavailable)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(CoreError::LocalStateUnavailable);
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(CoreError::LocalStateUnavailable);
    }
    Ok(())
}

fn set_metadata(transaction: &Transaction<'_>, key: &str, value: &str) -> Result<(), CoreError> {
    transaction
        .execute(
            "INSERT INTO metadata(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map(|_| ())
        .map_err(|_| CoreError::LocalStateUnavailable)
}

fn parse_uuid(value: &str) -> Result<UuidV4, CoreError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|_| CoreError::LocalStateUnavailable)
}

fn parse_uuid_sql(value: String) -> rusqlite::Result<UuidV4> {
    parse_uuid(&value).map_err(|_| rusqlite::Error::InvalidQuery)
}
