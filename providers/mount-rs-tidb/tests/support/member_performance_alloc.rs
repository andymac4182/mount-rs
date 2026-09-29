//! Requested Rust allocation traffic on the current test thread, not RSS.
//! The window includes the driver/runtime work on that thread during publish.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Counts {
    pub calls: u64,
    pub requested_bytes: u64,
}
thread_local! {
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    static COUNTS: Cell<Counts> = const { Cell::new(Counts { calls: 0, requested_bytes: 0 }) };
}
fn record(bytes: usize) {
    let _ = BUSY.try_with(|busy| {
        if busy.replace(true) {
            return;
        }
        let _ = RECORDING.try_with(|active| {
            if active.get() {
                let _ = COUNTS.try_with(|counts| {
                    let old = counts.get();
                    counts.set(Counts {
                        calls: old.calls.saturating_add(1),
                        requested_bytes: old.requested_bytes.saturating_add(bytes as u64),
                    });
                });
            }
        });
        busy.set(false);
    });
}
struct Meter;
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Meter = Meter;

pub(super) struct Window;
impl Window {
    pub fn begin() -> Self {
        assert!(!RECORDING.with(Cell::get));
        COUNTS.with(|counts| counts.set(Counts::default()));
        RECORDING.with(|active| active.set(true));
        Self
    }
    pub fn finish(self) -> Counts {
        drop(self);
        COUNTS.with(Cell::get)
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        RECORDING.with(|active| active.set(false));
    }
}
