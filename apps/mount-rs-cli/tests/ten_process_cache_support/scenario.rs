//! Ten real CLI PIDs, with independent SQLite bytes/metadata and generation-only cache banks.
use super::{
    config::{CLUSTER, CacheSettings, Fixture, sha256, write},
    contracts::*,
    process::{Fleet, free_disk, native_deadline},
};
use mount_rs_blob_cache::{
    CacheScope, Discovery, DiscoveryMode, FixedDiscovery, LocalCache, PeerId, ScopeIdentity,
};
use mount_rs_core::{
    ErrorCode, FsDriver, Loopback,
    storage::{BlockId, BlockStore, MetadataStore, NodeData},
};
use mount_rs_remote_client::{
    connection::{ClientError, RemoteConnection},
    credentials::CredentialSource,
    driver::RemoteFsDriver,
};
use mount_rs_remote_protocol::{Operation, OperationName};
use mount_rs_sdk::{Filesystem, SplitOptions, StoreConfig};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    future::Future,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

async fn checked<T, F>(fleet: &mut Fleet, parent: Instant, future: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let deadline = clipped(Instant::now(), parent, REQUEST_SECONDS)?;
    tokio::pin!(future);
    loop {
        if Instant::now() >= deadline {
            return Err("request/operation deadline exhausted; no replay".into());
        }
        fleet.check()?;
        tokio::select! {
            result = &mut future => {
                let value = result?;
                fleet.check()?;
                if Instant::now() >= deadline { return Err("late operation completion; no replay".into()); }
                return Ok(value);
            }
            _ = tokio::time::sleep(Duration::from_millis(POLL_MS)) => {}
        }
    }
}
fn split(fixture: &Fixture, n: usize, owner: &str) -> SplitOptions {
    let mut options = SplitOptions::memory(owner, BLOCK_BYTES).with_compact_inode_updates(true);
    options.metadata = StoreConfig::Sqlite {
        path: fixture.root.join(format!("metadata-{n}.sqlite")),
    };
    options.blocks = StoreConfig::Sqlite {
        path: fixture.root.join(format!("blocks-{n}.sqlite")),
    };
    options
}
async fn connect(
    fleet: &mut Fleet,
    fixture: &Fixture,
    node: usize,
    n: usize,
    deadline: Instant,
) -> Result<(Arc<RemoteConnection>, Arc<RemoteFsDriver>)> {
    let address = fleet.address(node)?;
    let roots = fixture.roots.clone();
    let token = fixture.tokens[n].clone();
    let connection = checked(fleet, deadline, async {
        RemoteConnection::connect(
            address,
            "localhost",
            roots,
            partition(n),
            CredentialSource::File(token),
        )
        .await
        .map_err(|e| format!("signed QUIC connect: {e}"))
    })
    .await?;
    let driver = checked(fleet, deadline, async {
        RemoteFsDriver::new(connection.clone(), drive(n))
            .await
            .map(Arc::new)
            .map_err(|e| e.to_string())
    })
    .await?;
    Ok((connection, driver))
}
async fn read(
    fleet: &mut Fleet,
    fixture: &Fixture,
    node: usize,
    file: usize,
    deadline: Instant,
) -> Result<()> {
    let (connection, driver) = connect(fleet, fixture, node, 0, deadline).await?;
    let view = Loopback::from_arc(driver);
    let bytes = checked(fleet, deadline, async {
        view.read_file(&format!("/file-{file}"))
            .await
            .map_err(|e| e.to_string())
    })
    .await?;
    connection.close();
    if bytes != payload(0, file) {
        return Err("remote bytes mismatch".into());
    }
    Ok(())
}
fn launch(
    fleet: &mut Fleet,
    fixture: &Fixture,
    n: usize,
    settings: CacheSettings<'_>,
    deadline: Instant,
) -> Result<()> {
    let generation = fleet.generation();
    let config = fixture.service_config(n, generation, settings)?;
    fleet.launch(
        n,
        generation,
        &config,
        fixture.peer_addresses[n],
        settings.directory,
        deadline,
    )?;
    Ok(())
}
fn stop(fleet: &mut Fleet, node: usize, phase: Instant) -> Result<Bank> {
    let deadline = clipped(Instant::now(), phase, SHUTDOWN_SECONDS)?;
    fleet
        .stop_node(node, deadline)?
        .remove(&drive(0))
        .ok_or_else(|| "drive0 shutdown bank absent".into())
}
fn cache_path(fixture: &Fixture, name: &str) -> PathBuf {
    fixture.root.join(format!("cache-{name}"))
}
fn entry(cache: &Path, scope: &CacheScope, id: &BlockId) -> PathBuf {
    let key = LocalCache::key_hashed(&LocalCache::scope_hash(scope), id);
    let name = key.iter().map(|b| format!("{b:02x}")).collect::<String>();
    cache.join(name)
}
fn complete_entry(path: &Path, body: &[u8]) -> Result<bool> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    Ok(bytes.len() == DISK_ENTRY_BYTES && &bytes[32..] == body)
}
async fn observe_entry(fleet: &mut Fleet, path: &Path, bytes: &[u8], phase: Instant) -> Result<()> {
    loop {
        if Instant::now() >= phase {
            return Err("owned atomic cache admission deadline exhausted".into());
        }
        fleet.check()?;
        if complete_entry(path, bytes)? {
            if Instant::now() >= phase {
                return Err("late cache admission observation".into());
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
}
async fn fresh_oracle(
    fleet: &mut Fleet,
    fixture: &Fixture,
    phase: Instant,
) -> Result<(Vec<CacheScope>, Vec<Vec<BlockId>>, Value)> {
    let mut scopes = Vec::new();
    let mut ids = Vec::new();
    let mut receipts = Vec::new();
    for n in 0..NODES {
        let metadata = SqliteMetadataStore::open(fixture.root.join(format!("metadata-{n}.sqlite")))
            .map_err(|e| e.to_string())?;
        let mode = checked(fleet, phase, async {
            metadata
                .compact_inode_mode_state()
                .await
                .map_err(|e| e.to_string())
        })
        .await?
        .ok_or("fresh metadata has no compact mode")?;
        let snapshot = checked(fleet, phase, async {
            metadata
                .load_compact_snapshot(mode.backing)
                .await
                .map_err(|e| e.to_string())
        })
        .await?;
        let namespace = snapshot.namespace().map_err(|e| e.to_string())?;
        if namespace.nodes.len() != FILES + 1 || snapshot.anchor.members.len() != FILES + 1 {
            return Err("fresh namespace membership mismatch".into());
        }
        let root = namespace
            .nodes
            .get(&namespace.root)
            .ok_or("namespace root missing")?;
        if !root.stats.is_directory() {
            return Err("fresh namespace root is not a directory".into());
        }
        let NodeData::Directory { entries } = &root.data else {
            return Err("root data is not directory".into());
        };
        if entries.len() != FILES {
            return Err("fresh namespace entry count mismatch".into());
        }
        let mode_text: String =
            rusqlite::Connection::open(fixture.root.join(format!("metadata-{n}.sqlite")))
                .map_err(|e| e.to_string())?
                .query_row(
                    "SELECT write_mode FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
        if mode_text != "MRC5" {
            return Err(format!("unexpected physical write mode: {mode_text}"));
        }
        let blocks = SqliteBlockStore::open(fixture.root.join(format!("blocks-{n}.sqlite")))
            .map_err(|e| e.to_string())?;
        checked(fleet, phase, async {
            blocks
                .verify_concurrent_backing(mode.backing)
                .await
                .map_err(|e| e.to_string())
        })
        .await?;
        let mut file_ids = Vec::new();
        let mut digests = Vec::new();
        let connection =
            rusqlite::Connection::open(fixture.root.join(format!("blocks-{n}.sqlite")))
                .map_err(|e| e.to_string())?;
        for file in 0..FILES {
            let name = format!("file-{file}");
            let item = entries
                .iter()
                .find(|e| e.name == name)
                .ok_or("fresh expected file missing")?;
            let node = namespace
                .nodes
                .get(&item.inode)
                .ok_or("file guard missing")?;
            let NodeData::File(layout) = &node.data else {
                return Err("fresh expected file is not a file".into());
            };
            if !node.stats.is_file()
                || node.stats.size != BLOCK_BYTES as u64
                || layout.extents.len() != 1
            {
                return Err("fresh file type/length/extent mismatch".into());
            }
            let extent = &layout.extents[0];
            if extent.file_offset != 0
                || extent.block_offset != 0
                || extent.length != BLOCK_BYTES as u64
            {
                return Err("fresh extent bounds mismatch".into());
            }
            let bytes: Vec<u8> = connection
                .query_row(
                    "SELECT bytes FROM mount_rs_blocks WHERE id=?1",
                    [&extent.block.0],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            if bytes != payload(n, file) {
                return Err("fresh undecorated raw SQLite byte oracle mismatch".into());
            }
            file_ids.push(extent.block.clone());
            digests.push(sha256(&bytes));
        }
        // Reopen the public SDK without a decorator as a separate logical read path.
        let filesystem = checked(fleet, phase, async {
            Filesystem::split(split(fixture, n, &format!("fresh-oracle-{n}")))
                .await
                .map_err(|e| e.to_string())
        })
        .await?;
        let view = Loopback::from_arc(filesystem.driver());
        for file in 0..FILES {
            let bytes = checked(fleet, phase, async {
                view.read_file(&format!("/file-{file}"))
                    .await
                    .map_err(|e| e.to_string())
            })
            .await?;
            if bytes != payload(n, file) {
                return Err("fresh undecorated SDK bytes mismatch".into());
            }
        }
        checked(fleet, phase, async {
            filesystem.shutdown().await.map_err(|e| e.to_string())
        })
        .await?;
        receipts.push(
            json!({"partition":partition(n),"drive":drive(n),"write_mode":mode_text,
            "backing":mode.backing.to_hex(),"anchor_generation":snapshot.anchor.generation,
            "members":snapshot.anchor.members.len(),"nodes":namespace.nodes.len(),
            "files":FILES,"bytes_each":BLOCK_BYTES,"byte_sha256":digests,
            "oracle":"fresh SQLite typed metadata + raw stored blob bytes + undecorated SDK"}),
        );
        scopes.push(CacheScope {
            identity: ScopeIdentity {
                cluster: CLUSTER.into(),
                partition: partition(n),
                drive: drive(n),
            },
            backing: mode.backing,
        });
        ids.push(file_ids);
    }
    if Instant::now() >= phase {
        return Err("fresh oracle completed after deadline".into());
    }
    Ok((scopes, ids, json!(receipts)))
}

async fn security(
    fleet: &mut Fleet,
    fixture: &mut Fixture,
    phase: Instant,
    phases: &mut Vec<Value>,
) -> Result<()> {
    let node = 0;
    let cache = cache_path(fixture, "security");
    launch(
        fleet,
        fixture,
        node,
        CacheSettings {
            mode: "deterministic",
            ram_bytes: RAM_BYTES,
            disk_bytes: DISK_BYTES,
            peers: &[],
            directory: &cache,
        },
        phase,
    )?;
    let address = fleet.address(node)?;
    let rejected = checked(fleet, phase, async {
        Ok(RemoteConnection::connect(
            address,
            "localhost",
            fixture.roots.clone(),
            partition(0),
            CredentialSource::File(fixture.bad_signature.clone()),
        )
        .await)
    })
    .await?;
    if !matches!(rejected, Err(ClientError::Authentication)) {
        return Err("invalid signature did not produce authentication rejection".into());
    }
    let cross_partition = checked(fleet, phase, async {
        Ok(RemoteConnection::connect(
            address,
            "localhost",
            fixture.roots.clone(),
            partition(2),
            CredentialSource::File(fixture.tokens[0].clone()),
        )
        .await)
    })
    .await?;
    if !matches!(cross_partition, Err(ClientError::Authentication)) {
        return Err("cross-partition did not produce authentication rejection".into());
    }
    let (connection, driver) = connect(fleet, fixture, node, 0, phase).await?;
    let view = Loopback::from_arc(driver.clone());
    let cached = checked(fleet, phase, async {
        view.read_file("/file-0").await.map_err(|e| e.to_string())
    })
    .await?;
    if cached != payload(0, 0) {
        return Err("security prewarm mismatch".into());
    }
    for forbidden in [drive(1), format!("{}/{}", partition(2), drive(2))] {
        let denied = checked(fleet, phase, async {
            Ok(connection
                .request(
                    &forbidden,
                    Operation {
                        name: OperationName::Stat,
                        body: json!({"path":"/"}),
                    },
                )
                .await)
        })
        .await?;
        if !matches!(denied, Err(ClientError::Remote(code)) if code == "EACCES") {
            return Err(
                "cached sibling/cross-partition drive request did not return EACCES".into(),
            );
        }
    }
    fixture.document["grants"]["workload-0"]["drives"][drive(0)] = json!("read");
    let config = fixture.apply_config(1)?;
    fleet.apply(&config, clipped(Instant::now(), phase, REQUEST_SECONDS)?)?;
    let denied = checked(fleet, phase, async {
        Ok(driver.write_file("/file-0", b"forbidden").await)
    })
    .await?;
    if !matches!(denied, Err(error) if error.code == ErrorCode::Eacces) {
        return Err("live read-only grant did not deny write with EACCES".into());
    }
    let allowed = checked(fleet, phase, async {
        view.read_file("/file-0").await.map_err(|e| e.to_string())
    })
    .await?;
    if allowed != payload(0, 0) {
        return Err("read-only granted read mismatch".into());
    }
    fixture.document["grants"]
        .as_object_mut()
        .ok_or("grants object missing")?
        .remove("workload-0");
    let config = fixture.apply_config(2)?;
    fleet.apply(&config, clipped(Instant::now(), phase, REQUEST_SECONDS)?)?;
    let denied = checked(fleet, phase, async { Ok(view.read_file("/file-0").await) }).await?;
    if !matches!(denied, Err(error) if error.code == ErrorCode::Eacces) {
        return Err("existing cached route did not deny next revoked operation with EACCES".into());
    }
    connection.close();
    stop(fleet, node, phase)?;
    phases.push(json!({"phase":"signed-auth-route-read-only-revocation","invalid_signature":"authentication_rejected",
        "cross_partition":"authentication_rejected","cached_sibling_and_cross_drive":"EACCES",
        "read_only_write":"EACCES","revoked_next_operation":"EACCES","catalog_revisions":[1,2,3]}));
    Ok(())
}

async fn cache_cells(
    fleet: &mut Fleet,
    fixture: &Fixture,
    mode: &str,
    scope: &CacheScope,
    ids: &[BlockId],
    work: Instant,
    phases: &mut Vec<Value>,
) -> Result<()> {
    let phase = clipped(Instant::now(), work, PHASE_SECONDS)?;
    for n in 0..NODES {
        let peers = (0..NODES).filter(|p| *p != n).collect::<Vec<_>>();
        let cache = cache_path(fixture, &format!("{mode}-cold-{n}"));
        launch(
            fleet,
            fixture,
            n,
            CacheSettings {
                mode,
                ram_bytes: RAM_BYTES,
                disk_bytes: DISK_BYTES,
                peers: &peers,
                directory: &cache,
            },
            phase,
        )?;
    }
    fleet.check()?;
    let distinct = fleet
        .processes
        .iter()
        .map(|p| p.receipt.pid)
        .collect::<BTreeSet<_>>();
    if distinct.len() != NODES {
        return Err("ten distinct simultaneous CLI PIDs not observed".into());
    }
    phases.push(
        json!({"phase":"ten-live","mode":mode,"pids":distinct,"partitions":PARTITIONS,
        "drives":NODES,"files_each":FILES,"bytes_each":BLOCK_BYTES,"capacity_claim":false}),
    );
    let discovery = FixedDiscovery::new_with_mode(
        (0..NODES).map(|n| PeerId(format!("node-{n}"))).collect(),
        3,
        if mode == "peer-query" {
            DiscoveryMode::PeerQuery
        } else {
            DiscoveryMode::Deterministic
        },
    )
    .map_err(|e| e.to_string())?;
    let owners = discovery.placement(scope, &ids[0]);
    let owner_indices = owners
        .iter()
        .map(|p| {
            p.0.strip_prefix("node-")
                .ok_or("placement ID invalid")?
                .parse::<usize>()
                .map_err(|_| "placement index invalid".to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    let source = (0..NODES)
        .find(|n| !owner_indices.contains(n))
        .ok_or("source selection failed")?;
    let requester = (0..NODES)
        .find(|n| *n != source && !owner_indices.contains(n))
        .ok_or("cold requester selection failed")?;
    read(fleet, fixture, source, 0, phase).await?;
    let holder = owner_indices[0];
    let holder_cache = fleet.cache(holder)?;
    let holder_entry = entry(&holder_cache, scope, &ids[0]);
    observe_entry(fleet, &holder_entry, &payload(0, 0), phase).await?;
    read(fleet, fixture, requester, 0, phase).await?;
    let bank = stop(fleet, requester, phase)?;
    bank.expect(0, 1, 0, BLOCK_BYTES as u64)?;
    phases.push(
        json!({"phase":"cold-peer","mode":mode,"source":source,"requester":requester,
        "ranked_holders":owner_indices,"observed_complete_holder_entry":holder_entry,
        "requester_private_empty_cache":true,"backing":scope.backing.to_hex(),"bank":bank,
        "savings_scope":"one logical backing fetch avoided for the requester generation"}),
    );

    // RAM-only and disk-only are explicit isolated counterfactual generations: no eligible peers.
    let phase = clipped(Instant::now(), work, PHASE_SECONDS)?;
    let ram_cache = cache_path(fixture, &format!("{mode}-ram-only"));
    launch(
        fleet,
        fixture,
        requester,
        CacheSettings {
            mode,
            ram_bytes: RAM_BYTES,
            disk_bytes: 0,
            peers: &[],
            directory: &ram_cache,
        },
        phase,
    )?;
    for _ in 0..21 {
        read(fleet, fixture, requester, 0, phase).await?;
    }
    let bank = stop(fleet, requester, phase)?;
    bank.expect(20, 0, 1, 20 * BLOCK_BYTES as u64)?;
    phases.push(
        json!({"phase":"ram-only","mode":mode,"resident_bytes":BLOCK_BYTES,
        "node_wide_ram_bytes":RAM_BYTES,"incoming_eligible_peers":0,"bank":bank}),
    );

    let disk_cache = cache_path(fixture, &format!("{mode}-disk-only"));
    launch(
        fleet,
        fixture,
        requester,
        CacheSettings {
            mode,
            ram_bytes: 0,
            disk_bytes: DISK_BYTES,
            peers: &[],
            directory: &disk_cache,
        },
        phase,
    )?;
    read(fleet, fixture, requester, 0, phase).await?;
    let disk_entry = entry(&disk_cache, scope, &ids[0]);
    observe_entry(fleet, &disk_entry, &payload(0, 0), phase).await?;
    let warm = stop(fleet, requester, phase)?;
    warm.expect(0, 0, 1, 0)?;
    launch(
        fleet,
        fixture,
        requester,
        CacheSettings {
            mode,
            ram_bytes: 0,
            disk_bytes: DISK_BYTES,
            peers: &[],
            directory: &disk_cache,
        },
        phase,
    )?;
    read(fleet, fixture, requester, 0, phase).await?;
    let disk = stop(fleet, requester, phase)?;
    disk.expect(1, 0, 0, BLOCK_BYTES as u64)?;
    if !complete_entry(&disk_entry, &payload(0, 0))? {
        return Err("disk persistence body absent after reap".into());
    }
    let mut bytes = std::fs::read(&disk_entry).map_err(|e| e.to_string())?;
    bytes[DISK_ENTRY_BYTES - 1] ^= 1;
    write(&disk_entry, bytes)?;
    launch(
        fleet,
        fixture,
        requester,
        CacheSettings {
            mode,
            ram_bytes: 0,
            disk_bytes: DISK_BYTES,
            peers: &[],
            directory: &disk_cache,
        },
        phase,
    )?;
    read(fleet, fixture, requester, 0, phase).await?;
    let corrupt = stop(fleet, requester, phase)?;
    corrupt.expect(0, 0, 1, 0)?;
    phases.push(json!({"phase":"disk-restart-corruption","mode":mode,"same_disk_path":disk_cache,
        "ram_bytes":0,"eligible_peers":0,"warm":warm,"restart":disk,"corrupt_fallback":corrupt,
        "corruption":"owned payload byte changed with persisted checksum unchanged after actual reap"}));

    let phase = clipped(Instant::now(), work, PHASE_SECONDS)?;
    let evict_cache = cache_path(fixture, &format!("{mode}-eviction"));
    launch(
        fleet,
        fixture,
        requester,
        CacheSettings {
            mode,
            ram_bytes: 0,
            disk_bytes: DISK_ENTRY_BYTES,
            peers: &[],
            directory: &evict_cache,
        },
        phase,
    )?;
    read(fleet, fixture, requester, 0, phase).await?;
    let old = entry(&evict_cache, scope, &ids[0]);
    let replacement = entry(&evict_cache, scope, &ids[1]);
    observe_entry(fleet, &old, &payload(0, 0), phase).await?;
    let admitted = complete_entry(&old, &payload(0, 0))?;
    read(fleet, fixture, requester, 1, phase).await?;
    observe_entry(fleet, &replacement, &payload(0, 1), phase).await?;
    stop(fleet, requester, phase)?;
    eviction_oracle(
        admitted,
        old.exists(),
        complete_entry(&replacement, &payload(0, 1))?,
    )?;
    launch(
        fleet,
        fixture,
        requester,
        CacheSettings {
            mode,
            ram_bytes: 0,
            disk_bytes: DISK_ENTRY_BYTES,
            peers: &[],
            directory: &evict_cache,
        },
        phase,
    )?;
    read(fleet, fixture, requester, 0, phase).await?;
    let bank = stop(fleet, requester, phase)?;
    bank.expect(0, 0, 1, 0)?;
    phases.push(
        json!({"phase":"disk-eviction","mode":mode,"disk_budget_bytes":DISK_ENTRY_BYTES,
        "old_complete_admission_observed":admitted,"old_removed_after_reap":true,
        "replacement_complete_after_reap":true,"fresh_old_read_bank":bank}),
    );

    // Only one attested holder is eligible in this requester configuration.
    let phase = clipped(Instant::now(), work, PHASE_SECONDS)?;
    stop(fleet, holder, phase)?;
    launch(
        fleet,
        fixture,
        holder,
        CacheSettings {
            mode,
            ram_bytes: 0,
            disk_bytes: DISK_BYTES,
            peers: &(0..NODES).filter(|p| *p != holder).collect::<Vec<_>>(),
            directory: &holder_cache,
        },
        phase,
    )?;
    if !complete_entry(&holder_entry, &payload(0, 0))? {
        return Err("restarted holder disk entry absent".into());
    }
    let peer_cache = cache_path(fixture, &format!("{mode}-one-restarted-peer"));
    launch(
        fleet,
        fixture,
        requester,
        CacheSettings {
            mode,
            ram_bytes: RAM_BYTES,
            disk_bytes: DISK_BYTES,
            peers: &[holder],
            directory: &peer_cache,
        },
        phase,
    )?;
    read(fleet, fixture, requester, 0, phase).await?;
    let peer = stop(fleet, requester, phase)?;
    peer.expect(0, 1, 0, BLOCK_BYTES as u64)?;
    stop(fleet, holder, phase)?;
    let outage_cache = cache_path(fixture, &format!("{mode}-peer-outage-empty"));
    launch(
        fleet,
        fixture,
        requester,
        CacheSettings {
            mode,
            ram_bytes: RAM_BYTES,
            disk_bytes: DISK_BYTES,
            peers: &[holder],
            directory: &outage_cache,
        },
        phase,
    )?;
    read(fleet, fixture, requester, 0, phase).await?;
    let outage = stop(fleet, requester, phase)?;
    outage.expect(0, 0, 1, 0)?;
    phases.push(
        json!({"phase":"isolated-holder-restart-outage","mode":mode,"only_eligible_peer":holder,
        "holder_same_socket":fixture.peer_addresses[holder],"holder_same_disk_path":holder_cache,
        "holder_ram_bytes":0,"restart_bank":peer,"outage_bank":outage,
        "outage_requester_private_empty_cache":true}),
    );
    fleet.stop_all(clipped(Instant::now(), work, SHUTDOWN_SECONDS)?)?;
    Ok(())
}

async fn run(
    fleet: &mut Fleet,
    fixture: &mut Fixture,
    phases: &mut Vec<Value>,
    setup: Instant,
) -> Result<()> {
    if Instant::now() >= setup {
        return Err("fixture setup exhausted singleton budget".into());
    }
    let config = fixture.apply_config(0)?;
    fleet.apply(&config, clipped(Instant::now(), setup, REQUEST_SECONDS)?)?;
    // Initialize each compact backing sequentially before ten independent process opens.
    for n in 0..NODES {
        let filesystem = checked(fleet, setup, async {
            Filesystem::split(split(fixture, n, &format!("initialize-{n}")))
                .await
                .map_err(|e| e.to_string())
        })
        .await?;
        checked(fleet, setup, async {
            filesystem.shutdown().await.map_err(|e| e.to_string())
        })
        .await?;
    }
    fixture.release_reservations();
    for n in 0..NODES {
        let peers = (0..NODES).filter(|p| *p != n).collect::<Vec<_>>();
        launch(
            fleet,
            fixture,
            n,
            CacheSettings {
                mode: "deterministic",
                ram_bytes: RAM_BYTES,
                disk_bytes: DISK_BYTES,
                peers: &peers,
                directory: &cache_path(fixture, &format!("seed-{n}")),
            },
            setup,
        )?;
    }
    let pids = fleet
        .processes
        .iter()
        .map(|p| p.receipt.pid)
        .collect::<BTreeSet<_>>();
    if pids.len() != NODES {
        return Err("seed did not observe ten simultaneous CLI PIDs".into());
    }
    for n in 0..NODES {
        let (connection, driver) = connect(fleet, fixture, n, n, setup).await?;
        for file in 0..FILES {
            checked(fleet, setup, async {
                driver
                    .write_file(&format!("/file-{file}"), &payload(n, file))
                    .await
                    .map_err(|e| e.to_string())
            })
            .await?;
        }
        checked(fleet, setup, async {
            driver.syncfs().await.map_err(|e| e.to_string())
        })
        .await?;
        connection.close();
    }
    fleet.stop_all(clipped(Instant::now(), setup, SHUTDOWN_SECONDS)?)?;
    let (scopes, ids, oracle) = fresh_oracle(fleet, fixture, setup).await?;
    phases.push(json!({"phase":"signed-seed-and-fresh-durable-oracle","pids":pids,"oracle":oracle,
        "remote_client_transport":"QUIC","write_ack_scope":"signed RPC completion plus fresh SQLite reopen",
        "lost_reply_oracle":"unavailable; no real relay in this qualification"}));
    let work = Instant::now() + Duration::from_secs(WORK_SECONDS);
    for mode in ["deterministic", "peer-query"] {
        cache_cells(fleet, fixture, mode, &scopes[0], &ids[0], work, phases).await?;
    }
    security(
        fleet,
        fixture,
        clipped(Instant::now(), work, PHASE_SECONDS)?,
        phases,
    )
    .await?;
    let (final_scopes, final_ids, oracle) = fresh_oracle(
        fleet,
        fixture,
        clipped(Instant::now(), work, PHASE_SECONDS)?,
    )
    .await?;
    if final_scopes != scopes || final_ids != ids {
        return Err(
            "fresh backing scope/block identity changed during read/fault/denial cells".into(),
        );
    }
    phases.push(
        json!({"phase":"final-fresh-oracle","oracle":oracle,"same_backings_and_blocks":true}),
    );
    Ok(())
}

fn credential_cleanup(
    root: &Path,
    inventory: &[PathBuf],
    ownership_complete: bool,
    deadline: Instant,
) -> (bool, Value) {
    let mut rows = Vec::new();
    let mut complete = ownership_complete;
    for path in inventory {
        if path.parent() != Some(root) {
            complete = false;
            rows.push(json!({"file":path.file_name().and_then(|name| name.to_str()),"status":"ownership_mismatch_no_export"}));
            continue;
        }
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                rows.push(json!({"file":path.file_name().and_then(|name| name.to_str()),"status":"absent"}));
                continue;
            }
            Err(error) => {
                complete = false;
                rows.push(json!({"file":path.file_name().and_then(|name| name.to_str()),"status":"pending_no_export","error":error.to_string()}));
                continue;
            }
        };
        if !metadata.file_type().is_file() || metadata.nlink() != 1 || metadata.len() > FILE_CAP {
            complete = false;
            rows.push(json!({"file":path.file_name().and_then(|name| name.to_str()),"status":"unproven_file_ownership_no_export"}));
            continue;
        }
        let digest = std::fs::read(path).map(|bytes| sha256(&bytes));
        if !ownership_complete || Instant::now() >= deadline {
            complete = false;
            rows.push(json!({"file":path.file_name().and_then(|name| name.to_str()),"sha256":digest.ok(),"status":"pending_no_export"}));
            continue;
        }
        match digest.and_then(|hash| std::fs::remove_file(path).map(|_| hash)) {
            Ok(hash) => {
                let absent = matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
                if !absent {
                    complete = false;
                }
                rows.push(json!({"file":path.file_name().and_then(|name| name.to_str()),"sha256":hash,
                    "status":if absent {"removed_after_reap_and_oracles"} else {"removal_unproven_no_export"}}));
            }
            Err(error) => {
                complete = false;
                rows.push(json!({"file":path.file_name().and_then(|name| name.to_str()),"status":"cleanup_failed_no_export","error":error.to_string()}));
            }
        }
    }
    if Instant::now() >= deadline {
        complete = false;
    }
    (
        complete,
        json!({"complete":complete,"declared_inventory_count":inventory.len(),"files":rows,
        "rule":"never export private keys/bearer tokens; uncertain ownership keeps private files with pending cleanup"}),
    )
}

pub fn worker() {
    let root = PathBuf::from(
        std::env::var_os("MOUNT_RS_TEN_PROCESS_ROOT").expect("private supervisor worker entry"),
    );
    let supervisor: u32 = std::env::var("MOUNT_RS_TEN_PROCESS_SUPERVISOR")
        .expect("supervisor identity")
        .parse()
        .expect("supervisor PID");
    assert_eq!(
        unsafe { libc::getppid() } as u32,
        supervisor,
        "worker must be direct owned child"
    );
    assert_eq!(
        unsafe { libc::getpgrp() } as u32,
        std::process::id(),
        "worker must lead its owned process group"
    );
    let setup_ns = std::env::var("MOUNT_RS_TEN_PROCESS_SETUP_NS")
        .expect("shared setup clock")
        .parse()
        .expect("setup monotonic timestamp");
    let outer_ns = std::env::var("MOUNT_RS_TEN_PROCESS_OUTER_NS")
        .expect("shared outer clock")
        .parse()
        .expect("outer monotonic timestamp");
    let setup = native_deadline(setup_ns).expect("remaining shared setup budget");
    let mut fleet = Fleet::new(&root, supervisor, outer_ns)
        .expect("initial sampled empty-child resource receipt");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("worker runtime");
    let mut phases = Vec::new();
    let credentials = Fixture::credential_inventory(&root);
    let mut result = Fixture::new(&root).and_then(|mut fixture| {
        runtime.block_on(run(&mut fleet, &mut fixture, &mut phases, setup))
    });
    let cleanup_deadline = fleet.cleanup_deadline().unwrap_or_else(|_| Instant::now());
    let cleanup = fleet.stop_all(cleanup_deadline);
    if let Err(error) = cleanup {
        result = Err(format!(
            "{}; cleanup: {error}",
            result.err().unwrap_or_default()
        ));
    }
    let audit = Instant::now() + Duration::from_secs(AUDIT_SECONDS);
    let processes = fleet.retired.clone();
    let owned_cleanup_closed = fleet.owned_cleanup_closed();
    let unreaped = fleet
        .processes
        .iter()
        .map(|p| p.receipt.clone())
        .collect::<Vec<_>>();
    let ownership_and_oracles_complete = result.is_ok()
        && unreaped.is_empty()
        && processes
            .iter()
            .all(|p| p.qualify(&p.node, p.generation, p.pid).is_ok())
        && free_disk(&root).unwrap_or(0) >= FREE_DISK_FLOOR;
    let (credentials_complete, credentials_receipt) =
        credential_cleanup(&root, &credentials, ownership_and_oracles_complete, audit);
    let complete = ownership_and_oracles_complete && credentials_complete;
    let receipt = json!({"schema":1,"complete":complete,"owned_cleanup_closed":owned_cleanup_closed,"failure":result.as_ref().err(),
        "scope":"ten public CLI debug PIDs, SQLite MRC5, five partitions, ten drives, sixteen 4KiB files per drive",
        "debug_assertions":cfg!(debug_assertions),"local_oidc_fixture":cfg!(feature="local-oidc-fixture"),
        "worker_pid":std::process::id(),"supervisor_pid":supervisor,"process_group":unsafe { libc::getpgrp() },
        "limits":{"setup_seconds":SETUP_SECONDS,"work_seconds":WORK_SECONDS,"phase_seconds":PHASE_SECONDS,
            "request_seconds":REQUEST_SECONDS,"shutdown_seconds":SHUTDOWN_SECONDS,"last_force_seconds":FORCE_SECONDS,
            "audit_seconds":AUDIT_SECONDS,"rss_bytes_each":RSS_CAP,"rss_bytes_owned_aggregate":RSS_CAP,"free_disk_floor":FREE_DISK_FLOOR,
            "cooperative_poll_ms":POLL_MS,"output_file_cap":FILE_CAP,"output_line_cap":LINE_CAP,"output_total_cap":TOTAL_CAP,
            "ram_bytes_node_wide":RAM_BYTES,"disk_bytes_default":DISK_BYTES,
            "max_blob_bytes":64*1024,"max_inflight":128,"peer_transfer_bytes":8*1024*1024},
        "processes":processes,"unreaped_owned":unreaped,"generation_logical_banks":fleet.banks,"phases":phases,
        "owned_resource_evidence":fleet.resource_evidence(),
        "private_credential_cleanup":credentials_receipt,
        "unavailable":{"maintenance_quiescence":"CLI shutdown does not prove worker drain",
            "exact_phase_counters":"only cumulative generation shutdown banks are exported",
            "raw_adapter_calls":"not exported by this SQLite CLI seam",
            "backing_http_attempts_and_returned_bytes":"no HTTP fixture in this cell",
            "physical_device_io":"not measured","quic_peer_bytes":"not exported by CLI",
            "cpu_allocations":"not measured","tiDB_rustFS_redis":"separate lineage unqualified",
            "enospc_and_real_lost_reply":"not exercised"}});
    write(
        &root.join("receipt.json"),
        serde_json::to_vec_pretty(&receipt).expect("receipt serialize"),
    )
    .expect("retained worker receipt");
    assert!(
        complete && Instant::now() < audit,
        "qualification incomplete: {:?}",
        result.err()
    );
}
