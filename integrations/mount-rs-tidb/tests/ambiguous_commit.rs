//! Failure-injection coverage for TiDB transaction commits.
//!
//! This test deliberately sits between the provider and a real TiDB SQL
//! endpoint. It forwards the MySQL wire protocol, lets TiDB finish a COMMIT,
//! then drops the client connection before the COMMIT response is delivered.
//! The provider must surface an unknown outcome and must not replay the
//! publication automatically.

use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{MetadataStore, Namespace, NodeData, NodeMetadata};
use mount_rs_core::{FsDriver, MemoryFs, S_IFDIR};
use mount_rs_tidb::{TidbMetadataStore, TidbStorageOptions};
use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use std::collections::BTreeMap;
use std::io::{Error, ErrorKind, Result as IoResult};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use url::Url;

fn tidb_url() -> String {
    std::env::var("MOUNT_RS_TIDB_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| {
            panic!("MOUNT_RS_TIDB_URL must be set when explicitly running the ignored TiDB test")
        })
}

fn unique_volume_key() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_nanos();
    format!("mount-rs-tidb-ambiguous-{}-{timestamp}", std::process::id())
}

fn options(volume_key: &str) -> TidbStorageOptions {
    TidbStorageOptions::new(volume_key)
        .with_durable(true)
        .with_max_block_bytes(4 * 1024 * 1024)
        .with_max_namespace_bytes(4 * 1024 * 1024)
}

async fn assert_actual_tidb(url: &str) {
    let pool = Pool::from_url(url).expect("the TiDB identity URL must be parseable");
    let mut connection = pool
        .get_conn()
        .await
        .expect("the TiDB identity connection must be reachable");
    let identity: Option<(String, String)> = connection
        .exec_first("SELECT tidb_version(), VERSION()", ())
        .await
        .expect("the TiDB identity query must succeed");
    let (tidb_version, version) = identity.expect("the TiDB identity query must return a row");
    let identity_text = format!("{tidb_version} {version}").to_ascii_lowercase();
    assert!(
        identity_text.contains("tidb"),
        "server identity did not identify TiDB: tidb_version()={tidb_version:?}, version={version:?}"
    );
    drop(connection);
    pool.disconnect()
        .await
        .expect("the TiDB identity pool must close");
}

async fn root_namespace() -> Namespace {
    let stats = MemoryFs::empty().stat("/").await.expect("root stat");
    let root = stats.ino;
    assert_eq!(stats.mode & mount_rs_core::S_IFMT, S_IFDIR);
    Namespace {
        format_version: 1,
        root,
        next_inode: root + 1,
        default_uid: 0,
        default_gid: 0,
        umask: 0o022,
        default_chunker: FixedSizeChunker::new(4096).expect("fixed chunker").config(),
        nodes: BTreeMap::from([(
            root,
            NodeMetadata {
                stats,
                data: NodeData::Directory { entries: vec![] },
            },
        )]),
    }
}

#[derive(Debug)]
struct MysqlPacket {
    sequence: u8,
    payload: Vec<u8>,
}

async fn read_packet<R>(reader: &mut R) -> IoResult<MysqlPacket>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).await?;
    let payload_len =
        usize::from(header[0]) | (usize::from(header[1]) << 8) | (usize::from(header[2]) << 16);
    const MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;
    if payload_len > MAX_PACKET_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "ambiguous-commit proxy received an oversized MySQL packet",
        ));
    }
    let mut payload = vec![0; payload_len];
    reader.read_exact(&mut payload).await?;
    Ok(MysqlPacket {
        sequence: header[3],
        payload,
    })
}

async fn write_packet<W>(writer: &mut W, packet: &MysqlPacket) -> IoResult<()>
where
    W: AsyncWrite + Unpin,
{
    let payload_len = packet.payload.len();
    if payload_len > 0x00ff_ffff {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "ambiguous-commit proxy cannot write an oversized MySQL packet",
        ));
    }
    let header = [
        (payload_len & 0xff) as u8,
        ((payload_len >> 8) & 0xff) as u8,
        ((payload_len >> 16) & 0xff) as u8,
        packet.sequence,
    ];
    writer.write_all(&header).await?;
    writer.write_all(&packet.payload).await?;
    writer.flush().await
}

fn is_commit_query(packet: &MysqlPacket) -> bool {
    packet.payload.first() == Some(&0x03)
        && std::str::from_utf8(&packet.payload[1..])
            .is_ok_and(|query| query.trim().eq_ignore_ascii_case("COMMIT"))
}

async fn relay_until_commit_response(client: TcpStream, upstream: TcpStream) -> IoResult<()> {
    let (mut client_reader, mut client_writer) = client.into_split();
    let (mut upstream_reader, mut upstream_writer) = upstream.into_split();

    loop {
        tokio::select! {
            packet = read_packet(&mut client_reader) => {
                let packet = packet?;
                let is_commit = is_commit_query(&packet);
                write_packet(&mut upstream_writer, &packet).await?;
                if is_commit {
                    // Reading the response proves TiDB has finished processing
                    // COMMIT. Drop both sockets before forwarding it so the
                    // provider cannot observe whether the commit succeeded.
                    let _commit_response = read_packet(&mut upstream_reader).await?;
                    return Ok(());
                }
            }
            packet = read_packet(&mut upstream_reader) => {
                let packet = packet?;
                write_packet(&mut client_writer, &packet).await?;
            }
        }
    }
}

async fn start_commit_drop_proxy(database_url: &str) -> (String, JoinHandle<IoResult<()>>) {
    let target = Url::parse(database_url).expect("the TiDB URL must be parseable");
    assert_eq!(
        target.scheme(),
        "mysql",
        "the commit failure-injection lane requires an unencrypted mysql:// URL"
    );
    let target_host = target
        .host_str()
        .expect("the TiDB URL must contain a host")
        .to_owned();
    let target_port = target.port().unwrap_or(3306);
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind the ambiguous-commit proxy");
    let proxy_port = listener
        .local_addr()
        .expect("read the ambiguous-commit proxy address")
        .port();
    let mut proxy_url = target.clone();
    proxy_url
        .set_host(Some("127.0.0.1"))
        .expect("rewrite the proxy host");
    proxy_url
        .set_port(Some(proxy_port))
        .expect("rewrite the proxy port");

    let task = tokio::spawn(async move {
        let (client, _) = listener.accept().await?;
        let upstream = TcpStream::connect((target_host.as_str(), target_port)).await?;
        // mysql_async may try to acquire another pooled connection after the
        // commit connection is dropped. Close those extra attempts instead
        // of leaving an established socket queued behind the single relay,
        // which would make the failure-injection test wait forever.
        let reject_extra: JoinHandle<IoResult<()>> = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((extra, _)) => drop(extra),
                    Err(error) => return Err(error),
                }
            }
        });
        let result = relay_until_commit_response(client, upstream).await;
        reject_extra.abort();
        let _ = reject_extra.await;
        result
    });
    (proxy_url.to_string(), task)
}

async fn delete_metadata_row(url: &str, volume_key: &str) {
    let pool = Pool::from_url(url).expect("the TiDB cleanup URL must be parseable");
    let mut connection = pool
        .get_conn()
        .await
        .expect("the TiDB cleanup connection must be reachable");
    connection
        .exec_drop(
            "DELETE FROM mount_rs_tidb_metadata WHERE volume_key=?",
            (volume_key,),
        )
        .await
        .expect("the scoped TiDB metadata row must be removable");
    drop(connection);
    pool.disconnect()
        .await
        .expect("the TiDB cleanup pool must close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_commit_outcome_is_ambiguous_and_not_replayed() {
    let direct_url = tidb_url();
    assert_actual_tidb(&direct_url).await;
    let volume_key = unique_volume_key();
    let direct = TidbMetadataStore::connect_with_options(&direct_url, options(&volume_key))
        .await
        .expect("connect the direct TiDB metadata store");
    let lease = direct
        .acquire_writer("tidb-ambiguous-commit", std::time::Duration::from_secs(30))
        .await
        .expect("acquire the TiDB writer before failure injection");

    let (proxy_url, proxy_task) = start_commit_drop_proxy(&direct_url).await;
    let via_proxy = TidbMetadataStore::connect_with_options(&proxy_url, options(&volume_key))
        .await
        .expect("connect the TiDB metadata store through the proxy");
    let error = via_proxy
        .publish(0, &lease, root_namespace().await)
        .await
        .expect_err("a dropped COMMIT response must not be reported as success");
    assert!(
        error.to_string().contains("commit outcome is unknown"),
        "ambiguous commit must be distinguishable from a retryable statement conflict: {error}"
    );
    proxy_task
        .await
        .expect("the ambiguous-commit proxy task must not panic")
        .expect("the proxy must observe and drop the TiDB COMMIT response");

    // Reconcile without replaying the publication. TiDB may have committed or
    // rolled back before the response was lost; both outcomes are valid here,
    // but the provider must not manufacture a second publication.
    let observed = direct
        .load()
        .await
        .expect("reconcile the direct TiDB metadata state");
    assert!(
        observed.revision == 0 || observed.revision == 1,
        "ambiguous publication must not advance more than once: revision={}",
        observed.revision
    );
    direct
        .release_writer(&lease)
        .await
        .expect("release the scoped TiDB writer after reconciliation");
    via_proxy
        .close()
        .await
        .expect("close the proxied TiDB metadata pool");
    direct
        .close()
        .await
        .expect("close the direct TiDB metadata pool");
    delete_metadata_row(&direct_url, &volume_key).await;
}
