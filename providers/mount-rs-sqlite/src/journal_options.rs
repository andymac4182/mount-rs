//! Construction-only journal policy. No authority enrollment or request-path work.

use super::*;

pub(super) fn validate_path(path: &Path, options: SqliteStorageOptions) -> Result<()> {
    if options.journal_mode == SqliteJournalMode::Wal
        && (path.as_os_str().is_empty()
            || path == Path::new(":memory:")
            || path.to_str().is_some_and(|path| path.starts_with("file:")))
    {
        return Err(FsError::new(ErrorCode::Einval)
            .with_syscall("configure SQLite journal mode")
            .with_message("SQLite WAL requires a local file path; empty, :memory: and file: paths are unsupported"));
    }
    Ok(())
}

pub(super) fn apply(database: &Database, options: SqliteStorageOptions) -> Result<()> {
    if options.journal_mode == SqliteJournalMode::Preserve {
        return Ok(());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = database;
        Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("configure SQLite journal mode")
            .with_message("SQLite WAL requires a qualified local Linux or macOS file"))
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let connection = database.lock()?;
        if !connection.is_autocommit() {
            return Err(backend_error(
                "SQLite journal configuration requires autocommit",
            ));
        }
        // A single file can contain both roles. Validate both without invoking
        // another role's initializer, which could enroll or migrate authority.
        validate_authorities(database, &connection)?;
        database.require_concurrent_local_file("WAL")?;
        let synchronous: i64 = connection
            .query_row("PRAGMA main.synchronous", [], |row| row.get(0))
            .map_err(backend_error)?;
        if synchronous != 2 {
            return Err(backend_error("SQLite WAL requires synchronous=FULL (2)"));
        }
        let mode: String = connection
            .query_row("PRAGMA main.journal_mode=WAL", [], |row| row.get(0))
            .map_err(backend_error)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(backend_error("SQLite refused journal_mode=WAL"));
        }
        let synchronous: i64 = connection
            .query_row("PRAGMA main.synchronous", [], |row| row.get(0))
            .map_err(backend_error)?;
        if synchronous != 2 {
            return Err(backend_error("SQLite WAL requires synchronous=FULL (2)"));
        }
        database.current_file_stamp()?;
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_authorities(database: &Database, connection: &Connection) -> Result<()> {
    if let Some(columns) = table_columns(connection, "mount_rs_metadata")? {
        require_primary_key(connection, "mount_rs_metadata", "id")?;
        // A legacy table without authority columns has no persisted authority
        // to inspect. Partial authority columns fail closed.
        let authority = [
            "write_mode",
            "backing_id",
            "physical_dev",
            "physical_ino",
            "physical_path",
        ];
        if authority.iter().any(|column| columns.contains(*column)) {
            require_columns("mount_rs_metadata", &columns, &authority)?;
            require_columns(
                "mount_rs_metadata",
                &columns,
                &["id", "owner", "fence", "expires"],
            )?;
            let mut statement = connection.prepare(
                "SELECT id, write_mode, backing_id, owner, fence, expires, physical_dev, physical_ino, physical_path FROM mount_rs_metadata"
            ).map_err(backend_error)?;
            let mut rows = statement.query([]).map_err(backend_error)?;
            let row = rows
                .next()
                .map_err(backend_error)?
                .ok_or_else(|| incompatible_schema("SQLite metadata authority is missing"))?;
            let id: i64 = row.get(0).map_err(backend_error)?;
            let mode: Option<String> = row.get(1).map_err(backend_error)?;
            let backing: Option<String> = row.get(2).map_err(backend_error)?;
            let owner: Option<String> = row.get(3).map_err(backend_error)?;
            let fence: i64 = row.get(4).map_err(backend_error)?;
            let expires: i64 = row.get(5).map_err(backend_error)?;
            let dev: Option<String> = row.get(6).map_err(backend_error)?;
            let ino: Option<String> = row.get(7).map_err(backend_error)?;
            let path: Option<String> = row.get(8).map_err(backend_error)?;
            let bound = matches!(
                mode.as_deref(),
                Some(
                    BOUND_WRITE_MODE | DELEGATED_WRITE_MODE | INODE_WRITE_MODE | COMPACT_WRITE_MODE
                )
            );
            let valid = match mode.as_deref() {
                None => backing.is_none() && fence != CONCURRENT_FENCE_SENTINEL,
                Some(CONCURRENT_WRITE_MODE) => {
                    backing.is_none()
                        && owner.is_none()
                        && fence == CONCURRENT_FENCE_SENTINEL
                        && expires == 0
                }
                _ if bound => {
                    backing
                        .as_deref()
                        .is_some_and(|id| ConcurrentBackingId::from_hex(id).is_ok())
                        && owner.is_none()
                        && fence == CONCURRENT_FENCE_SENTINEL
                        && expires == 0
                }
                _ => false,
            };
            if id != 1 || !valid || rows.next().map_err(backend_error)?.is_some() {
                return Err(incompatible_schema(
                    "SQLite metadata authority row is invalid",
                ));
            }
            let delegation_json: Option<String> = if columns.contains("delegation_state") {
                connection
                    .query_row(
                        "SELECT delegation_state FROM mount_rs_metadata WHERE id=1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(backend_error)?
            } else {
                None
            };
            let namespace_json: Option<String> = if mode.as_deref() == Some(DELEGATED_WRITE_MODE) {
                require_columns("mount_rs_metadata", &columns, &["namespace"])?;
                connection
                    .query_row(
                        "SELECT namespace FROM mount_rs_metadata WHERE id=1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(backend_error)?
            } else {
                None
            };
            validate_delegation_authority(
                mode.as_deref(),
                backing.as_deref(),
                delegation_json.as_deref(),
                namespace_json.as_deref(),
            )?;
            if bound || dev.is_some() || ino.is_some() || path.is_some() {
                require_matching_metadata_stamp(
                    database,
                    dev.as_deref(),
                    ino.as_deref(),
                    path.as_deref(),
                )?;
            }
        }
    }
    if let Some(columns) = table_columns(connection, "mount_rs_block_authority")? {
        require_authority_primary_key(connection)?;
        require_columns(
            "mount_rs_block_authority",
            &columns,
            &[
                "id",
                "backing_id",
                "physical_dev",
                "physical_ino",
                "physical_path",
            ],
        )?;
        let mut statement = connection.prepare(
            "SELECT id, backing_id, physical_dev, physical_ino, physical_path FROM mount_rs_block_authority"
        ).map_err(backend_error)?;
        let mut rows = statement.query([]).map_err(backend_error)?;
        if let Some(row) = rows.next().map_err(backend_error)? {
            let id: i64 = row.get(0).map_err(backend_error)?;
            let backing: String = row.get(1).map_err(backend_error)?;
            let dev: Option<String> = row.get(2).map_err(backend_error)?;
            let ino: Option<String> = row.get(3).map_err(backend_error)?;
            let path: Option<String> = row.get(4).map_err(backend_error)?;
            if id != 1
                || ConcurrentBackingId::from_hex(&backing).is_err()
                || rows.next().map_err(backend_error)?.is_some()
            {
                return Err(incompatible_schema("SQLite block authority row is invalid"));
            }
            let stored = FileStamp::from_text(dev.as_deref(), ino.as_deref())?
                .ok_or_else(|| incompatible_schema("SQLite block authority has no file stamp"))?;
            if stored != database.current_file_stamp()? {
                return Err(FsError::new(ErrorCode::Estale)
                    .with_syscall("configure SQLite journal mode")
                    .with_message("SQLite block authority belongs to another physical file"));
            }
            database.require_concurrent_local_file("blocks")?;
            require_matching_auxiliary_path(database, path.as_deref())?;
        }
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use futures_lite::future::block_on;

    const WAL: SqliteStorageOptions = SqliteStorageOptions {
        journal_mode: SqliteJournalMode::Wal,
    };

    fn mode(connection: &Connection) -> String {
        connection
            .query_row("PRAGMA main.journal_mode", [], |row| row.get(0))
            .unwrap()
    }

    fn assert_policy(database: &Database, expected: &str) {
        let connection = database.lock().unwrap();
        assert_eq!(mode(&connection), expected);
        assert_eq!(
            connection
                .query_row("PRAGMA main.synchronous", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    fn authority(connection: &Connection, metadata: bool) -> String {
        connection.query_row(if metadata {
            "SELECT json_array(write_mode,backing_id,physical_dev,physical_ino,physical_path) FROM mount_rs_metadata WHERE id=1"
        } else {
            "SELECT json_array(backing_id,physical_dev,physical_ino,physical_path) FROM mount_rs_block_authority WHERE id=1"
        }, [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn sqlite_journal_options_refuse_opposite_authority_without_primary_key() {
        for metadata_authority in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("database.db");
            let metadata = SqliteMetadataStore::open(&path).unwrap();
            let blocks = SqliteBlockStore::open(&path).unwrap();
            let backing = block_on(blocks.prepare_concurrent_backing()).unwrap();
            block_on(metadata.prepare_bound_concurrent_mode(backing)).unwrap();
            drop((metadata, blocks));
            let connection = Connection::open(&path).unwrap();
            let table = if metadata_authority {
                "mount_rs_metadata"
            } else {
                "mount_rs_block_authority"
            };
            connection.execute_batch(&format!(
                "CREATE TABLE malformed AS SELECT * FROM {table}; DROP TABLE {table}; ALTER TABLE malformed RENAME TO {table};"
            )).unwrap();
            let before = authority(&connection, metadata_authority);
            drop(connection);
            // The opposite constructor already refuses this schema. Selecting
            // WAL through the other role must refuse before journal mutation.
            if metadata_authority {
                assert!(SqliteMetadataStore::open(&path).is_err());
                assert!(SqliteBlockStore::open_with_options(&path, WAL).is_err());
            } else {
                assert!(SqliteBlockStore::open(&path).is_err());
                assert!(SqliteMetadataStore::open_with_options(&path, WAL).is_err());
            }
            let connection = Connection::open(&path).unwrap();
            assert_eq!(mode(&connection), "delete");
            assert_eq!(authority(&connection, metadata_authority), before);
        }
    }

    #[test]
    fn sqlite_journal_options_refuse_invalid_opposite_delegated_authority() {
        use mount_rs_core::{
            FsDriver,
            chunking::{Chunker, FixedSizeChunker},
            storage::NodeData,
        };

        for invalid_grants in [None, Some("{"), Some("{}")] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("database.db");
            let metadata = SqliteMetadataStore::open(&path).unwrap();
            let blocks = SqliteBlockStore::open(&path).unwrap();
            let backing = block_on(blocks.prepare_concurrent_backing()).unwrap();
            block_on(metadata.prepare_bound_concurrent_mode(backing)).unwrap();
            let stats = block_on(mount_rs_memfs::MemoryFs::empty().stat("/")).unwrap();
            let root = stats.ino;
            let namespace = Namespace {
                format_version: 1,
                root,
                next_inode: root + 1,
                default_uid: 0,
                default_gid: 0,
                umask: 0o022,
                default_chunker: FixedSizeChunker::new(4096).unwrap().config(),
                nodes: BTreeMap::from([(
                    root,
                    NodeMetadata {
                        stats,
                        data: NodeData::Directory { entries: vec![] },
                    },
                )]),
            };
            block_on(metadata.publish_bound_if_revision(backing, 0, namespace)).unwrap();
            block_on(metadata.prepare_delegated_mode(backing, 1)).unwrap();
            drop((metadata, blocks));
            let connection = Connection::open(&path).unwrap();
            connection
                .execute(
                    "UPDATE mount_rs_metadata SET delegation_state=?1 WHERE id=1",
                    params![invalid_grants],
                )
                .unwrap();
            let before: String = connection.query_row(
                "SELECT json_array(write_mode,backing_id,owner,fence,expires,physical_dev,physical_ino,physical_path,namespace,delegation_state) FROM mount_rs_metadata WHERE id=1",
                [], |row| row.get(0)
            ).unwrap();
            drop(connection);
            assert!(SqliteMetadataStore::open(&path).is_err());
            assert!(SqliteBlockStore::open_with_options(&path, WAL).is_err());
            let connection = Connection::open(&path).unwrap();
            assert_eq!(mode(&connection), "delete");
            let after: String = connection.query_row(
                "SELECT json_array(write_mode,backing_id,owner,fence,expires,physical_dev,physical_ino,physical_path,namespace,delegation_state) FROM mount_rs_metadata WHERE id=1",
                [], |row| row.get(0)
            ).unwrap();
            assert_eq!(after, before);
        }
    }

    #[test]
    fn sqlite_journal_options_default_preserves_delete_and_existing_wal() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("same.db");
        let metadata = SqliteMetadataStore::open(&path).unwrap();
        let blocks = SqliteBlockStore::open(&path).unwrap();
        assert_policy(&metadata.0, "delete");
        assert_policy(&blocks.0, "delete");
        drop((metadata, blocks));
        let metadata = SqliteMetadataStore::open_with_options(&path, WAL).unwrap();
        let blocks = SqliteBlockStore::open_with_options(&path, WAL).unwrap();
        assert_policy(&metadata.0, "wal");
        assert_policy(&blocks.0, "wal");
        drop((metadata, blocks));
        assert_policy(&SqliteMetadataStore::open(&path).unwrap().0, "wal");
        assert_policy(&SqliteBlockStore::open(&path).unwrap().0, "wal");
        assert_policy(&SqliteMetadataStore::in_memory().unwrap().0, "memory");
        assert_policy(&SqliteBlockStore::in_memory().unwrap().0, "memory");
    }

    #[test]
    fn sqlite_journal_options_both_roles_reopen_exact_committed_bytes() {
        for same_file in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let metadata_path = directory.path().join("metadata.db");
            let block_path = if same_file {
                metadata_path.clone()
            } else {
                directory.path().join("blocks.db")
            };
            let metadata = SqliteMetadataStore::open_with_options(&metadata_path, WAL).unwrap();
            let blocks = SqliteBlockStore::open_with_options(&block_path, WAL).unwrap();
            let payload: Vec<u8> = (0..131_079).map(|index| (index % 251) as u8).collect();
            let id = block_on(blocks.put(&payload)).unwrap();
            block_on(blocks.flush()).unwrap();
            assert_policy(&metadata.0, "wal");
            assert_policy(&blocks.0, "wal");
            let volume = metadata.1.clone();
            drop((metadata, blocks));
            let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
            let blocks = SqliteBlockStore::open(&block_path).unwrap();
            assert_eq!(metadata.1, volume);
            assert_eq!(block_on(blocks.get(&id)).unwrap(), payload);
            assert_policy(&metadata.0, "wal");
            assert_policy(&blocks.0, "wal");
        }
    }

    #[test]
    fn sqlite_journal_options_reject_special_paths_before_open() {
        for path in [
            "",
            ":memory:",
            "file::memory:",
            "file:somewhere.db?mode=rwc",
        ] {
            assert!(
                SqliteMetadataStore::open_with_options(path, WAL)
                    .err()
                    .unwrap()
                    .is(ErrorCode::Einval)
            );
            assert!(
                SqliteBlockStore::open_with_options(path, WAL)
                    .err()
                    .unwrap()
                    .is(ErrorCode::Einval)
            );
        }
    }

    #[test]
    fn sqlite_journal_options_hardlink_refusal_keeps_mode_and_symlink_succeeds() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("database.db");
        drop(SqliteMetadataStore::open(&path).unwrap());
        let link = directory.path().join("hard.db");
        std::fs::hard_link(&path, &link).unwrap();
        assert!(SqliteMetadataStore::open_with_options(&link, WAL).is_err());
        assert!(SqliteBlockStore::open_with_options(&path, WAL).is_err());
        assert_eq!(mode(&Connection::open(&path).unwrap()), "delete");
        std::fs::remove_file(link).unwrap();
        let symlink = directory.path().join("symbolic.db");
        std::os::unix::fs::symlink(&path, &symlink).unwrap();
        assert_policy(
            &SqliteMetadataStore::open_with_options(&symlink, WAL)
                .unwrap()
                .0,
            "wal",
        );
        assert_policy(
            &SqliteBlockStore::open_with_options(&symlink, WAL)
                .unwrap()
                .0,
            "wal",
        );
    }

    #[test]
    fn sqlite_journal_options_refuse_copied_and_opposite_role_authority_without_mutation() {
        // Opening blocks must validate metadata authority, and opening metadata
        // must validate block authority, even when the selected role is valid.
        for metadata_authority in [false, true] {
            for copied in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let original = directory.path().join("original.db");
                let metadata = SqliteMetadataStore::open(&original).unwrap();
                let blocks = SqliteBlockStore::open(&original).unwrap();
                let backing = block_on(blocks.prepare_concurrent_backing()).unwrap();
                block_on(metadata.prepare_bound_concurrent_mode(backing)).unwrap();
                drop((metadata, blocks));
                let path = if copied {
                    let copy = directory.path().join("copy.db");
                    std::fs::copy(&original, &copy).unwrap();
                    copy
                } else {
                    original
                };
                let connection = Connection::open(&path).unwrap();
                if !copied {
                    connection
                        .execute(
                            if metadata_authority {
                                "UPDATE mount_rs_metadata SET physical_path='00' WHERE id=1"
                            } else {
                                "UPDATE mount_rs_block_authority SET physical_path='00' WHERE id=1"
                            },
                            [],
                        )
                        .unwrap();
                }
                let before = authority(&connection, metadata_authority);
                assert_eq!(mode(&connection), "delete");
                drop(connection);
                if metadata_authority {
                    assert!(SqliteBlockStore::open_with_options(&path, WAL).is_err());
                } else {
                    assert!(SqliteMetadataStore::open_with_options(&path, WAL).is_err());
                }
                let connection = Connection::open(&path).unwrap();
                assert_eq!(mode(&connection), "delete");
                assert_eq!(authority(&connection, metadata_authority), before);
            }
        }
    }
}
