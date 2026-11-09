// Adversarial test waker: execute a handoff during a selected registration clone.
use std::{
    mem::ManuallyDrop,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{RawWaker, RawWakerVTable, Waker},
};
struct Handoff {
    clones: AtomicUsize,
    trigger: usize,
    action: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
unsafe fn clone_waker(pointer: *const ()) -> RawWaker {
    // SAFETY: every raw pointer owns one Arc reference; this borrow must not drop it.
    let state = ManuallyDrop::new(unsafe { Arc::from_raw(pointer.cast::<Handoff>()) });
    if state.clones.fetch_add(1, Ordering::SeqCst) + 1 == state.trigger
        && let Some(action) = state.action.lock().unwrap().take()
    {
        action();
    }
    raw(Arc::clone(&state))
}
unsafe fn wake(pointer: *const ()) {
    // SAFETY: consuming wake releases its one owned Arc reference.
    drop(unsafe { Arc::from_raw(pointer.cast::<Handoff>()) });
}
unsafe fn wake_by_ref(_: *const ()) {}
unsafe fn drop_waker(pointer: *const ()) {
    // SAFETY: dropping a raw waker releases its one owned Arc reference.
    drop(unsafe { Arc::from_raw(pointer.cast::<Handoff>()) });
}
static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_waker, wake, wake_by_ref, drop_waker);
fn raw(state: Arc<Handoff>) -> RawWaker {
    RawWaker::new(Arc::into_raw(state).cast(), &VTABLE)
}
pub fn on_clone(trigger: usize, action: impl FnOnce() + Send + 'static) -> Waker {
    let state = Arc::new(Handoff {
        clones: AtomicUsize::new(0),
        trigger,
        action: Mutex::new(Some(Box::new(action))),
    });
    // SAFETY: the vtable maintains owned Arc references for clone/wake/drop.
    unsafe { Waker::from_raw(raw(state)) }
}
