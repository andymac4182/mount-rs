# Archil CLI static internals review

Reviewed 2026-09-24. Downloaded the official Linux x86-64 CLI selected by the installer latest pointer: `0.8.40-1790124124`. The binary was not executed or installed.

## Artifact identity and method

Source: https://s3.amazonaws.com/archil-client/pkg/archil-linux-musl-amd64-0.8.40-1790124124

SHA-256: `1c45fc9e4d8b166ee3add68a12450133e7abf39babea97224a67adee67d41aaa`

ELF x86-64 PIE, unstripped, with DWARF debug sections. Rust symbols identify `archil_client`, `archil::fuse`, `fuser`, Tokio, and `regatta_common` transaction dependencies. Static review used LLVM objdump for symbols and disassembly and Ghidra 11.0 for selected pseudocode. Ghidra rebases this ELF at 0x100000: exported filenames retain original ELF symbol addresses; Ghidra addresses add that base.

Artifacts live in `/Users/amcclenaghan/github/andymac4182/archil-cli-review`: binary, raw/demangled symbols, strings, selected assembly, `targets.tsv`, scripts, and 25 pseudocode files in `decompiled/`.

## Findings and evidence strength

1. **The Linux mount contains the filesystem client locally.** Symbols implement `ArchilFUSEAdapter` methods, including read/write, fsync/fsyncdir, getlk/setlk. This supports ordinary local applications using an OS mount. The remote SQLite SDK execution path is a separate interface.
2. **Fsync enters the client transaction scheduler.** In assembly `disassembly/0000000000626b30.asm`, the fsync coroutine calls `TransactionScheduler::sync` at ELF 0x5fcd10. The scheduler has sync, sync_internal and sync_inode_if_pending paths. This confirms the client flush path; it does not independently prove server replication or power-loss durability.
3. **Checkout includes a cache-coherence mechanism.** `mirror_checkout_invalidation` includes a kernel-notification path. Embedded diagnostics distinguish inode, entry and attribute invalidation and report kernel invalidation failures. Delegation code contains a conditional-operation drain barrier and a diagnostic for failing to sync conditional transactions before checkout. Exact state-machine ordering still needs deeper analysis: optimized async jump tables were not fully reconstructed by the focused decompiler run.
4. **POSIX file locks are represented in the local client.** `FileLockManager::new`, `set_file_lock`, `get_conflicting_lock`, `get_lock_waiter`, and `unlock_all_for_owner` were decompiled. The reviewed code initializes and accesses in-memory hash structures, searches conflicting ranges and handles local notification/waiter state. These functions show no network RPC path. This corroborates the documentation's explicit restriction that flock/fcntl locks do not coordinate across clients; it is not evidence of a distributed SQLite lock service.
5. **Writes are coordinated through transactions and server protocol operations.** Symbols include conditional_write, commit_conditional, commit_conditional_batch, commit_unconditional, dependency relationships and dispatch/completion. FileDataCache has conditional commit claim/nonces and invalidation after nonce advance. These are evidence of client/server concurrency machinery, not a recovered complete wire protocol or a direct-to-S3 mount.

## Implications for mount-rs

The practical target is ordinary SQLite on a local filesystem mount whose authority is confined to one client. Across clients, safe handoff needs an ownership/delegation mechanism covering the database and its journal/WAL files, pending-write drain, durable publication, and both userspace and kernel cache invalidation before the new owner uses the files. Local POSIX locks alone do not supply this guarantee.

For mount-rs, qualify this in stages: exclusive Linux FUSE mount with multiple local SQLite processes; journal/WAL/fsync and crash tests; ownership expiry and fencing; handoff of a complete database directory; then macOS FSKit qualification. Do not advertise independent shared-mount WAL writers until distributed locking/coherence and shared-memory semantics have an explicit, tested solution. See the companion local-mount/SDK review for current mount-rs source gaps and existing qualification boundaries.

## Limits

This is selected-function static analysis, not a full source recovery. Eleven exported async coroutines stop at unrecovered jump tables; those pseudocode files explicitly carry warnings, so their control flow was assessed through assembly/symbols instead. Nonasync file-lock and cache functions produced substantial pseudocode, but optimized Rust layouts, types and variable names remain uncertain. Server internals and macOS FSKit implementation were not decompiled. No live mount, credentials, network protocol capture, or crash qualification was performed.
