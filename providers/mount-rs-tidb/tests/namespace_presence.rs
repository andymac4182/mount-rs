//! Closed namespace-presence controls. Actual TiDB controls are opt-in.
//!
//! Inspection may initialize shared schemas and sessions. It must not insert
//! a namespace row, reserve a key, or imply that another writer cannot race it.

use mount_rs_core::ErrorCode;
use mount_rs_tidb::{TidbNamespacePresence, TidbPoolContext, TidbStorageOptions};
use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use std::time::{SystemTime, UNIX_EPOCH};

const TABLES: [&str; 5] = [
    "mount_rs_tidb_metadata",
    "mount_rs_tidb_inodes",
    "mount_rs_tidb_compact_guards",
    "mount_rs_tidb_block_authority",
    "mount_rs_tidb_blocks",
];

fn presence(flags: [bool; 5]) -> TidbNamespacePresence {
    TidbNamespacePresence {
        metadata: flags[0],
        inodes: flags[1],
        compact_guards: flags[2],
        block_authority: flags[3],
        blocks: flags[4],
    }
}

#[test]
fn namespace_presence_requires_every_table_to_be_absent() {
    assert!(presence([false; 5]).is_absent());
    for index in 0..5 {
        let mut flags = [false; 5];
        flags[index] = true;
        assert!(!presence(flags).is_absent(), "present table {index}");
    }
}

#[test]
fn namespace_presence_sql_is_closed_and_exact_key_parameterized() {
    let source = include_str!("../src/storage.rs");
    let sql = source
        .split_once("const NAMESPACE_PRESENCE_SQL: &str = \"")
        .expect("the closed presence statement must exist")
        .1
        .split_once("\";")
        .expect("the closed presence statement must end")
        .0;
    assert_eq!(sql.split_whitespace().next(), Some("SELECT"));
    assert_eq!(sql.matches("EXISTS(").count(), TABLES.len());
    assert_eq!(sql.matches('?').count(), TABLES.len());
    for table in TABLES {
        assert_eq!(
            sql.matches(&format!("SELECT 1 FROM {table} WHERE volume_key=?"))
                .count(),
            1,
            "the exact key must be bound once for {table}"
        );
    }
    for forbidden in ["INSERT", "UPDATE", "DELETE", "LIKE", "COUNT(", "FOR UPDATE"] {
        assert!(!sql.contains(forbidden), "unexpected statement {forbidden}");
    }
}

#[tokio::test]
async fn namespace_presence_rejects_invalid_scope_before_connecting() {
    let context = TidbPoolContext::new("mysql://unused@127.0.0.1:1/unused", 1).unwrap();
    for key in [String::new(), "nul\0key".into(), "x".repeat(256)] {
        let error = context
            .inspect_namespace_presence(&key)
            .await
            .expect_err("invalid keys must fail before opening the disconnected endpoint");
        assert!(error.is(ErrorCode::Einval));
    }
    context.close().await.unwrap();
    assert!(
        context
            .inspect_namespace_presence("valid-key")
            .await
            .expect_err("closed context must reject inspection before connecting")
            .is(ErrorCode::Estale)
    );
    context.close().await.unwrap();
}

fn tidb_url() -> String {
    std::env::var("MOUNT_RS_TIDB_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .expect("MOUNT_RS_TIDB_URL is required for the explicitly selected actual TiDB test")
}

async fn seed(connection: &mut mysql_async::Conn, table: usize, key: &str) {
    let statement = match table {
        0 => {
            "INSERT INTO mount_rs_tidb_metadata (volume_key,revision,namespace,owner,fence,expires) VALUES (?,0,NULL,NULL,0,0)"
        }
        1 => {
            "INSERT INTO mount_rs_tidb_inodes (volume_key,inode,generation,revision,node) VALUES (?,1,1,1,'{}')"
        }
        2 => {
            "INSERT INTO mount_rs_tidb_compact_guards (volume_key,inode,incarnation,epoch,revision,node) VALUES (?,1,1,1,1,'{}')"
        }
        3 => {
            "INSERT INTO mount_rs_tidb_block_authority (volume_key,backing_id) VALUES (?,REPEAT('a',32))"
        }
        4 => {
            "INSERT INTO mount_rs_tidb_blocks (volume_key,id,bytes) VALUES (?,CONCAT('b',REPEAT('0',64)),'owned-presence-control')"
        }
        _ => unreachable!(),
    };
    connection
        .exec_drop(statement, (key,))
        .await
        .unwrap_or_else(|_| panic!("could not seed the owned presence control"));
}

async fn delete(connection: &mut mysql_async::Conn, table: &str, key: &str) {
    assert!(TABLES.contains(&table));
    connection
        .exec_drop(format!("DELETE FROM {table} WHERE volume_key=?"), (key,))
        .await
        .unwrap_or_else(|_| panic!("could not remove the owned presence control"));
}

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_namespace_presence_covers_orphans_exact_keys_and_lifecycle() {
    let url = tidb_url();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let key = format!("namespace-presence-{}-{timestamp}", std::process::id());
    let adjacent = format!("{key} ");
    let pool = Pool::from_url(&url)
        .unwrap_or_else(|_| panic!("could not create the presence-control oracle"));
    let mut oracle = pool
        .get_conn()
        .await
        .unwrap_or_else(|_| panic!("could not connect the presence-control oracle"));
    let identity: Option<String> = oracle
        .query_first("SELECT tidb_version()")
        .await
        .unwrap_or_else(|_| panic!("could not verify actual TiDB identity"));
    assert!(identity.unwrap().to_ascii_lowercase().contains("tidb"));

    let context = TidbPoolContext::new(&url, 1)
        .unwrap_or_else(|_| panic!("could not create the owned presence context"));
    let empty = context
        .inspect_namespace_presence(&key)
        .await
        .unwrap_or_else(|_| panic!("could not inspect the owned namespace"));
    assert_eq!(empty, presence([false; 5]));

    // Independent row counts prove schema-only inspection created no owned row.
    for table in TABLES {
        let count: Option<u64> = oracle
            .exec_first(
                format!("SELECT COUNT(*) FROM {table} WHERE volume_key=?"),
                (&key,),
            )
            .await
            .unwrap_or_else(|_| panic!("could not inspect owned rows independently"));
        assert_eq!(count, Some(0), "inspection inserted a row in {table}");
    }

    // VARBINARY exact identity includes trailing spaces, across every family.
    for index in 0..TABLES.len() {
        seed(&mut oracle, index, &adjacent).await;
    }
    assert_eq!(
        context.inspect_namespace_presence(&key).await.unwrap(),
        empty
    );
    assert_eq!(
        context.inspect_namespace_presence(&adjacent).await.unwrap(),
        presence([true; 5])
    );

    // Each orphan family is observable even without the metadata anchor.
    for (index, table) in TABLES.iter().enumerate() {
        seed(&mut oracle, index, &key).await;
        let mut flags = [false; 5];
        flags[index] = true;
        assert_eq!(
            context.inspect_namespace_presence(&key).await.unwrap(),
            presence(flags),
            "missed independently present {table}"
        );
        delete(&mut oracle, table, &key).await;
    }

    let metadata = context
        .metadata(TidbStorageOptions::new(&key))
        .await
        .unwrap();
    let mut flags = [false; 5];
    flags[0] = true;
    assert_eq!(
        context.inspect_namespace_presence(&key).await.unwrap(),
        presence(flags)
    );
    // Store close does not disconnect its shared context; context close does.
    metadata.close().await.unwrap();
    assert!(
        !context
            .inspect_namespace_presence(&key)
            .await
            .unwrap()
            .is_absent()
    );
    context.close().await.unwrap();
    assert!(
        context
            .inspect_namespace_presence(&key)
            .await
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
    context.close().await.unwrap();

    for owned_key in [&key, &adjacent] {
        for table in TABLES {
            delete(&mut oracle, table, owned_key).await;
        }
    }
    drop(oracle);
    pool.disconnect()
        .await
        .unwrap_or_else(|_| panic!("could not close the presence-control oracle"));
}
