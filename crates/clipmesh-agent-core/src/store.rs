use std::{
    fs::{self, OpenOptions},
    path::Path,
};

#[cfg(unix)]
use std::os::unix::{fs::MetadataExt, fs::OpenOptionsExt, fs::PermissionsExt};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use uuid::Uuid;

use crate::{
    ClipContentV1, CoreError, LoopMarker, OutboxItem, PersistentSnapshot, PlatformRevision,
    PublishEventV1, ReceivedEvent,
};

pub(crate) struct StateStore {
    connection: Connection,
}

impl StateStore {
    pub(crate) fn open_or_initialize(path: &Path) -> Result<Self, CoreError> {
        validate_parent(path)?;
        if path.exists() || fs::symlink_metadata(path).is_ok() {
            Self::open(path)
        } else {
            Self::initialize(path)
        }
    }

    fn initialize(path: &Path) -> Result<Self, CoreError> {
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        options
            .open(path)
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        validate_database_files(path)?;
        let connection = Connection::open(path).map_err(|_| CoreError::LocalStateUnavailable)?;
        connection
            .execute_batch(
                "PRAGMA secure_delete = ON;
                 PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = FULL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA user_version = 1;
                 CREATE TABLE metadata (
                     key TEXT PRIMARY KEY NOT NULL,
                     value TEXT NOT NULL
                 );
                 CREATE TABLE outbox (
                     insertion_order INTEGER PRIMARY KEY AUTOINCREMENT,
                     message_id TEXT NOT NULL UNIQUE,
                     clear_generation TEXT NOT NULL,
                     created_at_ms INTEGER NOT NULL,
                     content BLOB NOT NULL
                 );
                 CREATE TABLE history (
                     cursor TEXT PRIMARY KEY NOT NULL,
                     message_id TEXT NOT NULL UNIQUE,
                     clear_generation TEXT NOT NULL,
                     source_peer_id TEXT NOT NULL,
                     created_at_ms INTEGER NOT NULL,
                     accepted_at_ms INTEGER NOT NULL,
                     expires_at_ms INTEGER NOT NULL,
                     content BLOB NOT NULL
                 );
                 CREATE TABLE processed_message (
                     cursor TEXT PRIMARY KEY NOT NULL,
                     message_id TEXT NOT NULL UNIQUE
                 );
                 CREATE TABLE loop_marker (
                     singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                     message_id TEXT NOT NULL,
                     revision TEXT NOT NULL,
                     content BLOB NOT NULL
                 );
                 CREATE TABLE publish_failure (
                     insertion_order INTEGER PRIMARY KEY AUTOINCREMENT,
                     message_id TEXT NOT NULL,
                     code TEXT NOT NULL
                 );",
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        secure_sidecars(path)?;
        let store = Self { connection };
        store.snapshot()?;
        Ok(store)
    }

    fn open(path: &Path) -> Result<Self, CoreError> {
        validate_database_files(path)?;
        let readonly = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| CoreError::LocalStateUnavailable)?;
        let version: u32 = readonly
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        if version != 1 {
            return Err(CoreError::LocalStateUnavailable);
        }
        let integrity: String = readonly
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        if integrity != "ok" {
            return Err(CoreError::LocalStateUnavailable);
        }
        drop(readonly);

        let connection = Connection::open(path).map_err(|_| CoreError::LocalStateUnavailable)?;
        connection
            .execute_batch(
                "PRAGMA secure_delete = ON;
                 PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = FULL;
                 PRAGMA foreign_keys = ON;",
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        secure_sidecars(path)?;
        let store = Self { connection };
        store.snapshot()?;
        Ok(store)
    }

    pub(crate) fn snapshot(&self) -> Result<PersistentSnapshot, CoreError> {
        let history_epoch = self
            .metadata("history_epoch")?
            .map(|value| Uuid::parse_str(&value).map_err(|_| CoreError::LocalStateUnavailable))
            .transpose()?;
        let clear_generation = self
            .metadata("clear_generation")?
            .map(|value| parse_counter(&value))
            .transpose()?;
        let last_cursor = self
            .metadata("last_cursor")?
            .map(|value| parse_counter(&value))
            .transpose()?;
        let loop_marker = self
            .connection
            .query_row(
                "SELECT message_id, revision, content FROM loop_marker WHERE singleton = 1",
                [],
                |row| {
                    let message_id = parse_uuid_sql(row.get::<_, String>(0)?)?;
                    let revision = PlatformRevision::from_storage(row.get(1)?);
                    let bytes = row.get::<_, Vec<u8>>(2)?;
                    let content = ClipContentV1::from_storage_blob(&bytes)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?;
                    Ok(LoopMarker {
                        message_id,
                        content,
                        revision,
                    })
                },
            )
            .optional()
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        Ok(PersistentSnapshot {
            history_epoch,
            clear_generation,
            last_cursor,
            outbox: self.outbox_items()?,
            loop_marker,
            processed_message_count: self.table_count("processed_message")?,
            history_count: self.table_count("history")?,
        })
    }

    pub(crate) fn outbox_items(&self) -> Result<Vec<OutboxItem>, CoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT message_id, clear_generation, created_at_ms, content
                 FROM outbox ORDER BY insertion_order",
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        let rows = statement
            .query_map([], |row| {
                let bytes = row.get::<_, Vec<u8>>(3)?;
                Ok(OutboxItem {
                    event: PublishEventV1 {
                        message_id: parse_uuid_sql(row.get(0)?)?,
                        clear_generation: parse_counter_sql(row.get(1)?)?,
                        created_at_ms: row.get(2)?,
                        content: ClipContentV1::from_storage_blob(&bytes)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    },
                })
            })
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn establish_context(
        &mut self,
        history_epoch: &Uuid,
        clear_generation: u64,
    ) -> Result<(), CoreError> {
        let transaction = self.transaction()?;
        set_metadata(&transaction, "history_epoch", &history_epoch.to_string())?;
        set_metadata(
            &transaction,
            "clear_generation",
            &clear_generation.to_string(),
        )?;
        commit(transaction)
    }

    pub(crate) fn apply_epoch_change(
        &mut self,
        history_epoch: &Uuid,
        clear_generation: u64,
    ) -> Result<(), CoreError> {
        let transaction = self.transaction()?;
        transaction
            .execute_batch(
                "DELETE FROM history;
                 DELETE FROM processed_message;",
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        set_metadata(&transaction, "history_epoch", &history_epoch.to_string())?;
        set_metadata(
            &transaction,
            "clear_generation",
            &clear_generation.to_string(),
        )?;
        transaction
            .execute("DELETE FROM metadata WHERE key = 'last_cursor'", [])
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        commit(transaction)
    }

    pub(crate) fn remove_stale_outbox(
        &mut self,
        now_ms: i64,
        retention_seconds: u64,
        clear_generation: u64,
    ) -> Result<usize, CoreError> {
        let retention_ms = retention_seconds.saturating_mul(1_000).min(i64::MAX as u64) as i64;
        self.connection
            .execute(
                "DELETE FROM outbox
                 WHERE clear_generation != ?1 OR created_at_ms < ?2",
                params![
                    clear_generation.to_string(),
                    now_ms.saturating_sub(retention_ms)
                ],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn outbox_usage(&self) -> Result<(usize, usize), CoreError> {
        self.connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(LENGTH(content)), 0) FROM outbox",
                [],
                |row| {
                    Ok((
                        usize::try_from(row.get::<_, i64>(0)?).unwrap_or(usize::MAX),
                        usize::try_from(row.get::<_, i64>(1)?).unwrap_or(usize::MAX),
                    ))
                },
            )
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn insert_outbox(&mut self, event: &PublishEventV1) -> Result<(), CoreError> {
        let transaction = self.transaction()?;
        transaction
            .execute(
                "INSERT INTO outbox(message_id, clear_generation, created_at_ms, content)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    event.message_id.to_string(),
                    event.clear_generation.to_string(),
                    event.created_at_ms,
                    event.content.as_storage_blob(),
                ],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        commit(transaction)
    }

    pub(crate) fn remove_outbox(&mut self, message_id: Uuid) -> Result<bool, CoreError> {
        self.connection
            .execute(
                "DELETE FROM outbox WHERE message_id = ?1",
                [message_id.to_string()],
            )
            .map(|count| count != 0)
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn record_publish_failure(
        &mut self,
        message_id: Uuid,
        code: &str,
    ) -> Result<(), CoreError> {
        let transaction = self.transaction()?;
        transaction
            .execute(
                "INSERT INTO publish_failure(message_id, code) VALUES (?1, ?2)",
                params![message_id.to_string(), code],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        transaction
            .execute(
                "DELETE FROM outbox WHERE message_id = ?1",
                [message_id.to_string()],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        commit(transaction)
    }

    pub(crate) fn has_processed(&self, message_id: Uuid) -> Result<bool, CoreError> {
        self.connection
            .query_row(
                "SELECT 1 FROM processed_message WHERE message_id = ?1",
                [message_id.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map(|value| value.is_some())
            .map_err(|_| CoreError::LocalStateUnavailable)
    }

    pub(crate) fn record_received(
        &mut self,
        received: &ReceivedEvent,
        content: &ClipContentV1,
    ) -> Result<(), CoreError> {
        let transaction = self.transaction()?;
        set_metadata(
            &transaction,
            "history_epoch",
            &received.history_epoch.to_string(),
        )?;
        set_metadata(
            &transaction,
            "clear_generation",
            &received.clear_generation.to_string(),
        )?;
        set_metadata(&transaction, "last_cursor", &received.cursor.to_string())?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO history(
                     cursor, message_id, clear_generation, source_peer_id,
                     created_at_ms, accepted_at_ms, expires_at_ms, content
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    received.cursor.to_string(),
                    received.message_id.to_string(),
                    received.clear_generation.to_string(),
                    received.source_peer_id,
                    received.created_at_ms,
                    received.accepted_at_ms,
                    received.expires_at_ms,
                    content.as_storage_blob(),
                ],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO processed_message(cursor, message_id) VALUES (?1, ?2)",
                params![received.cursor.to_string(), received.message_id.to_string()],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        transaction
            .execute(
                "DELETE FROM processed_message WHERE cursor NOT IN
                 (SELECT cursor FROM processed_message ORDER BY LENGTH(cursor) DESC, cursor DESC LIMIT 1024)",
                [],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        commit(transaction)
    }

    pub(crate) fn replace_loop_marker(&mut self, marker: &LoopMarker) -> Result<(), CoreError> {
        let transaction = self.transaction()?;
        transaction
            .execute(
                "INSERT INTO loop_marker(singleton, message_id, revision, content)
                 VALUES (1, ?1, ?2, ?3)
                 ON CONFLICT(singleton) DO UPDATE SET
                 message_id = excluded.message_id,
                 revision = excluded.revision,
                 content = excluded.content",
                params![
                    marker.message_id.to_string(),
                    marker.revision.storage_value(),
                    marker.content.as_storage_blob(),
                ],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        commit(transaction)
    }

    pub(crate) fn clear_loop_marker(&mut self) -> Result<(), CoreError> {
        let transaction = self.transaction()?;
        transaction
            .execute("DELETE FROM loop_marker", [])
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        commit(transaction)
    }

    pub(crate) fn clear_local_history(&mut self) -> Result<(), CoreError> {
        let transaction = self.transaction()?;
        transaction
            .execute_batch("DELETE FROM history; DELETE FROM processed_message;")
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        commit(transaction)
    }

    pub(crate) fn apply_generation_change(
        &mut self,
        history_epoch: &Uuid,
        clear_generation: u64,
        cursor: Option<u64>,
    ) -> Result<(), CoreError> {
        let transaction = self.transaction()?;
        transaction
            .execute(
                "DELETE FROM outbox WHERE clear_generation < ?1",
                [clear_generation.to_string()],
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        transaction
            .execute_batch(
                "DELETE FROM history;
                 DELETE FROM processed_message;
                 DELETE FROM loop_marker;",
            )
            .map_err(|_| CoreError::LocalStateUnavailable)?;
        set_metadata(&transaction, "history_epoch", &history_epoch.to_string())?;
        set_metadata(
            &transaction,
            "clear_generation",
            &clear_generation.to_string(),
        )?;
        if let Some(cursor) = cursor {
            set_metadata(&transaction, "last_cursor", &cursor.to_string())?;
        } else {
            transaction
                .execute("DELETE FROM metadata WHERE key = 'last_cursor'", [])
                .map_err(|_| CoreError::LocalStateUnavailable)?;
        }
        commit(transaction)
    }

    fn transaction(&mut self) -> Result<Transaction<'_>, CoreError> {
        self.connection
            .transaction()
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

    fn table_count(&self, table: &str) -> Result<usize, CoreError> {
        let statement = format!("SELECT COUNT(*) FROM {table}");
        self.connection
            .query_row(&statement, [], |row| row.get::<_, i64>(0))
            .map(|count| usize::try_from(count).unwrap_or(usize::MAX))
            .map_err(|_| CoreError::LocalStateUnavailable)
    }
}

fn validate_parent(path: &Path) -> Result<(), CoreError> {
    let parent = path.parent().ok_or(CoreError::StatePathInsecure)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| CoreError::StatePathInsecure)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(CoreError::StatePathInsecure);
    }
    validate_owner_mode(&metadata, 0o077)
}

fn validate_database_files(path: &Path) -> Result<(), CoreError> {
    validate_regular_owner_file(path)?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = path.with_file_name(format!(
            "{}{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or(CoreError::StatePathInsecure)?,
            suffix
        ));
        if fs::symlink_metadata(&sidecar).is_ok() {
            validate_regular_owner_file(&sidecar)?;
        }
    }
    Ok(())
}

fn validate_regular_owner_file(path: &Path) -> Result<(), CoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| CoreError::StatePathInsecure)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(CoreError::StatePathInsecure);
    }
    validate_owner_mode(&metadata, 0o177)
}

fn validate_owner_mode(metadata: &fs::Metadata, forbidden: u32) -> Result<(), CoreError> {
    #[cfg(unix)]
    if metadata.permissions().mode() & forbidden != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(CoreError::StatePathInsecure);
    }
    Ok(())
}

fn secure_sidecars(path: &Path) -> Result<(), CoreError> {
    #[cfg(unix)]
    for suffix in ["-wal", "-shm"] {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Err(CoreError::StatePathInsecure);
        };
        let sidecar = path.with_file_name(format!("{name}{suffix}"));
        if sidecar.exists() {
            fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600))
                .map_err(|_| CoreError::LocalStateUnavailable)?;
        }
    }
    Ok(())
}

fn commit(transaction: Transaction<'_>) -> Result<(), CoreError> {
    transaction
        .commit()
        .map_err(|_| CoreError::LocalStateUnavailable)
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

fn parse_counter(value: &str) -> Result<u64, CoreError> {
    value
        .parse::<u64>()
        .map_err(|_| CoreError::LocalStateUnavailable)
}

fn parse_counter_sql(value: String) -> rusqlite::Result<u64> {
    parse_counter(&value).map_err(|_| rusqlite::Error::InvalidQuery)
}

fn parse_uuid_sql(value: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&value).map_err(|_| rusqlite::Error::InvalidQuery)
}
