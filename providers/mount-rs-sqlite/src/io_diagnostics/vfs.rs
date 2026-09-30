//! Transparent, opt-in instrumentation of the pinned native SQLite VFS.
//! Counters describe VFS invocations, not system calls or physical storage.

use super::{Timing, checked_add};
use mount_rs_core::{Result, backend_error};
use rusqlite::{Connection, ffi};
use std::{
    cell::{Cell, UnsafeCell},
    ffi::{CStr, CString, c_char, c_int, c_void},
    marker::PhantomData,
    mem::{align_of, size_of},
    ptr,
    rc::Rc,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

const ROLES: [&str; 5] = ["main_database", "main_journal", "wal", "temporary", "other"];
const OPERATIONS: [&str; 3] = ["read", "write", "sync"];
const SCOPE: &str = "process selected observed native VFS invocations including external connections and connection close; excludes observer read/write/sync and checkpoint signals, SHM, mmap and other VFS operations; not syscalls or physical device IOPS";
const BYTE_SCOPE: &str = "requested native xRead/xWrite bytes; confirmed only on SQLITE_OK; partial error bytes unavailable; xSync bytes are zero";
const CHECKPOINT_SCOPE: &str = "matched Instant wall time from CKPT_START arrival to CKPT_DONE arrival; backfill copy window only, after initial WAL sync and before final database truncate/sync; signals do not prove checkpoint success";
const LIFECYCLE_SCOPE: &str = "native file/context lifecycle including observer sidecar opens and closes; live gauges are never reset";

#[derive(Default)]
struct Operation {
    timing: Timing,
    errors: AtomicU64,
    requested: AtomicU64,
    confirmed: AtomicU64,
    short_reads: AtomicU64,
}
impl Operation {
    fn record(&self, bank: &Bank, elapsed: Option<u64>, amount: u64, rc: c_int, read: bool) {
        self.timing.record(elapsed);
        checked_add(&self.requested, &bank.overflow, amount);
        if rc == ffi::SQLITE_OK {
            checked_add(&self.confirmed, &bank.overflow, amount);
        } else {
            checked_add(&self.errors, &bank.overflow, 1);
            if read && rc == ffi::SQLITE_IOERR_SHORT_READ {
                checked_add(&self.short_reads, &bank.overflow, 1);
            }
        }
    }
    fn snapshot(&self, name: String) -> serde_json::Value {
        let mut value = self.timing.snapshot();
        value["name"] = serde_json::json!(name);
        value["errors"] = serde_json::json!(self.errors.load(Ordering::Relaxed));
        value["requested_bytes"] = serde_json::json!(self.requested.load(Ordering::Relaxed));
        value["confirmed_bytes"] = serde_json::json!(self.confirmed.load(Ordering::Relaxed));
        value["short_reads"] = serde_json::json!(self.short_reads.load(Ordering::Relaxed));
        value
    }
    fn reset(&self) {
        self.timing.reset();
        for counter in [
            &self.errors,
            &self.requested,
            &self.confirmed,
            &self.short_reads,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }
}

#[derive(Default)]
struct Bank {
    overflow: AtomicBool,
    resetting: AtomicBool,
    reset_raced: AtomicBool,
    in_flight: AtomicU64,
    live_files: AtomicU64,
    live_contexts: AtomicU64,
    registered: AtomicU64,
    open_attempts: AtomicU64,
    open_errors: AtomicU64,
    files_opened: AtomicU64,
    close_calls: AtomicU64,
    close_errors: AtomicU64,
    operations: [Operation; 15],
    starts: AtomicU64,
    dones: AtomicU64,
    unmatched_starts: AtomicU64,
    unmatched_dones: AtomicU64,
    aborted: AtomicU64,
    active_windows: AtomicU64,
    paired: Timing,
}
impl Bank {
    fn decrement(&self, gauge: &AtomicU64) {
        if gauge
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .is_err()
        {
            self.overflow.store(true, Ordering::Relaxed);
        }
    }
    fn enter(&self) {
        if self
            .in_flight
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .is_err()
        {
            self.overflow.store(true, Ordering::Relaxed);
        }
        // A reset is permitted only at an externally drained boundary. Detect
        // callers violating it instead of blocking a native storage callback.
        // The two-atomic handshake is sequentially consistent so the reset
        // and callback cannot both validate an old state on different atomics.
        if self.resetting.load(Ordering::SeqCst) {
            self.reset_raced.store(true, Ordering::Release);
        }
    }
    fn value(&self) -> serde_json::Value {
        let entries: Vec<_> = ROLES
            .iter()
            .enumerate()
            .flat_map(|(role, name)| {
                OPERATIONS.iter().enumerate().map(move |(op, operation)| {
                    self.operations[role * 3 + op].snapshot(format!("{name}.{operation}"))
                })
            })
            .collect();
        let overflow = self.overflow.load(Ordering::Relaxed)
            || self.paired.overflow.load(Ordering::Relaxed)
            || self
                .operations
                .iter()
                .any(|op| op.timing.overflow.load(Ordering::Relaxed));
        serde_json::json!({"schema":"mount-rs.sqlite-vfs.v1", "scope":SCOPE,
            "byte_scope":BYTE_SCOPE,"checkpoint_scope":CHECKPOINT_SCOPE,"lifecycle_scope":LIFECYCLE_SCOPE,
            "overflow":overflow,"in_flight":self.in_flight.load(Ordering::Acquire),
            "live_files":self.live_files.load(Ordering::Relaxed),
            "live_contexts":self.live_contexts.load(Ordering::Relaxed),
            "registered_vfs":self.registered.load(Ordering::Relaxed),
            "open_attempts":self.open_attempts.load(Ordering::Relaxed),
            "open_errors":self.open_errors.load(Ordering::Relaxed),
            "files_opened":self.files_opened.load(Ordering::Relaxed),
            "close_calls":self.close_calls.load(Ordering::Relaxed),
            "close_errors":self.close_errors.load(Ordering::Relaxed),"entries":entries,
            "checkpoint":{"starts":self.starts.load(Ordering::Relaxed),
                "dones":self.dones.load(Ordering::Relaxed),
                "unmatched_starts":self.unmatched_starts.load(Ordering::Relaxed),
                "unmatched_dones":self.unmatched_dones.load(Ordering::Relaxed),
                "aborted_windows":self.aborted.load(Ordering::Relaxed),
                "active_windows":self.active_windows.load(Ordering::Acquire),
                "paired":self.paired.snapshot()}})
    }
    fn snapshot(&self, reset: bool) -> serde_json::Value {
        if self.reset_raced.load(Ordering::Acquire) {
            return serde_json::json!({"error":"SQLite VFS counters raced with reset"});
        }
        if !reset {
            return self.value();
        }
        if self
            .resetting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return serde_json::json!({"error":"SQLite VFS reset already active"});
        }
        if self.in_flight.load(Ordering::SeqCst) != 0
            || self.active_windows.load(Ordering::Acquire) != 0
        {
            self.resetting.store(false, Ordering::SeqCst);
            return serde_json::json!({"error":"SQLite VFS reset requires drained callbacks and copy windows"});
        }
        let value = self.value();
        // A detected reset race is sticky for this process. Clearing it here
        // could erase a callback's concurrent notification and publish a
        // partially reset bank as complete. A fresh process owns a fresh bank.
        self.overflow.store(false, Ordering::Relaxed);
        for counter in [
            &self.open_attempts,
            &self.open_errors,
            &self.files_opened,
            &self.close_calls,
            &self.close_errors,
            &self.starts,
            &self.dones,
            &self.unmatched_starts,
            &self.unmatched_dones,
            &self.aborted,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
        for operation in &self.operations {
            operation.reset();
        }
        self.paired.reset();
        // These are live gauges, including registrations/files surviving this
        // epoch. Never reset them along with cumulative counters.
        self.resetting.store(false, Ordering::SeqCst);
        // A callback that saw resetting=true may not yet have published its
        // race notification. If it has departed, its release decrement makes
        // the notification visible; if still active, fail closed here.
        if self.in_flight.load(Ordering::SeqCst) != 0 {
            self.reset_raced.store(true, Ordering::Release);
        }
        if self.reset_raced.load(Ordering::Acquire) {
            serde_json::json!({"error":"SQLite VFS counters raced with reset"})
        } else {
            value
        }
    }
}
static BANK: OnceLock<Bank> = OnceLock::new();
fn bank() -> &'static Bank {
    BANK.get_or_init(Bank::default)
}
pub(super) fn snapshot(reset: bool) -> serde_json::Value {
    bank().snapshot(reset)
}

thread_local! {
    static OBSERVER: Cell<bool> = const { Cell::new(false) };
}
pub(super) struct ObserverScope {
    previous: Option<bool>,
    // A TLS scope must be restored on its creating thread. No Rc is allocated.
    _thread: PhantomData<Rc<()>>,
}
impl ObserverScope {
    pub(super) fn new() -> Self {
        Self {
            previous: OBSERVER.try_with(|value| value.replace(true)).ok(),
            _thread: PhantomData,
        }
    }
}
impl Drop for ObserverScope {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            let _ = OBSERVER.try_with(|value| value.set(previous));
        }
    }
}

struct Invocation {
    observed: bool,
    started: Option<Instant>,
}
impl Invocation {
    fn new(timed: bool) -> Self {
        bank().enter();
        // These fixed observer queries execute callbacks synchronously on the
        // caller. Arbitrary SQLite queries can also use sorter worker threads.
        // Thread teardown must never turn a native callback into a Rust panic.
        let observed = !OBSERVER.try_with(Cell::get).unwrap_or(false);
        Self {
            observed,
            started: (observed && timed).then(Instant::now),
        }
    }
    fn record(&self, role: usize, operation: usize, amount: c_int, rc: c_int) {
        if let Some(started) = self.started {
            bank().operations[role * 3 + operation].record(
                bank(),
                elapsed(started),
                u64::try_from(amount).unwrap_or(0),
                rc,
                operation == 0,
            );
        }
    }
}
impl Drop for Invocation {
    fn drop(&mut self) {
        bank().decrement(&bank().in_flight);
    }
}
fn elapsed(started: Instant) -> Option<u64> {
    duration_ns(started.elapsed())
}
fn duration_ns(duration: std::time::Duration) -> Option<u64> {
    u64::try_from(duration.as_nanos()).ok()
}

struct Context {
    parent: *mut ffi::sqlite3_vfs,
    inner_offset: usize,
}
struct Global {
    name: CString,
    _context: Box<Context>,
    // SQLite mutates pNext under its own registry mutex. Do not expose a Rust
    // shared reference to that mutable C structure, or copy its live pNext.
    vfs: Box<UnsafeCell<ffi::sqlite3_vfs>>,
}
// The builtin native VFS and this neutral context live for the process. All
// fields except SQLite-owned pNext are immutable; access to pNext is SQLite's.
unsafe impl Send for Global {}
unsafe impl Sync for Global {}
static GLOBAL: OnceLock<std::result::Result<Global, &'static str>> = OnceLock::new();
pub(super) struct Registration {
    global: &'static Global,
}
impl Global {
    fn new() -> std::result::Result<Self, &'static str> {
        let _ = bank(); // Establish the fixed bank before any native callback.
        let rc = unsafe { ffi::sqlite3_initialize() };
        if rc != ffi::SQLITE_OK {
            return Err("SQLite VFS initialization failed");
        }
        let parent = unsafe { ffi::sqlite3_vfs_find(ptr::null()) };
        // Pinned builtin VFS objects are process-lifetime statics. Arbitrary
        // third-party registrations may be freed before our context is closed.
        if parent.is_null() || unsafe { (*parent).zName.is_null() } {
            return Err("SQLite VFS native default unavailable");
        }
        let name = unsafe { CStr::from_ptr((*parent).zName) };
        if !matches!(name.to_bytes(), b"unix" | b"win32")
            || parent != unsafe { ffi::sqlite3_vfs_find(name.as_ptr()) }
        {
            return Err("SQLite VFS diagnostics require a builtin native default");
        }
        let version = unsafe { (*parent).iVersion };
        if !(1..=3).contains(&version) {
            return Err("SQLite VFS version unsupported");
        }
        let native_size = usize::try_from(unsafe { (*parent).szOsFile })
            .map_err(|_| "SQLite VFS native file size unavailable")?;
        if native_size < size_of::<ffi::sqlite3_file>() {
            return Err("SQLite VFS native file size unavailable");
        }
        let offset = size_of::<File>()
            .checked_add(7)
            .map(|n| n & !7)
            .ok_or("SQLite VFS file size overflow")?;
        let total = offset
            .checked_add(native_size)
            .and_then(|n| c_int::try_from(n).ok())
            .ok_or("SQLite VFS file size overflow")?;
        let name = CString::new("mount-rs-observed-native-v1")
            .map_err(|_| "SQLite VFS registration name invalid")?;
        if !unsafe { ffi::sqlite3_vfs_find(name.as_ptr()) }.is_null() {
            return Err("SQLite VFS diagnostic name already registered");
        }
        let mut context = Box::new(Context {
            parent,
            inner_offset: offset,
        });
        let vfs = Box::new(UnsafeCell::new(unsafe { native_vfs(parent) }));
        unsafe {
            (*vfs.get()).szOsFile = total;
            (*vfs.get()).zName = name.as_ptr();
            (*vfs.get()).pAppData = ptr::from_mut(context.as_mut()).cast();
        }
        let rc = unsafe { ffi::sqlite3_vfs_register(vfs.get(), 0) };
        if rc != ffi::SQLITE_OK {
            return Err("SQLite VFS registration failed");
        }
        checked_add(&bank().registered, &bank().overflow, 1);
        Ok(Self {
            name,
            _context: context,
            vfs,
        })
    }
}
impl Registration {
    pub(super) fn new() -> Result<Self> {
        let global = GLOBAL
            .get_or_init(Global::new)
            .as_ref()
            .map_err(|message| backend_error(*message))?;
        if unsafe { ffi::sqlite3_vfs_find(ptr::null()) } != global._context.parent {
            return Err(backend_error("SQLite VFS native default changed"));
        }
        checked_add(&bank().live_contexts, &bank().overflow, 1);
        Ok(Self { global })
    }
    pub(super) fn name(&self) -> &str {
        // Constructor-generated ASCII; this method is outside the C callbacks.
        self.global
            .name
            .to_str()
            .expect("diagnostic VFS name is ASCII")
    }
    pub(super) fn is_selected(&self, connection: &Connection) -> bool {
        let mut actual: *mut ffi::sqlite3_vfs = ptr::null_mut();
        // SQLite handles VFS_POINTER in its core, including memory databases.
        let rc = unsafe {
            ffi::sqlite3_file_control(
                connection.handle(),
                c"main".as_ptr(),
                ffi::SQLITE_FCNTL_VFS_POINTER,
                ptr::from_mut(&mut actual).cast(),
            )
        };
        rc == ffi::SQLITE_OK && actual == self.global.vfs.get()
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        // This is a provider marker, not ownership of the permanent VFS. Safe
        // external connections (even :memory:) can outlive every marker.
        bank().decrement(&bank().live_contexts);
    }
}

#[repr(C)]
struct File {
    header: ffi::sqlite3_file,
    methods: ffi::sqlite3_io_methods,
    inner_offset: usize,
    role: usize,
    window: Option<Instant>,
}
// SQLite's pinned pager allocation rounds to 8-byte alignment. The native unix
// file uses that same ABI alignment; both wrapper and rounded inner fit it.
const _: () = assert!(align_of::<File>() <= 8);
unsafe fn inner(file: *mut ffi::sqlite3_file) -> *mut ffi::sqlite3_file {
    unsafe {
        file.cast::<u8>()
            .add((*file.cast::<File>()).inner_offset)
            .cast()
    }
}
fn role(flags: c_int) -> usize {
    if flags & ffi::SQLITE_OPEN_MAIN_DB != 0 {
        0
    } else if flags & ffi::SQLITE_OPEN_MAIN_JOURNAL != 0 {
        1
    } else if flags & ffi::SQLITE_OPEN_WAL != 0 {
        2
    } else if flags
        & (ffi::SQLITE_OPEN_TEMP_DB
            | ffi::SQLITE_OPEN_TEMP_JOURNAL
            | ffi::SQLITE_OPEN_TRANSIENT_DB
            | ffi::SQLITE_OPEN_SUBJOURNAL
            | ffi::SQLITE_OPEN_SUPER_JOURNAL)
        != 0
    {
        3
    } else {
        4
    }
}

unsafe extern "C" fn open(
    vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    output: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    unsafe {
        let context = (*vfs).pAppData.cast::<Context>();
        let _call = Invocation::new(false);
        checked_add(&bank().open_attempts, &bank().overflow, 1);
        let file = output.cast::<File>();
        ptr::write(
            file,
            File {
                header: ffi::sqlite3_file {
                    pMethods: ptr::null(),
                },
                methods: empty_methods(),
                inner_offset: (*context).inner_offset,
                role: role(flags),
                window: None,
            },
        );
        let real = inner(output);
        (*real).pMethods = ptr::null();
        let parent = (*context).parent;
        let rc = match (*parent).xOpen {
            Some(callback) => callback(parent, name, real, flags, out_flags),
            None => ffi::SQLITE_CANTOPEN,
        };
        if rc != ffi::SQLITE_OK {
            checked_add(&bank().open_errors, &bank().overflow, 1);
        }
        if !(*real).pMethods.is_null() {
            // A failed open may still require native cleanup. Publish a fully
            // initialized wrapper so SQLite can call its xClose exactly once.
            (*file).methods = native_methods((*real).pMethods);
            (*file).header.pMethods = ptr::addr_of!((*file).methods);
            checked_add(&bank().live_files, &bank().overflow, 1);
            // Includes failed-but-closeable opens and observer sidecars.
            checked_add(&bank().files_opened, &bank().overflow, 1);
        }
        rc
    }
}
unsafe extern "C" fn close(file: *mut ffi::sqlite3_file) -> c_int {
    unsafe {
        let wrapped = file.cast::<File>();
        let _call = Invocation::new(false);
        checked_add(&bank().close_calls, &bank().overflow, 1);
        if (*wrapped).window.take().is_some() {
            checked_add(&bank().aborted, &bank().overflow, 1);
            bank().decrement(&bank().active_windows);
        }
        let real = inner(file);
        let rc = match (*(*real).pMethods).xClose {
            Some(callback) => callback(real),
            None => ffi::SQLITE_IOERR_CLOSE,
        };
        if rc != ffi::SQLITE_OK {
            checked_add(&bank().close_errors, &bank().overflow, 1);
        }
        bank().decrement(&bank().live_files);
        rc
    }
}
unsafe extern "C" fn read(
    file: *mut ffi::sqlite3_file,
    buffer: *mut c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    unsafe {
        let call = Invocation::new(true);
        let real = inner(file);
        let rc = match (*(*real).pMethods).xRead {
            Some(callback) => callback(real, buffer, amount, offset),
            None => ffi::SQLITE_IOERR_READ,
        };
        call.record((*file.cast::<File>()).role, 0, amount, rc);
        rc
    }
}
unsafe extern "C" fn write(
    file: *mut ffi::sqlite3_file,
    buffer: *const c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    unsafe {
        let call = Invocation::new(true);
        let real = inner(file);
        let rc = match (*(*real).pMethods).xWrite {
            Some(callback) => callback(real, buffer, amount, offset),
            None => ffi::SQLITE_IOERR_WRITE,
        };
        call.record((*file.cast::<File>()).role, 1, amount, rc);
        rc
    }
}
unsafe extern "C" fn sync(file: *mut ffi::sqlite3_file, flags: c_int) -> c_int {
    unsafe {
        let call = Invocation::new(true);
        let real = inner(file);
        let rc = match (*(*real).pMethods).xSync {
            Some(callback) => callback(real, flags),
            None => ffi::SQLITE_IOERR_FSYNC,
        };
        call.record((*file.cast::<File>()).role, 2, 0, rc);
        rc
    }
}
unsafe extern "C" fn file_control(
    file: *mut ffi::sqlite3_file,
    op: c_int,
    arg: *mut c_void,
) -> c_int {
    unsafe {
        let wrapped = file.cast::<File>();
        let call = Invocation::new(false);
        if op == ffi::SQLITE_FCNTL_CKPT_START {
            if call.observed {
                checked_add(&bank().starts, &bank().overflow, 1);
                if (*wrapped).window.is_some() {
                    checked_add(&bank().unmatched_starts, &bank().overflow, 1);
                } else {
                    checked_add(&bank().active_windows, &bank().overflow, 1);
                }
                (*wrapped).window = Some(Instant::now());
            } else if (*wrapped).window.take().is_some() {
                checked_add(&bank().aborted, &bank().overflow, 1);
                bank().decrement(&bank().active_windows);
            }
        } else if op == ffi::SQLITE_FCNTL_CKPT_DONE {
            let started = (*wrapped).window.take();
            if call.observed {
                checked_add(&bank().dones, &bank().overflow, 1);
                if let Some(started) = started {
                    bank().paired.record(elapsed(started));
                } else {
                    checked_add(&bank().unmatched_dones, &bank().overflow, 1);
                }
            } else if started.is_some() {
                checked_add(&bank().aborted, &bank().overflow, 1);
            }
            if started.is_some() {
                bank().decrement(&bank().active_windows);
            }
        }
        let real = inner(file);
        let before = (*real).pMethods;
        let rc = match (*before).xFileControl {
            Some(callback) => callback(real, op, arg),
            None => ffi::SQLITE_NOTFOUND,
        };
        if (*real).pMethods != before {
            // On macOS SET_LOCKPROXYFILE can switch a native POSIX v3 table
            // to a proxy v1 table. Withdraw exactly the capabilities it does.
            if (*real).pMethods.is_null() {
                (*wrapped).header.pMethods = ptr::null();
            } else {
                (*wrapped).methods = native_methods((*real).pMethods);
                (*wrapped).header.pMethods = ptr::addr_of!((*wrapped).methods);
            }
        }
        rc
    }
}

macro_rules! io_forward {
    ($name:ident, $field:ident, ($($arg:ident: $ty:ty),*) -> $ret:ty, $missing:expr) => {
        unsafe extern "C" fn $name(file: *mut ffi::sqlite3_file, $($arg: $ty),*) -> $ret {
            unsafe { let real = inner(file); match (*(*real).pMethods).$field {
                Some(callback) => callback(real, $($arg),*), None => $missing,
            } }
        }
    };
}
io_forward!(truncate, xTruncate, (size: ffi::sqlite3_int64) -> c_int, ffi::SQLITE_IOERR_TRUNCATE);
io_forward!(file_size, xFileSize, (size: *mut ffi::sqlite3_int64) -> c_int, ffi::SQLITE_IOERR_FSTAT);
io_forward!(lock, xLock, (kind: c_int) -> c_int, ffi::SQLITE_IOERR_LOCK);
io_forward!(unlock, xUnlock, (kind: c_int) -> c_int, ffi::SQLITE_IOERR_UNLOCK);
io_forward!(reserved, xCheckReservedLock, (out: *mut c_int) -> c_int, ffi::SQLITE_IOERR_CHECKRESERVEDLOCK);
io_forward!(sector_size, xSectorSize, () -> c_int, 0);
io_forward!(characteristics, xDeviceCharacteristics, () -> c_int, 0);
io_forward!(shm_map, xShmMap, (page: c_int, size: c_int, extend: c_int, out: *mut *mut c_void) -> c_int, ffi::SQLITE_IOERR_SHMMAP);
io_forward!(shm_lock, xShmLock, (offset: c_int, count: c_int, flags: c_int) -> c_int, ffi::SQLITE_IOERR_SHMLOCK);
io_forward!(shm_barrier, xShmBarrier, () -> (), ());
io_forward!(shm_unmap, xShmUnmap, (delete: c_int) -> c_int, ffi::SQLITE_IOERR_SHMOPEN);
io_forward!(fetch, xFetch, (offset: ffi::sqlite3_int64, amount: c_int, out: *mut *mut c_void) -> c_int, ffi::SQLITE_IOERR_MMAP);
io_forward!(unfetch, xUnfetch, (offset: ffi::sqlite3_int64, buffer: *mut c_void) -> c_int, ffi::SQLITE_IOERR_MMAP);

fn empty_methods() -> ffi::sqlite3_io_methods {
    ffi::sqlite3_io_methods {
        iVersion: 0,
        xClose: None,
        xRead: None,
        xWrite: None,
        xTruncate: None,
        xSync: None,
        xFileSize: None,
        xLock: None,
        xUnlock: None,
        xCheckReservedLock: None,
        xFileControl: None,
        xSectorSize: None,
        xDeviceCharacteristics: None,
        xShmMap: None,
        xShmLock: None,
        xShmBarrier: None,
        xShmUnmap: None,
        xFetch: None,
        xUnfetch: None,
    }
}
unsafe fn native_methods(native: *const ffi::sqlite3_io_methods) -> ffi::sqlite3_io_methods {
    unsafe {
        // Read individual fields, never copy a complete v3 table from a v1
        // allocation. Optional native methods remain NULL in the wrapper.
        let version = (*native).iVersion;
        let mut result = empty_methods();
        result.iVersion = version.min(3);
        result.xClose = (*native).xClose.map(|_| close as _);
        result.xRead = (*native).xRead.map(|_| read as _);
        result.xWrite = (*native).xWrite.map(|_| write as _);
        result.xTruncate = (*native).xTruncate.map(|_| truncate as _);
        result.xSync = (*native).xSync.map(|_| sync as _);
        result.xFileSize = (*native).xFileSize.map(|_| file_size as _);
        result.xLock = (*native).xLock.map(|_| lock as _);
        result.xUnlock = (*native).xUnlock.map(|_| unlock as _);
        result.xCheckReservedLock = (*native).xCheckReservedLock.map(|_| reserved as _);
        result.xFileControl = (*native).xFileControl.map(|_| file_control as _);
        result.xSectorSize = (*native).xSectorSize.map(|_| sector_size as _);
        result.xDeviceCharacteristics = (*native)
            .xDeviceCharacteristics
            .map(|_| characteristics as _);
        if version >= 2 {
            result.xShmMap = (*native).xShmMap.map(|_| shm_map as _);
            result.xShmLock = (*native).xShmLock.map(|_| shm_lock as _);
            result.xShmBarrier = (*native).xShmBarrier.map(|_| shm_barrier as _);
            result.xShmUnmap = (*native).xShmUnmap.map(|_| shm_unmap as _);
        }
        if version >= 3 {
            result.xFetch = (*native).xFetch.map(|_| fetch as _);
            result.xUnfetch = (*native).xUnfetch.map(|_| unfetch as _);
        }
        result
    }
}

macro_rules! vfs_forward {
    ($name:ident, $field:ident, ($($arg:ident: $ty:ty),*) -> $ret:ty, $missing:expr) => {
        unsafe extern "C" fn $name(vfs: *mut ffi::sqlite3_vfs, $($arg: $ty),*) -> $ret {
            unsafe { let parent = (*(*vfs).pAppData.cast::<Context>()).parent;
                match (*parent).$field { Some(callback) => callback(parent, $($arg),*), None => $missing } }
        }
    };
}
type DlSymbol = Option<unsafe extern "C" fn(*mut ffi::sqlite3_vfs, *mut c_void, *const c_char)>;
vfs_forward!(delete, xDelete, (name: *const c_char, sync_dir: c_int) -> c_int, ffi::SQLITE_IOERR_DELETE);
vfs_forward!(access, xAccess, (name: *const c_char, flags: c_int, out: *mut c_int) -> c_int, ffi::SQLITE_IOERR_ACCESS);
vfs_forward!(full_path, xFullPathname, (name: *const c_char, length: c_int, out: *mut c_char) -> c_int, ffi::SQLITE_CANTOPEN);
vfs_forward!(dl_open, xDlOpen, (name: *const c_char) -> *mut c_void, ptr::null_mut());
vfs_forward!(dl_error, xDlError, (length: c_int, out: *mut c_char) -> (), ());
vfs_forward!(dl_sym, xDlSym, (handle: *mut c_void, name: *const c_char) -> DlSymbol, None);
vfs_forward!(dl_close, xDlClose, (handle: *mut c_void) -> (), ());
vfs_forward!(randomness, xRandomness, (length: c_int, out: *mut c_char) -> c_int, 0);
vfs_forward!(sleep, xSleep, (micros: c_int) -> c_int, 0);
vfs_forward!(current_time, xCurrentTime, (out: *mut f64) -> c_int, ffi::SQLITE_ERROR);
vfs_forward!(last_error, xGetLastError, (length: c_int, out: *mut c_char) -> c_int, 0);
vfs_forward!(current_time_int64, xCurrentTimeInt64, (out: *mut ffi::sqlite3_int64) -> c_int, ffi::SQLITE_ERROR);
vfs_forward!(set_syscall, xSetSystemCall, (name: *const c_char, callback: ffi::sqlite3_syscall_ptr) -> c_int, ffi::SQLITE_NOTFOUND);
vfs_forward!(get_syscall, xGetSystemCall, (name: *const c_char) -> ffi::sqlite3_syscall_ptr, None);
vfs_forward!(next_syscall, xNextSystemCall, (name: *const c_char) -> *const c_char, ptr::null());

unsafe fn native_vfs(parent: *mut ffi::sqlite3_vfs) -> ffi::sqlite3_vfs {
    unsafe {
        let version = (*parent).iVersion;
        // pNext is managed by SQLite, and is never copied from its live list.
        ffi::sqlite3_vfs {
            iVersion: version,
            szOsFile: 0,
            mxPathname: (*parent).mxPathname,
            pNext: ptr::null_mut(),
            zName: ptr::null(),
            pAppData: ptr::null_mut(),
            xOpen: (*parent).xOpen.map(|_| open as _),
            xDelete: (*parent).xDelete.map(|_| delete as _),
            xAccess: (*parent).xAccess.map(|_| access as _),
            xFullPathname: (*parent).xFullPathname.map(|_| full_path as _),
            xDlOpen: (*parent).xDlOpen.map(|_| dl_open as _),
            xDlError: (*parent).xDlError.map(|_| dl_error as _),
            xDlSym: (*parent).xDlSym.map(|_| dl_sym as _),
            xDlClose: (*parent).xDlClose.map(|_| dl_close as _),
            xRandomness: (*parent).xRandomness.map(|_| randomness as _),
            xSleep: (*parent).xSleep.map(|_| sleep as _),
            xCurrentTime: (*parent).xCurrentTime.map(|_| current_time as _),
            xGetLastError: (*parent).xGetLastError.map(|_| last_error as _),
            xCurrentTimeInt64: if version >= 2 {
                (*parent).xCurrentTimeInt64.map(|_| current_time_int64 as _)
            } else {
                None
            },
            xSetSystemCall: if version >= 3 {
                (*parent).xSetSystemCall.map(|_| set_syscall as _)
            } else {
                None
            },
            xGetSystemCall: if version >= 3 {
                (*parent).xGetSystemCall.map(|_| get_syscall as _)
            } else {
                None
            },
            xNextSystemCall: if version >= 3 {
                (*parent).xNextSystemCall.map(|_| next_syscall as _)
            } else {
                None
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_bank_byte_accounting_and_reset_preserve_live_gauges() {
        let b = Bank::default();
        b.live_files.store(3, Ordering::Relaxed);
        b.registered.store(2, Ordering::Relaxed);
        b.live_contexts.store(4, Ordering::Relaxed);
        b.operations[0].record(&b, Some(1000), 17, ffi::SQLITE_IOERR_SHORT_READ, true);
        b.operations[0].record(&b, Some(2000), 17, ffi::SQLITE_OK, true);
        let v = b.snapshot(true);
        assert_eq!(v["entries"][0]["completed"], 2);
        assert_eq!(v["entries"][0]["requested_bytes"], 34);
        assert_eq!(v["entries"][0]["confirmed_bytes"], 17);
        assert_eq!(v["entries"][0]["short_reads"], 1);
        assert_eq!(v["entries"][0]["errors"], 1);
        assert_eq!(b.snapshot(false)["entries"][0]["completed"], 0);
        assert_eq!(b.snapshot(false)["live_files"], 3);
        assert_eq!(b.snapshot(false)["registered_vfs"], 2);
        assert_eq!(b.snapshot(false)["live_contexts"], 4);
        b.enter();
        assert!(b.snapshot(true).get("error").is_some());
        assert_eq!(b.in_flight.load(Ordering::Relaxed), 1);
        b.decrement(&b.in_flight);
        b.active_windows.store(1, Ordering::Relaxed);
        assert!(b.snapshot(true).get("error").is_some());
        assert_eq!(b.active_windows.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn detected_reset_race_stays_unavailable_without_mutating_counters() {
        let b = Bank::default();
        b.operations[0].record(&b, Some(1000), 3, ffi::SQLITE_OK, true);
        b.resetting.store(true, Ordering::Release);
        b.enter();
        b.decrement(&b.in_flight);
        b.resetting.store(false, Ordering::Release);
        assert!(b.snapshot(false).get("error").is_some());
        assert!(b.snapshot(true).get("error").is_some());
        assert_eq!(b.operations[0].requested.load(Ordering::Relaxed), 3);
        assert_eq!(b.in_flight.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn elapsed_overflow_preserves_call_and_byte_counts_as_invalid_timing() {
        let b = Bank::default();
        assert_eq!(
            duration_ns(std::time::Duration::from_nanos(u64::MAX)),
            Some(u64::MAX)
        );
        let overflow = duration_ns(std::time::Duration::new(u64::MAX, 999_999_999));
        assert_eq!(overflow, None);
        b.operations[1].record(&b, overflow, 17, ffi::SQLITE_OK, false);
        b.paired.record(overflow);
        let value = b.snapshot(false);
        for timing in [&value["entries"][1], &value["checkpoint"]["paired"]] {
            assert_eq!(timing["completed"], 1);
            assert_eq!(timing["invalid_elapsed"], 1);
            assert_eq!(timing["elapsed_ns"], 0);
            assert_eq!(timing["max_elapsed_ns"], 0);
            assert_eq!(
                timing["histogram_log2_us"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap())
                    .sum::<u64>(),
                0
            );
        }
        assert_eq!(value["entries"][1]["requested_bytes"], 17);
        assert_eq!(value["entries"][1]["confirmed_bytes"], 17);
    }

    #[test]
    fn observer_scope_is_nested_thread_local_and_restored_on_unwind() {
        assert!(!OBSERVER.with(Cell::get));
        {
            let _outer = ObserverScope::new();
            assert!(OBSERVER.with(Cell::get));
            {
                let _inner = ObserverScope::new();
                assert!(OBSERVER.with(Cell::get));
            }
            assert!(OBSERVER.with(Cell::get));
            assert!(
                !std::thread::spawn(|| OBSERVER.with(Cell::get))
                    .join()
                    .unwrap()
            );
        }
        assert!(!OBSERVER.with(Cell::get));
        let result = std::panic::catch_unwind(|| {
            let _scope = ObserverScope::new();
            panic!("observer scope unwind control");
        });
        assert!(result.is_err());
        assert!(!OBSERVER.with(Cell::get));
    }

    #[test]
    fn versioned_optional_io_methods_remain_absent() {
        let mut native = empty_methods();
        native.iVersion = 1;
        native.xFetch = Some(fetch);
        native.xShmMap = Some(shm_map);
        let wrapped = unsafe { native_methods(&native) };
        assert_eq!(wrapped.iVersion, 1);
        assert!(wrapped.xFetch.is_none());
        assert!(wrapped.xShmMap.is_none());
        assert!(wrapped.xSectorSize.is_none());
        native.iVersion = 2;
        let wrapped = unsafe { native_methods(&native) };
        assert!(wrapped.xShmMap.is_some());
        assert!(wrapped.xFetch.is_none());
        native.iVersion = 3;
        let wrapped = unsafe { native_methods(&native) };
        assert!(wrapped.xFetch.is_some());
        assert!(wrapped.xUnfetch.is_none());
    }

    #[test]
    fn versioned_optional_vfs_methods_remain_absent() {
        let mut native: ffi::sqlite3_vfs = unsafe { std::mem::zeroed() };
        native.iVersion = 1;
        native.pNext = ptr::from_mut(&mut native);
        native.xCurrentTimeInt64 = Some(current_time_int64);
        native.xSetSystemCall = Some(set_syscall);
        native.xGetSystemCall = Some(get_syscall);
        native.xNextSystemCall = Some(next_syscall);
        let wrapped = unsafe { native_vfs(&mut native) };
        assert!(wrapped.pNext.is_null());
        assert!(wrapped.xOpen.is_none());
        assert!(wrapped.xCurrentTimeInt64.is_none());
        assert!(wrapped.xSetSystemCall.is_none());
        native.iVersion = 2;
        let wrapped = unsafe { native_vfs(&mut native) };
        assert!(wrapped.xCurrentTimeInt64.is_some());
        assert!(wrapped.xSetSystemCall.is_none());
        native.iVersion = 3;
        let wrapped = unsafe { native_vfs(&mut native) };
        assert!(wrapped.xSetSystemCall.is_some());
        assert!(wrapped.xGetSystemCall.is_some());
        assert!(wrapped.xNextSystemCall.is_some());
    }

    // The process bank is shared with real provider tests. Native forwarding
    // controls require the same exclusive ownership as registry controls.
    #[test]
    #[ignore = "requires exclusive process VFS bank phase ownership"]
    fn failed_open_short_read_and_checkpoint_signals_forward_unchanged() {
        #[repr(C)]
        struct Native {
            header: ffi::sqlite3_file,
            closed: bool,
        }
        static FORWARDED: AtomicBool = AtomicBool::new(true);
        unsafe extern "C" fn native_close(file: *mut ffi::sqlite3_file) -> c_int {
            unsafe {
                (*file.cast::<Native>()).closed = true;
            }
            ffi::SQLITE_IOERR_CLOSE
        }
        unsafe extern "C" fn native_read(
            _file: *mut ffi::sqlite3_file,
            out: *mut c_void,
            n: c_int,
            offset: ffi::sqlite3_int64,
        ) -> c_int {
            FORWARDED.fetch_and(offset == 31 && n == 13 && !out.is_null(), Ordering::Relaxed);
            if n == 13 && !out.is_null() {
                unsafe {
                    ptr::write_bytes(out, 0, 13);
                }
            }
            ffi::SQLITE_IOERR_SHORT_READ
        }
        unsafe extern "C" fn native_write(
            _file: *mut ffi::sqlite3_file,
            bytes: *const c_void,
            n: c_int,
            offset: ffi::sqlite3_int64,
        ) -> c_int {
            let valid = n == 7 && offset == 99 && !bytes.is_null();
            FORWARDED.fetch_and(valid, Ordering::Relaxed);
            if valid {
                FORWARDED.fetch_and(
                    unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), 7) } == [0x55u8; 7],
                    Ordering::Relaxed,
                );
            }
            ffi::SQLITE_IOERR_WRITE
        }
        unsafe extern "C" fn native_sync(_file: *mut ffi::sqlite3_file, flags: c_int) -> c_int {
            FORWARDED.fetch_and(
                flags == (ffi::SQLITE_SYNC_FULL | ffi::SQLITE_SYNC_DATAONLY),
                Ordering::Relaxed,
            );
            ffi::SQLITE_IOERR_FSYNC
        }
        unsafe extern "C" fn native_control(
            file: *mut ffi::sqlite3_file,
            op: c_int,
            arg: *mut c_void,
        ) -> c_int {
            FORWARDED.fetch_and(arg.is_null(), Ordering::Relaxed);
            if op == ffi::SQLITE_FCNTL_SET_LOCKPROXYFILE {
                unsafe {
                    (*file).pMethods = &BASE_METHODS;
                }
            }
            ffi::SQLITE_NOTFOUND
        }
        unsafe extern "C" fn native_open(
            vfs: *mut ffi::sqlite3_vfs,
            name: ffi::sqlite3_filename,
            out: *mut ffi::sqlite3_file,
            flags: c_int,
            out_flags: *mut c_int,
        ) -> c_int {
            FORWARDED.fetch_and(
                name.is_null() && flags == ffi::SQLITE_OPEN_TEMP_DB && !out_flags.is_null(),
                Ordering::Relaxed,
            );
            unsafe {
                FORWARDED.fetch_and(
                    !(*vfs).pAppData.is_null() && *(*vfs).pAppData.cast::<usize>() == 7,
                    Ordering::Relaxed,
                );
                if !out_flags.is_null() {
                    *out_flags = ffi::SQLITE_OPEN_READONLY;
                }
                ptr::write(
                    out.cast::<Native>(),
                    Native {
                        header: ffi::sqlite3_file { pMethods: &METHODS },
                        closed: false,
                    },
                );
            }
            ffi::SQLITE_CANTOPEN
        }
        static BASE_METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
            iVersion: 1,
            xClose: Some(native_close),
            xRead: Some(native_read),
            xFileControl: Some(native_control),
            xWrite: Some(native_write),
            xTruncate: None,
            xSync: Some(native_sync),
            xFileSize: None,
            xLock: None,
            xUnlock: None,
            xCheckReservedLock: None,
            xSectorSize: None,
            xDeviceCharacteristics: None,
            xShmMap: None,
            xShmLock: None,
            xShmBarrier: None,
            xShmUnmap: None,
            xFetch: None,
            xUnfetch: None,
        };
        static METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
            iVersion: 3,
            xShmMap: Some(shm_map),
            xFetch: Some(fetch),
            ..BASE_METHODS
        };
        let mut parent: ffi::sqlite3_vfs = unsafe { std::mem::zeroed() };
        let mut parent_data = 7usize;
        parent.iVersion = 1;
        parent.pAppData = ptr::from_mut(&mut parent_data).cast();
        parent.xOpen = Some(native_open);
        let offset = (size_of::<File>() + 7) & !7;
        let mut context = Context {
            parent: &mut parent,
            inner_offset: offset,
        };
        let mut wrapper = unsafe { native_vfs(&mut parent) };
        wrapper.pAppData = ptr::from_mut(&mut context).cast();
        let mut memory = vec![0u64; (offset + size_of::<Native>()).div_ceil(8)];
        let file = memory.as_mut_ptr().cast::<ffi::sqlite3_file>();
        let mut flags = 0;
        FORWARDED.store(true, Ordering::Relaxed);
        assert!(snapshot(true).get("error").is_none());
        assert_eq!(
            unsafe {
                open(
                    &mut wrapper,
                    ptr::null(),
                    file,
                    ffi::SQLITE_OPEN_TEMP_DB,
                    &mut flags,
                )
            },
            ffi::SQLITE_CANTOPEN
        );
        assert_eq!(flags, ffi::SQLITE_OPEN_READONLY);
        assert!(!unsafe { (*file).pMethods.is_null() });
        assert_eq!(unsafe { (*(*file).pMethods).iVersion }, 3);
        assert!(unsafe { (*(*file).pMethods).xShmMap.is_some() });
        assert!(unsafe { (*(*file).pMethods).xFetch.is_some() });
        assert_eq!(
            unsafe { file_control(file, ffi::SQLITE_FCNTL_SET_LOCKPROXYFILE, ptr::null_mut()) },
            ffi::SQLITE_NOTFOUND
        );
        assert_eq!(unsafe { (*(*file).pMethods).iVersion }, 1);
        assert!(unsafe { (*(*file).pMethods).xShmMap.is_none() });
        assert!(unsafe { (*(*file).pMethods).xFetch.is_none() });
        let mut bytes = [9u8; 13];
        assert_eq!(
            unsafe { read(file, bytes.as_mut_ptr().cast(), 13, 31) },
            ffi::SQLITE_IOERR_SHORT_READ
        );
        assert_eq!(bytes, [0u8; 13]);
        assert_eq!(
            unsafe { write(file, [0x55u8; 7].as_ptr().cast(), 7, 99) },
            ffi::SQLITE_IOERR_WRITE
        );
        assert_eq!(
            unsafe { sync(file, ffi::SQLITE_SYNC_FULL | ffi::SQLITE_SYNC_DATAONLY) },
            ffi::SQLITE_IOERR_FSYNC
        );
        {
            let _observer = ObserverScope::new();
            let mut memory = vec![0u64; (offset + size_of::<Native>()).div_ceil(8)];
            let observer_file = memory.as_mut_ptr().cast::<ffi::sqlite3_file>();
            assert_eq!(
                unsafe {
                    open(
                        &mut wrapper,
                        ptr::null(),
                        observer_file,
                        ffi::SQLITE_OPEN_TEMP_DB,
                        &mut flags,
                    )
                },
                ffi::SQLITE_CANTOPEN
            );
            assert_eq!(
                unsafe { read(observer_file, bytes.as_mut_ptr().cast(), 13, 31) },
                ffi::SQLITE_IOERR_SHORT_READ
            );
            assert_eq!(
                unsafe {
                    file_control(observer_file, ffi::SQLITE_FCNTL_CKPT_START, ptr::null_mut())
                },
                ffi::SQLITE_NOTFOUND
            );
            assert_eq!(
                unsafe {
                    file_control(observer_file, ffi::SQLITE_FCNTL_CKPT_DONE, ptr::null_mut())
                },
                ffi::SQLITE_NOTFOUND
            );
            assert_eq!(unsafe { close(observer_file) }, ffi::SQLITE_IOERR_CLOSE);
        }
        assert_eq!(
            unsafe { file_control(file, ffi::SQLITE_FCNTL_CKPT_DONE, ptr::null_mut()) },
            ffi::SQLITE_NOTFOUND
        );
        unsafe {
            file_control(file, ffi::SQLITE_FCNTL_CKPT_START, ptr::null_mut());
            file_control(file, ffi::SQLITE_FCNTL_CKPT_START, ptr::null_mut());
        }
        assert!(snapshot(true).get("error").is_some());
        assert_eq!(unsafe { close(file) }, ffi::SQLITE_IOERR_CLOSE);
        assert!(unsafe { (*inner(file).cast::<Native>()).closed });
        assert!(FORWARDED.load(Ordering::Relaxed));
        let value = snapshot(false);
        assert_eq!(value["open_attempts"], 2);
        assert_eq!(value["open_errors"], 2);
        assert_eq!(value["files_opened"], 2);
        assert_eq!(value["close_errors"], 2);
        assert_eq!(value["close_calls"], 2);
        assert_eq!(value["live_files"], 0);
        assert_eq!(value["in_flight"], 0);
        assert_eq!(value["entries"][9]["short_reads"], 1);
        assert_eq!(value["entries"][9]["completed"], 1);
        assert_eq!(value["entries"][9]["confirmed_bytes"], 0);
        assert_eq!(value["entries"][10]["errors"], 1);
        assert_eq!(value["entries"][10]["requested_bytes"], 7);
        assert_eq!(value["entries"][10]["confirmed_bytes"], 0);
        assert_eq!(value["entries"][11]["completed"], 1);
        assert_eq!(value["entries"][11]["errors"], 1);
        assert_eq!(value["entries"][11]["requested_bytes"], 0);
        assert_eq!(value["checkpoint"]["starts"], 2);
        assert_eq!(value["checkpoint"]["unmatched_starts"], 1);
        assert_eq!(value["checkpoint"]["unmatched_dones"], 1);
        assert_eq!(value["checkpoint"]["aborted_windows"], 1);
        assert_eq!(value["checkpoint"]["active_windows"], 0);
        assert!(snapshot(true).get("error").is_none());
    }
}
