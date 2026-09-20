//! Rust-backed FUSE inode-table bindings.
//!
//! The FUSE transport owns the path/nodeid invariants. This module exposes
//! that already-tested table through the `./fuse` N-API subpath without
//! reimplementing hardlink identity, orphan retention, or subtree remapping in
//! JavaScript.

use mount_rs_core::{ErrorCode, FsError, Stats};
use mount_rs_fuse::inodes::{Inode, InodeTable};
use napi::bindgen_prelude::BigInt;
use napi::{Error, Status};
use napi_derive::napi;
use std::collections::BTreeSet;

use super::to_js_error;

fn u64_from_bigint(value: &BigInt) -> u64 {
    let (negative, magnitude, _) = value.get_u128();
    let low = magnitude as u64;
    if negative {
        0_u64.wrapping_sub(low)
    } else {
        low
    }
}

fn bigint(value: u64) -> BigInt {
    BigInt::from(value)
}

fn number_u64(name: &str, value: f64) -> napi::Result<u64> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return Err(Error::new(
            Status::InvalidArg,
            format!("{name} must be a non-negative integer"),
        ));
    }
    Ok(value as u64)
}

#[napi(object)]
pub struct NativeFuseInodeTableOptions {
    pub use_driver_ino: Option<bool>,
}

#[napi(object)]
pub struct NativeFuseInodeStats {
    pub dev: f64,
    pub ino: f64,
}

#[napi(object)]
pub struct NativeFuseInode {
    pub nodeid: BigInt,
    pub key: Option<String>,
    pub nlookup: BigInt,
    pub paths: Vec<String>,
}

fn snapshot(inode: &Inode) -> NativeFuseInode {
    NativeFuseInode {
        nodeid: bigint(inode.nodeid),
        key: inode.key.map(|(dev, ino)| format!("{dev}:{ino}")),
        nlookup: bigint(inode.nlookup),
        paths: inode.paths.clone(),
    }
}

fn stats(value: NativeFuseInodeStats) -> napi::Result<Stats> {
    Ok(Stats {
        dev: number_u64("dev", value.dev)?,
        ino: number_u64("ino", value.ino)?,
        mode: 0o100644,
        nlink: 1,
        uid: 0,
        gid: 0,
        rdev: 0,
        size: 0,
        blksize: 4096,
        blocks: 0,
        atime_ms: 0,
        mtime_ms: 0,
        ctime_ms: 0,
        birthtime_ms: 0,
    })
}

#[napi]
pub struct NativeFuseInodeTable {
    inner: InodeTable,
    nodeids: BTreeSet<u64>,
}

#[napi]
impl NativeFuseInodeTable {
    #[napi(constructor)]
    pub fn new(options: Option<NativeFuseInodeTableOptions>) -> Self {
        let use_driver_ino = options
            .and_then(|options| options.use_driver_ino)
            .unwrap_or(true);
        Self {
            inner: InodeTable::new(use_driver_ino),
            nodeids: BTreeSet::from([1]),
        }
    }

    #[napi(getter)]
    pub fn root(&self) -> NativeFuseInode {
        snapshot(
            self.inner
                .get(1)
                .expect("FUSE root inode is always present"),
        )
    }

    #[napi]
    pub fn get(&self, nodeid: BigInt) -> Option<NativeFuseInode> {
        self.inner.get(u64_from_bigint(&nodeid)).map(snapshot)
    }

    #[napi]
    pub fn nodeids(&self) -> Vec<BigInt> {
        self.nodeids.iter().copied().map(bigint).collect()
    }

    #[napi]
    pub fn at(&self, path: String) -> Option<NativeFuseInode> {
        self.inner.at(&path).map(snapshot)
    }

    #[napi]
    pub fn require(&self, nodeid: BigInt) -> napi::Result<NativeFuseInode> {
        self.inner
            .get(u64_from_bigint(&nodeid))
            .map(snapshot)
            .ok_or_else(|| to_js_error(FsError::new(ErrorCode::Estale)))
    }

    #[napi(js_name = "pathOf")]
    pub fn path_of(&self, nodeid: BigInt) -> napi::Result<String> {
        self.inner
            .require_path(u64_from_bigint(&nodeid))
            .map(str::to_owned)
            .map_err(to_js_error)
    }

    #[napi(js_name = "requirePath")]
    pub fn require_path(&self, nodeid: BigInt) -> napi::Result<String> {
        self.path_of(nodeid)
    }

    #[napi]
    pub fn bind(
        &mut self,
        path: String,
        stats_value: NativeFuseInodeStats,
    ) -> napi::Result<NativeFuseInode> {
        let stats = stats(stats_value)?;
        let nodeid = self.inner.bind(&path, &stats);
        self.nodeids.insert(nodeid);
        Ok(snapshot(
            self.inner
                .get(nodeid)
                .expect("bound FUSE inode is always present"),
        ))
    }

    #[napi]
    pub fn acquire(&mut self, nodeid: BigInt) -> napi::Result<NativeFuseInode> {
        let nodeid = u64_from_bigint(&nodeid);
        self.inner.acquire(nodeid).map_err(to_js_error)?;
        self.nodeids.insert(nodeid);
        Ok(snapshot(
            self.inner
                .get(nodeid)
                .expect("acquired FUSE inode is always present"),
        ))
    }

    #[napi]
    pub fn unbind(&mut self, path: String) -> Option<NativeFuseInode> {
        let nodeid = self.inner.unbind(&path)?;
        self.inner.get(nodeid).map(snapshot)
    }

    #[napi]
    pub fn forget(&mut self, nodeid: BigInt, count: BigInt) -> bool {
        let nodeid = u64_from_bigint(&nodeid);
        let removed = self.inner.forget(nodeid, u64_from_bigint(&count));
        if removed {
            self.nodeids.remove(&nodeid);
        }
        removed
    }

    #[napi]
    pub fn remap(&mut self, from: String, to: String) {
        self.inner.remap(&from, &to);
    }
}
