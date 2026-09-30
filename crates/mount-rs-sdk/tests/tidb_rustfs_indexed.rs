//! Correctness oracle for the production metadata/blob composition.
use mount_rs_rustfs::RustFsConfig;
use mount_rs_sdk::{Filesystem, SplitOptions, StorageContext, StoreConfig};
use mount_rs_tidb::TidbPoolContext;
use mysql_async::{IsolationLevel, Pool, TxOpts, prelude::Queryable};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const FIXTURE_FORMAT: &str = "mount-rs-tidb-rustfs-indexed-restart-v1";
const MAX_FIXTURE_BYTES: usize = 1024 * 1024;
const TABLES: [&str; 7] = [
    "mount_rs_tidb_metadata",
    "mount_rs_tidb_inodes",
    "mount_rs_tidb_compact_guards",
    "mount_rs_tidb_compact_members",
    "mount_rs_tidb_compact_dentries",
    "mount_rs_tidb_block_authority",
    "mount_rs_tidb_blocks",
];
// Root, renamed file, unlinked file tombstone, and the second persisted file.
const EXPECTED_ROW_COUNTS: [usize; 7] = [1, 0, 4, 4, 2, 0, 0];
const SNAPSHOT_QUERIES: [&str; 7] = [
    "SELECT CONCAT(revision,'|',IFNULL(HEX(write_mode),'~'),'|',IFNULL(HEX(backing_id),'~'),'|',IFNULL(HEX(owner),'~'),'|',fence,'|',expires,'|',IFNULL(HEX(namespace),'~'),'|',IFNULL(HEX(delegation),'~')) FROM mount_rs_tidb_metadata WHERE volume_key=?",
    "SELECT CONCAT(inode,'|',generation,'|',revision,'|',HEX(node)) FROM mount_rs_tidb_inodes WHERE volume_key=? ORDER BY inode",
    "SELECT CONCAT(inode,'|',incarnation,'|',epoch,'|',revision,'|',HEX(node)) FROM mount_rs_tidb_compact_guards WHERE volume_key=? ORDER BY inode",
    "SELECT CAST(inode AS CHAR) FROM mount_rs_tidb_compact_members WHERE volume_key=? ORDER BY inode",
    "SELECT CONCAT(parent,'|',ordinal,'|',HEX(name_hash),'|',HEX(name),'|',inode) FROM mount_rs_tidb_compact_dentries WHERE volume_key=? ORDER BY parent,ordinal",
    "SELECT HEX(backing_id) FROM mount_rs_tidb_block_authority WHERE volume_key=?",
    "SELECT CONCAT(HEX(id),'|',HEX(bytes)) FROM mount_rs_tidb_blocks WHERE volume_key=? ORDER BY id",
];
const SIZE_FIELDS: [&str; 7] = [
    "COALESCE(OCTET_LENGTH(namespace),0)+COALESCE(OCTET_LENGTH(delegation),0)+COALESCE(OCTET_LENGTH(backing_id),0)+COALESCE(OCTET_LENGTH(owner),0)",
    "OCTET_LENGTH(node)",
    "OCTET_LENGTH(node)",
    "0",
    "OCTET_LENGTH(name)+OCTET_LENGTH(name_hash)",
    "OCTET_LENGTH(backing_id)",
    "OCTET_LENGTH(id)+OCTET_LENGTH(bytes)",
];

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit owned fixture setting {name} required"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Standalone,
    Seed,
    Reopen,
}

fn phase(
    persisted: Option<&str>,
    topology: Option<&str>,
    owner: Option<&str>,
) -> Result<Phase, &'static str> {
    if !matches!(persisted, None | Some("0" | "1")) {
        return Err("restart expectation must be 0 or 1");
    }
    match (topology, owner, persisted) {
        (None, None, None | Some("0")) | (Some("single"), _, Some("0")) => Ok(Phase::Standalone),
        (Some("durable"), Some(owner), Some(expectation)) if canonical_component(owner) => {
            Ok(if expectation == "1" {
                Phase::Reopen
            } else {
                Phase::Seed
            })
        }
        _ => Err("restart qualification requires a complete owned durable harness phase"),
    }
}

fn canonical_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn canonical_prefix(value: &str) -> bool {
    value.len() <= 512 && value.split('/').all(canonical_component)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Binding {
    owner: String,
    key: String,
    prefix: String,
    endpoint: String,
    bucket: String,
    region: String,
}

impl Binding {
    fn fields(&self) -> [&str; 6] {
        [
            &self.owner,
            &self.key,
            &self.prefix,
            &self.endpoint,
            &self.bucket,
            &self.region,
        ]
    }

    fn validate(&self) -> Result<(), &'static str> {
        if !canonical_component(&self.owner)
            || !canonical_component(&self.key)
            || !canonical_prefix(&self.prefix)
            || self.fields().iter().any(|value| {
                value.is_empty() || value.len() > 1024 || value.contains(['\n', '\r', '\0'])
            })
        {
            return Err("invalid owned restart binding");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Witness {
    rows: [Vec<String>; 7],
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    encoded
}

fn unhex(value: &str) -> Result<Vec<u8>, &'static str> {
    if !value.len().is_multiple_of(2) || value.len() > MAX_FIXTURE_BYTES {
        return Err("invalid fixture hex length");
    }
    fn digit(byte: u8) -> Result<u8, &'static str> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err("noncanonical fixture hex"),
        }
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Ok(digit(pair[0])? * 16 + digit(pair[1])?))
        .collect()
}

fn number(value: &str) -> Result<u64, &'static str> {
    let number = value.parse::<u64>().map_err(|_| "invalid witness number")?;
    if number.to_string() != value {
        return Err("noncanonical witness number");
    }
    Ok(number)
}

impl Witness {
    fn validate(&self) -> Result<(), &'static str> {
        if self.rows.each_ref().map(Vec::len) != EXPECTED_ROW_COUNTS
            || self.rows.iter().flatten().map(String::len).sum::<usize>() > MAX_FIXTURE_BYTES / 2
        {
            return Err("unexpected normalized indexed row inventory");
        }
        let metadata = self.rows[0][0].split('|').collect::<Vec<_>>();
        if metadata.len() != 8
            || number(metadata[0])? == 0
            || unhex(metadata[1])? != b"MRC5"
            || unhex(metadata[2])?.len() != 32
            || metadata[3] != "~"
            || number(metadata[4])? != i64::MAX as u64
            || number(metadata[5])? != 0
            || metadata[7] != "~"
        {
            return Err("invalid indexed authority witness");
        }
        let namespace = unhex(metadata[6])?;
        if !namespace.starts_with(
            b"{\"layout\":\"mount-rs-tidb-indexed-compact\",\"version\":1,\"authority\":",
        ) || !namespace.ends_with(b"}")
        {
            return Err("wrong indexed authority layout");
        }
        for (index, row) in self.rows[2].iter().enumerate() {
            let fields = row.split('|').collect::<Vec<_>>();
            if fields.len() != 5
                || number(fields[0])? != index as u64 + 1
                || number(fields[1])? == 0
                || number(fields[2])? == 0
                || number(fields[3])? > i64::MAX as u64
                || unhex(fields[4])?.is_empty()
                || number(&self.rows[3][index])? != index as u64 + 1
            {
                return Err("invalid indexed guard or membership witness");
            }
        }
        for (index, row) in self.rows[4].iter().enumerate() {
            let fields = row.split('|').collect::<Vec<_>>();
            if fields.len() != 5
                || number(fields[0])? != 1
                || unhex(fields[2])?.len() != 32
                || unhex(fields[3])? != [b"renamed".as_slice(), b"tail".as_slice()][index]
                || number(fields[4])? != [2, 4][index]
            {
                return Err("invalid ordered indexed dentry witness");
            }
            let ordinal = number(fields[1])?;
            if index == 1 && ordinal <= number(self.rows[4][0].split('|').nth(1).unwrap())? {
                return Err("unordered indexed dentry witness");
            }
        }
        Ok(())
    }
}

fn fixture_body(binding: &Binding, witness: &Witness) -> Result<String, &'static str> {
    binding.validate()?;
    witness.validate()?;
    let mut body = format!("{FIXTURE_FORMAT}\n");
    for field in binding.fields() {
        body.push_str(&hex(field.as_bytes()));
        body.push('\n');
    }
    for (table, rows) in TABLES.iter().zip(&witness.rows) {
        body.push_str(&format!("{table}:{}\n", rows.len()));
        for row in rows {
            body.push_str(&hex(row.as_bytes()));
            body.push('\n');
        }
    }
    if body.len() > MAX_FIXTURE_BYTES {
        return Err("restart fixture exceeds its byte limit");
    }
    Ok(body)
}

fn parse_fixture(body: &str, expected: &Binding) -> Result<Witness, &'static str> {
    expected.validate()?;
    if body.len() > MAX_FIXTURE_BYTES || body.contains(['\r', '\0']) {
        return Err("invalid restart fixture body");
    }
    let mut lines = body
        .strip_suffix('\n')
        .ok_or("truncated restart fixture")?
        .split('\n');
    if lines.next() != Some(FIXTURE_FORMAT) {
        return Err("wrong restart fixture format");
    }
    for expected in expected.fields() {
        if unhex(lines.next().ok_or("missing restart binding")?)? != expected.as_bytes() {
            return Err("foreign restart fixture binding");
        }
    }
    let mut witness = Witness {
        rows: std::array::from_fn(|_| Vec::new()),
    };
    for (index, table) in TABLES.iter().enumerate() {
        let header = lines.next().ok_or("missing witness family")?;
        let count = number(
            header
                .strip_prefix(&format!("{table}:"))
                .ok_or("wrong witness family")?,
        )?;
        if count != EXPECTED_ROW_COUNTS[index] as u64 {
            return Err("wrong witness family inventory");
        }
        for _ in 0..count {
            witness.rows[index].push(
                String::from_utf8(unhex(lines.next().ok_or("missing witness row")?)?)
                    .map_err(|_| "non-UTF8 witness row")?,
            );
        }
    }
    if lines.next().is_some() {
        return Err("trailing restart fixture data");
    }
    witness.validate()?;
    Ok(witness)
}

fn exact_witness(expected: &Witness, actual: &Witness) -> Result<(), &'static str> {
    expected.validate()?;
    actual.validate()?;
    if expected != actual {
        return Err("persisted normalized indexed rows changed or disappeared");
    }
    Ok(())
}

fn create_fixture(path: &Path) -> File {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .expect("reserve fresh owned indexed restart fixture")
}

fn read_fixture(path: &Path, binding: &Binding) -> (String, Witness) {
    let before =
        std::fs::symlink_metadata(path).expect("indexed restart fixture must survive restart");
    assert!(before.is_file() && !before.file_type().is_symlink());
    assert!(before.len() <= MAX_FIXTURE_BYTES as u64);
    let mut file = File::open(path).expect("open retained indexed restart fixture");
    let opened = file
        .metadata()
        .expect("inspect opened indexed restart fixture");
    assert!(opened.is_file() && opened.len() == before.len());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!((before.dev(), before.ino()), (opened.dev(), opened.ino()));
        assert_eq!(opened.nlink(), 1);
        assert_eq!(
            opened.mode() & 0o077,
            0,
            "indexed restart fixture must be private"
        );
    }
    let mut body = String::new();
    (&mut file)
        .take(MAX_FIXTURE_BYTES as u64 + 1)
        .read_to_string(&mut body)
        .expect("read bounded indexed restart fixture");
    assert_eq!(body.len() as u64, opened.len());
    let witness = parse_fixture(&body, binding).expect("validate owned indexed restart fixture");
    (body, witness)
}

async fn normalized_witness(connection: &str, key: &str) -> Witness {
    let pool = Pool::from_url(connection).unwrap();
    let mut sql = pool.get_conn().await.unwrap();
    let mut options = TxOpts::default();
    options.with_isolation_level(IsolationLevel::RepeatableRead);
    let mut transaction = sql.start_transaction(options).await.unwrap();
    // Bound the actual graph and bodies in the same coherent view before
    // materializing HEX payloads. A foreign/oversized scope cannot turn the
    // restart check into an unbounded row or blob read.
    let mut total_bytes = 0_u64;
    for (index, table) in TABLES.iter().enumerate() {
        let sizes: Option<(u64, u64)> = transaction
            .exec_first(
                format!(
                    "SELECT COUNT(*),COALESCE(SUM({}),0) FROM {table} WHERE volume_key=?",
                    SIZE_FIELDS[index]
                ),
                (key,),
            )
            .await
            .unwrap();
        let (count, bytes) = sizes.expect("indexed witness inventory row");
        assert_eq!(
            count, EXPECTED_ROW_COUNTS[index] as u64,
            "indexed witness inventory changed"
        );
        total_bytes = total_bytes
            .checked_add(bytes)
            .expect("indexed witness byte count overflow");
        assert!(
            total_bytes <= MAX_FIXTURE_BYTES as u64 / 8,
            "indexed witness exceeds bounded source bytes"
        );
    }
    let mut witness = Witness {
        rows: std::array::from_fn(|_| Vec::new()),
    };
    for (index, query) in SNAPSHOT_QUERIES.iter().enumerate() {
        witness.rows[index] = transaction.exec(*query, (key,)).await.unwrap();
    }
    transaction.commit().await.unwrap();
    drop(sql);
    pool.disconnect().await.unwrap();
    witness
        .validate()
        .expect("actual normalized indexed witness must match the seeded layout");
    witness
}

async fn assert_metadata_absent(connection: &str, key: &str) {
    let context = TidbPoolContext::new(connection, 1).unwrap();
    let presence = context.inspect_namespace_presence(key).await.unwrap();
    context.close().await.unwrap();
    assert!(
        presence.is_absent(),
        "owned indexed metadata scope was not absent"
    );
}

#[cfg(test)]
mod restart_controls {
    use super::*;

    #[test]
    fn durable_seed_and_reopen_select_different_phases() {
        assert_eq!(
            phase(Some("0"), Some("durable"), Some("owned-run")),
            Ok(Phase::Seed)
        );
        assert_eq!(
            phase(Some("1"), Some("durable"), Some("owned-run")),
            Ok(Phase::Reopen)
        );
    }

    #[test]
    fn standalone_and_single_topology_finish_in_one_run() {
        assert_eq!(phase(None, None, None), Ok(Phase::Standalone));
        assert_eq!(
            phase(Some("0"), Some("single"), Some("owned-run")),
            Ok(Phase::Standalone)
        );
    }

    #[test]
    fn persisted_expectation_requires_owned_durable_harness() {
        assert!(phase(Some("1"), None, None).is_err());
        assert!(phase(Some("1"), Some("single"), Some("owned-run")).is_err());
        assert!(phase(Some("1"), Some("durable"), None).is_err());
        assert!(phase(Some("0"), Some("durable"), Some("")).is_err());
    }

    #[test]
    fn malformed_or_partial_harness_phase_is_rejected() {
        assert!(phase(Some("true"), Some("durable"), Some("owned-run")).is_err());
        assert!(phase(None, Some("durable"), Some("owned-run")).is_err());
        assert!(phase(Some("0"), Some("other"), Some("owned-run")).is_err());
    }

    fn binding() -> Binding {
        Binding {
            owner: "owned-run".to_owned(),
            key: "owned-run-indexed".to_owned(),
            prefix: "owned-run/indexed".to_owned(),
            endpoint: "http://127.0.0.1:9000".to_owned(),
            bucket: "owned-bucket".to_owned(),
            region: "local".to_owned(),
        }
    }

    fn witness() -> Witness {
        let namespace = br#"{"layout":"mount-rs-tidb-indexed-compact","version":1,"authority":{"generation":10}}"#;
        let mut witness = Witness {
            rows: std::array::from_fn(|_| Vec::new()),
        };
        witness.rows[0] = vec![format!(
            "10|{}|{}|~|{}|0|{}|~",
            hex(b"MRC5"),
            hex(&[0xab; 32]),
            i64::MAX,
            hex(namespace)
        )];
        for inode in 1..=4 {
            witness.rows[2].push(format!(
                "{inode}|1|10|0|{}",
                hex(b"{\"stats\":{},\"data\":{}}")
            ));
            witness.rows[3].push(inode.to_string());
        }
        witness.rows[4] = vec![
            format!("1|2|{}|{}|2", hex(&[0xcd; 32]), hex(b"renamed")),
            format!("1|3|{}|{}|4", hex(&[0xef; 32]), hex(b"tail")),
        ];
        witness
    }

    #[test]
    fn canonical_owned_fixture_round_trips_every_row_and_binding() {
        let expected = witness();
        let body = fixture_body(&binding(), &expected).unwrap();
        assert_eq!(parse_fixture(&body, &binding()), Ok(expected.clone()));
        assert_eq!(exact_witness(&expected, &expected), Ok(()));
    }

    #[test]
    fn every_foreign_scope_and_service_binding_is_rejected() {
        let body = fixture_body(&binding(), &witness()).unwrap();
        for field in 0..6 {
            let mut foreign = binding();
            match field {
                0 => foreign.owner = "other-run".to_owned(),
                1 => foreign.key = "other-volume".to_owned(),
                2 => foreign.prefix = "other-run/indexed".to_owned(),
                3 => foreign.endpoint = "http://127.0.0.1:9001".to_owned(),
                4 => foreign.bucket = "other-bucket".to_owned(),
                5 => foreign.region = "other-region".to_owned(),
                _ => unreachable!(),
            }
            assert_eq!(
                parse_fixture(&body, &foreign),
                Err("foreign restart fixture binding")
            );
        }
    }

    #[test]
    fn absent_truncated_future_trailing_and_noncanonical_fixture_frames_fail_closed() {
        let body = fixture_body(&binding(), &witness()).unwrap();
        for invalid in [
            String::new(),
            body[..body.len() - 1].to_owned(),
            body.replacen(FIXTURE_FORMAT, "future-format", 1),
            format!("{body}extra\n"),
            body.replacen(&hex(b"owned-run"), "7g", 1),
            body.replacen(
                "mount_rs_tidb_compact_members:4",
                "mount_rs_tidb_compact_members:04",
                1,
            ),
            body.replacen(
                "mount_rs_tidb_compact_guards:4",
                "mount_rs_tidb_compact_guards:0",
                1,
            ),
            body.replace('\n', "\r\n"),
            "x".repeat(MAX_FIXTURE_BYTES + 1),
        ] {
            assert!(parse_fixture(&invalid, &binding()).is_err());
        }
    }

    #[test]
    fn missing_mistyped_or_wrong_layout_rows_fail_closed() {
        let mut invalid = witness();
        invalid.rows[3].pop();
        assert!(invalid.validate().is_err());
        let mut invalid = witness();
        invalid.rows[3][0] = "01".to_owned();
        assert!(invalid.validate().is_err());
        let mut invalid = witness();
        invalid.rows[0][0] = invalid.rows[0][0].replacen(&hex(b"MRC5"), &hex(b"MRC2"), 1);
        assert!(invalid.validate().is_err());
        let mut invalid = witness();
        let mut fields = invalid.rows[0][0]
            .split('|')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        fields[6] =
            hex(br#"{"layout":"mount-rs-tidb-indexed-compact","version":2,"authority":{}}"#);
        invalid.rows[0][0] = fields.join("|");
        assert!(invalid.validate().is_err());
        let mut invalid = witness();
        invalid.rows[4].reverse();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn changed_authority_guard_revision_body_or_ordinal_never_matches_retained_witness() {
        let expected = witness();
        for change in 0..4 {
            let mut actual = expected.clone();
            match change {
                0 => actual.rows[0][0] = actual.rows[0][0].replacen("10|", "11|", 1),
                1 => actual.rows[2][1] = actual.rows[2][1].replacen("|0|", "|1|", 1),
                2 => {
                    actual.rows[2][1] = actual.rows[2][1].replacen(
                        &hex(b"{\"stats\":{},\"data\":{}}"),
                        &hex(b"{\"stats\":{\"size\":1},\"data\":{}}"),
                        1,
                    )
                }
                3 => actual.rows[4][1] = actual.rows[4][1].replacen("1|3|", "1|4|", 1),
                _ => unreachable!(),
            }
            assert!(
                actual.validate().is_ok(),
                "mutation is a well-framed different row"
            );
            assert_eq!(
                exact_witness(&expected, &actual),
                Err("persisted normalized indexed rows changed or disappeared")
            );
        }
    }

    #[test]
    fn owned_scope_components_and_fixture_bytes_are_bounded() {
        for prefix in [
            "",
            "/owned",
            "owned/",
            "owned/../indexed",
            "owned//indexed",
            "owned/%2f",
        ] {
            assert!(!canonical_prefix(prefix));
        }
        assert!(!canonical_prefix(&"x".repeat(513)));
        assert!(canonical_prefix("owned-run/indexed"));
        let mut too_large = witness();
        too_large.rows[2][0] = "x".repeat(MAX_FIXTURE_BYTES);
        assert!(fixture_body(&binding(), &too_large).is_err());
    }
}

fn payload(seed: usize, len: usize) -> Vec<u8> {
    (0..len)
        .map(|n| ((n * 37 + seed * 19 + n / 97) % 251) as u8)
        .collect()
}

async fn read_exact_file(fs: &Filesystem, path: &str, expected: &[u8]) {
    let handle = fs.driver().open(path, "r", 0).await.unwrap();
    let mut actual = vec![0; expected.len()];
    let mut offset = 0;
    while offset < actual.len() {
        let count = handle
            .read(&mut actual[offset..], Some(offset as u64))
            .await
            .unwrap();
        assert!(count > 0, "short acknowledged file");
        offset += count;
    }
    assert_eq!(actual, expected);
    assert_eq!(
        handle
            .read(&mut [0], Some(expected.len() as u64))
            .await
            .unwrap(),
        0
    );
    handle.close().await.unwrap();
}

fn final_renamed_bytes() -> Vec<u8> {
    let mut bytes = payload(1, 12_345);
    bytes[2048..6144].copy_from_slice(&payload(9, 4096));
    bytes.truncate(4097);
    bytes.resize(8193, 0);
    bytes
}

async fn assert_persisted_files(fs: &Filesystem) {
    read_exact_file(fs, "/renamed", &final_renamed_bytes()).await;
    read_exact_file(fs, "/tail", &payload(13, 6001)).await;
    for absent in ["/a", "/b"] {
        let error = match fs.driver().open(absent, "r", 0).await {
            Ok(_) => panic!("removed indexed path unexpectedly persisted"),
            Err(error) => error,
        };
        assert!(error.is(mount_rs_core::ErrorCode::Enoent));
    }
    let names = fs
        .driver()
        .readdir_bounded("/", 2)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["renamed", "tail"]);
}

async fn cleanup_scope(connection: &str, binding: &Binding, config: &RustFsConfig) {
    // Scope is either fresh and witnessed absent by this run, or exactly bound
    // by the retained fixture and checked against all persisted rows.
    let pool = Pool::from_url(connection).unwrap();
    let mut sql = pool.get_conn().await.unwrap();
    for table in TABLES {
        sql.exec_drop(
            format!("DELETE FROM {table} WHERE volume_key=?"),
            (&binding.key,),
        )
        .await
        .unwrap();
    }
    drop(sql);
    pool.disconnect().await.unwrap();
    assert_metadata_absent(connection, &binding.key).await;
    let store = config.build_store().unwrap();
    let exact_scope = format!("{}/", binding.prefix);
    let listed = store
        .list_with_delimiter(Some(&exact_scope.clone().into()))
        .await
        .unwrap();
    let mut prefixes = listed.common_prefixes;
    let mut objects = listed
        .objects
        .into_iter()
        .map(|object| object.location)
        .collect::<Vec<_>>();
    let mut pages = 1;
    assert!(prefixes.len() <= 32 && objects.len() <= 128);
    while let Some(prefix) = prefixes.pop() {
        assert!(prefix.as_ref().starts_with(&exact_scope));
        let listed = store.list_with_delimiter(Some(&prefix)).await.unwrap();
        for child in &listed.common_prefixes {
            assert!(child.as_ref().starts_with(&exact_scope));
        }
        prefixes.extend(listed.common_prefixes);
        objects.extend(listed.objects.into_iter().map(|object| object.location));
        pages += 1;
        assert!(pages <= 32 && prefixes.len() <= 32 && objects.len() <= 128);
    }
    for location in &objects {
        assert!(location.as_ref().starts_with(&exact_scope));
    }
    for location in objects {
        store.delete(&location).await.unwrap();
    }
    assert!(
        config
            .observe_owned_prefix_absence(&binding.prefix)
            .await
            .unwrap()
    );
}

#[tokio::test]
#[ignore = "requires explicit owned TiDB and RustFS fixture settings"]
async fn actual_tidb_rustfs_indexed_peer_writes_patterns_unlink_and_fresh_reopen() {
    let connection = required("MOUNT_RS_TIDB_URL");
    let config = RustFsConfig {
        endpoint: required("MOUNT_RS_RUSTFS_ENDPOINT"),
        bucket: required("MOUNT_RS_RUSTFS_BUCKET"),
        region: required("MOUNT_RS_RUSTFS_REGION"),
        access_key_id: required("MOUNT_RS_RUSTFS_ACCESS_KEY_ID"),
        secret_access_key: required("MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY"),
    };
    let persisted = std::env::var("MOUNT_RS_TIDB_EXPECT_PERSISTED").ok();
    let topology = std::env::var("MOUNT_RS_TIDB_TOPOLOGY").ok();
    let owner = std::env::var("MOUNT_RS_TIDB_CONSUMER_RUN_ID").ok();
    let phase = phase(persisted.as_deref(), topology.as_deref(), owner.as_deref())
        .expect("explicit owned indexed restart phase");
    let (owner, key, prefix, fixture_path) = if phase == Phase::Standalone {
        let key = format!(
            "indexed-rustfs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        (key.clone(), key.clone(), format!("{key}/blocks"), None)
    } else {
        let key = format!("{}-indexed", required("MOUNT_RS_TIDB_CHUNKED_VOLUME_KEY"));
        let prefix = format!("{}/indexed", required("MOUNT_RS_TIDB_RUSTFS_PREFIX"));
        let path = PathBuf::from(format!(
            "{}.indexed",
            required("MOUNT_RS_TIDB_CHUNKED_RUSTFS_FIXTURE")
        ));
        assert!(
            path.is_absolute(),
            "durable indexed fixture path must be absolute"
        );
        if let Ok(transient) = std::env::var("RUSTFS_RUN_DIR") {
            assert!(
                !path.starts_with(transient),
                "indexed restart fixture must survive harness phases"
            );
        }
        (owner.unwrap(), key, prefix, Some(path))
    };
    let binding = Binding {
        owner,
        key: key.clone(),
        prefix: prefix.clone(),
        endpoint: config.endpoint.clone(),
        bucket: config.bucket.clone(),
        region: config.region.clone(),
    };
    binding.validate().expect("owned indexed scope binding");
    println!("INDEXED_OWNED_SCOPE metadata={key} blobs={prefix}");
    let options = || {
        let mut options =
            SplitOptions::memory("indexed-rustfs-oracle", 4096).with_compact_inode_updates(true);
        options.metadata = StoreConfig::Tidb {
            connection: connection.clone(),
            volume_key: key.clone(),
            durable: true,
        };
        options.blocks = StoreConfig::RustFs {
            endpoint: config.endpoint.clone(),
            bucket: config.bucket.clone(),
            region: config.region.clone(),
            access_key_id: config.access_key_id.clone(),
            secret_access_key: config.secret_access_key.clone(),
            prefix: prefix.clone(),
            durable: true,
        };
        options
    };
    if phase == Phase::Reopen {
        let fixture_path = fixture_path.as_ref().unwrap();
        // Both ownership and the complete pre-restart physical witness are
        // checked before constructing a filesystem that could initialize rows.
        let (body, expected) = read_fixture(fixture_path, &binding);
        let actual = normalized_witness(&connection, &key).await;
        exact_witness(&expected, &actual)
            .expect("byte-exact indexed metadata survived TiDB restart");
        let context = StorageContext::new(2).unwrap();
        let reopened = Filesystem::split_with_context(options(), &context)
            .await
            .unwrap();
        assert_persisted_files(&reopened).await;
        let patch = payload(14, 513);
        let file = reopened.driver().open("/renamed", "r+", 0).await.unwrap();
        assert_eq!(file.write(&patch, Some(123)).await.unwrap(), patch.len());
        file.close().await.unwrap();
        let mut expected_bytes = final_renamed_bytes();
        expected_bytes[123..636].copy_from_slice(&patch);
        read_exact_file(&reopened, "/renamed", &expected_bytes).await;
        reopened.shutdown().await.unwrap();
        context.close().await.unwrap();
        drop((reopened, context));
        // Failed verification keeps the scope and witness for diagnosis.
        assert_eq!(read_fixture(fixture_path, &binding).0, body);
        cleanup_scope(&connection, &binding, &config).await;
        assert_eq!(read_fixture(fixture_path, &binding).0, body);
        std::fs::remove_file(fixture_path).expect("remove successfully qualified indexed fixture");
        println!(
            "INDEXED_TIDB_RUSTFS_REOPEN_PASS persisted_files=2 full_bytes=verified ordered_root=verified normalized_authority_guards_members_dentries=byte_exact post_restart_write=verified cleanup=complete"
        );
        return;
    }
    let mut fixture = fixture_path.as_ref().map(|path| create_fixture(path));
    assert_metadata_absent(&connection, &key).await;
    assert!(config.observe_owned_prefix_absence(&prefix).await.unwrap());
    let first_context = StorageContext::new(4).unwrap();
    let first = Filesystem::split_with_context(options(), &first_context)
        .await
        .unwrap();
    let original = payload(1, 12_345);
    first.driver().write_file("/a", &original).await.unwrap();
    first
        .driver()
        .write_file("/b", &payload(2, 8192))
        .await
        .unwrap();
    let second_context = StorageContext::new(4).unwrap();
    let second = Filesystem::split_with_context(options(), &second_context)
        .await
        .unwrap();
    read_exact_file(&second, "/a", &original).await;

    let a = first.driver().open("/a", "r+", 0).await.unwrap();
    let b = second.driver().open("/b", "r+", 0).await.unwrap();
    let patch_a = payload(9, 4096);
    let patch_b = payload(10, 4096);
    let (written_a, written_b) =
        tokio::join!(a.write(&patch_a, Some(2048)), b.write(&patch_b, Some(0)));
    assert_eq!(written_a.unwrap(), patch_a.len());
    assert_eq!(written_b.unwrap(), patch_b.len());
    a.close().await.unwrap();
    b.close().await.unwrap();
    let mut expected_a = original;
    expected_a[2048..6144].copy_from_slice(&patch_a);
    let mut expected_b = payload(2, 8192);
    expected_b[..4096].copy_from_slice(&patch_b);
    read_exact_file(&second, "/a", &expected_a).await;
    read_exact_file(&first, "/b", &expected_b).await;

    let append = second.driver().open("/a", "a", 0).await.unwrap();
    let tail = payload(11, 173);
    assert_eq!(append.write(&tail, None).await.unwrap(), tail.len());
    append.close().await.unwrap();
    expected_a.extend_from_slice(&tail);
    first.driver().rename("/a", "/renamed").await.unwrap();
    read_exact_file(&second, "/renamed", &expected_a).await;
    second.driver().truncate("/renamed", 4097).await.unwrap();
    expected_a.truncate(4097);
    first.driver().truncate("/renamed", 8193).await.unwrap();
    expected_a.resize(8193, 0);
    read_exact_file(&second, "/renamed", &expected_a).await;
    let orphan = second.driver().open("/b", "r+", 0).await.unwrap();
    first.driver().unlink("/b").await.unwrap();
    let mut orphan_bytes = vec![0; expected_b.len()];
    assert_eq!(
        orphan.read(&mut orphan_bytes, Some(0)).await.unwrap(),
        expected_b.len()
    );
    assert_eq!(orphan_bytes, expected_b);
    let orphan_patch = payload(12, 97);
    assert_eq!(
        orphan.write(&orphan_patch, Some(33)).await.unwrap(),
        orphan_patch.len()
    );
    expected_b[33..130].copy_from_slice(&orphan_patch);
    assert_eq!(
        orphan.read(&mut orphan_bytes, Some(0)).await.unwrap(),
        expected_b.len()
    );
    assert_eq!(orphan_bytes, expected_b);
    orphan.close().await.unwrap();
    assert!(first.driver().open("/b", "r", 0).await.is_err());
    first
        .driver()
        .write_file("/tail", &payload(13, 6001))
        .await
        .unwrap();
    first.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
    first_context.close().await.unwrap();
    second_context.close().await.unwrap();
    drop((first, second, first_context, second_context));

    let fresh_context = StorageContext::new(2).unwrap();
    let fresh = Filesystem::split_with_context(options(), &fresh_context)
        .await
        .unwrap();
    assert_eq!(expected_a, final_renamed_bytes());
    assert_persisted_files(&fresh).await;
    fresh.shutdown().await.unwrap();
    fresh_context.close().await.unwrap();
    drop((fresh, fresh_context));

    let witness = normalized_witness(&connection, &key).await;
    if let Some(fixture) = fixture.as_mut() {
        let body = fixture_body(&binding, &witness).unwrap();
        fixture
            .write_all(body.as_bytes())
            .expect("persist normalized indexed restart witness");
        fixture
            .sync_all()
            .expect("flush normalized indexed restart witness");
        println!(
            "INDEXED_TIDB_RUSTFS_SEED_PASS persisted_files=2 full_bytes=verified ordered_root=verified normalized_authority_guards_members_dentries=witnessed cleanup=retained"
        );
        return;
    }
    cleanup_scope(&connection, &binding, &config).await;
    println!(
        "INDEXED_TIDB_RUSTFS_PASS two_contexts=2 concurrent_disjoint_writes=2 full_bytes=verified fresh_reopen=1 cleanup=complete"
    );
}
