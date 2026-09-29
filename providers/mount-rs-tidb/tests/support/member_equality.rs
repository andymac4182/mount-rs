//! Actual server membership equality and fresh complete publication controls.
use super::*;

#[tokio::test]
#[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL; run serial"]
async fn actual_scoped_publication_returns_one_exact_membership_result() {
    for files in [4, 1_000] {
        let f = fixture(true).await;
        let empty = f.store.load_compact_snapshot(f.backing).await.unwrap();
        let mut namespace = empty.namespace().unwrap();
        for index in 0..files {
            add_file(&mut namespace, &format!("member-{index}"));
        }
        let setup =
            CompactStructuralDelta::capture(&empty, &namespace, StructuralScope::Full).unwrap();
        f.store.publish_compact_structure(&setup).await.unwrap();
        let before = f.store.load_compact_snapshot(f.backing).await.unwrap();
        let mut namespace = before.namespace().unwrap();
        add_file(&mut namespace, "exact-member-create");
        let delta =
            CompactStructuralDelta::capture(&before, &namespace, StructuralScope::FileCreate)
                .unwrap();
        let expected = delta.evaluate(&before).unwrap();
        let proxy = compact_proxy::Proxy::new(&f.url).await;
        let writer = TidbMetadataStore::connect_with_key(&proxy.url, &f.key)
            .await
            .unwrap();
        proxy.begin();
        let started = std::time::Instant::now();
        let publication = writer.publish_compact_structure(&delta).await;
        let elapsed = started.elapsed();
        let (queries, guard_rows) = proxy.end();
        let observer = TidbMetadataStore::connect_with_key(&f.url, &f.key)
            .await
            .unwrap();
        let actual = observer.load_compact_snapshot(f.backing).await;
        observer.close().await.unwrap();
        writer.close().await.unwrap();
        f.store.close().await.unwrap();
        proxy.shutdown().await;

        let publication = publication.unwrap();
        assert_eq!(publication.anchor, expected.anchor);
        assert_eq!(
            actual.unwrap(),
            expected,
            "fresh complete pure-reference parity"
        );
        assert_eq!(guard_rows, 1, "selected parent remains locked and complete");
        let membership_queries: Vec<_> = queries
            .iter()
            .filter(|sql| {
                sql.starts_with("SELECT ") && sql.contains(" FROM mount_rs_tidb_compact_members ")
            })
            .collect();
        assert_eq!(membership_queries.len(), 1);
        assert!(
            membership_queries[0]
                == "SELECT COUNT(*),COUNT(CASE WHEN inode BETWEEN ? AND ? THEN 1 END) FROM mount_rs_tidb_compact_members WHERE volume_key=?",
            "complete membership must be compared inside one server statement rather than returned as {} rows: {:?}",
            before.anchor.members.len(),
            membership_queries
        );
        assert_eq!(
            queries
                .iter()
                .filter(|sql| sql.eq_ignore_ascii_case("COMMIT"))
                .count(),
            1
        );
        eprintln!(
            "exact-member-control files={files} expected-members={} aggregate-statements=1 publication-ms={:.3} fresh-full-parity=true",
            before.anchor.members.len(),
            elapsed.as_secs_f64() * 1_000.0
        );
    }
}
