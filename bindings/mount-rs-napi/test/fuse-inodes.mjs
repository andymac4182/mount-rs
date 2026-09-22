import assert from "node:assert/strict"
import { pathToFileURL } from "node:url"
import { InodeTable } from "../fuse.cjs"

const source = process.env.MOUNTX_SOURCE
assert.ok(source, "MOUNTX_SOURCE is required; inode parity never silently skips")
assert.equal(
  (await import(pathToFileURL(`${source}/src/fuse/inodes.ts`).href)).INODE_GENERATION,
  0n,
)
const { InodeTable: OracleInodeTable } = await import(
  pathToFileURL(`${source}/src/fuse/inodes.ts`).href,
)

const stat = (dev, ino) => ({
  dev,
  ino,
  mode: 0o100644,
  nlink: 1,
  uid: 0,
  gid: 0,
  rdev: 0,
  size: 0,
  blksize: 4096,
  blocks: 0,
  atimeMs: 0,
  mtimeMs: 0,
  ctimeMs: 0,
  birthtimeMs: 0,
  isFile: () => true,
  isDirectory: () => false,
  isSymbolicLink: () => false,
  isBlockDevice: () => false,
  isCharacterDevice: () => false,
  isFIFO: () => false,
  isSocket: () => false,
})

const view = (inode) => inode == null ? undefined : {
  nodeid: inode.nodeid,
  key: inode.key,
  nlookup: inode.nlookup,
  paths: [...inode.paths].sort(),
}

function scenario(Table) {
  const table = new Table()
  const result = {
    root: view(table.root),
    hardlink: {},
    orphan: {},
    rename: {},
  }

  const first = table.bind("/a", stat(7, 11))
  table.acquire(first)
  const second = table.bind("/b", stat(7, 11))
  result.hardlink = { sameNode: first.nodeid === second.nodeid, paths: view(table.get(first.nodeid)) }

  table.unbind("/a")
  result.orphan.afterUnbind = view(table.get(first.nodeid))
  result.orphan.path = (() => {
    try { return table.pathOf(first) } catch (error) { return { code: error.code } }
  })()
  table.unbind("/b")
  result.orphan.afterAllUnbound = view(table.get(first.nodeid))
  result.orphan.forget = table.forget(first.nodeid, 1n)
  result.orphan.afterForget = view(table.get(first.nodeid))

  const child = table.bind("/old/deep/file", stat(9, 21))
  const replaced = table.bind("/new/deep/file", stat(9, 22))
  table.remap("/old", "/new")
  result.rename = {
    child: view(table.get(child.nodeid)),
    replaced: view(table.get(replaced.nodeid)),
    oldPath: view(table.at("/old/deep/file")),
    newPath: view(table.at("/new/deep/file")),
  }

  return result
}

const native = scenario(InodeTable)
const oracle = scenario(OracleInodeTable)
assert.deepEqual(native, oracle, "Rust-backed inode table matches the TypeScript oracle")

const noIdentity = new InodeTable({ useDriverIno: false })
assert.equal(typeof noIdentity.nodeids, "function")
assert.ok(noIdentity.root.paths instanceof Set)
const first = noIdentity.bind("/first", stat(1, 99))
const second = noIdentity.bind("/second", stat(1, 99))
assert.notEqual(first.nodeid, second.nodeid, "useDriverIno=false disables hardlink identity reuse")
assert.deepEqual(noIdentity.root.paths, new Set(["/"]))
assert.deepEqual(noIdentity.nodeids().sort(), [1n, first.nodeid, second.nodeid].sort())

console.log("mount-rs N-API Rust-backed FUSE inode table: PASS")
