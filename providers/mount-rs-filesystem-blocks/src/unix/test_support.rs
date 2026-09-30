//! Private, per-store syscall-boundary controls; never compiled into production.
use super::{FsError, Result};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Point {
    File = 1,
    Directory = 2,
    Publication = 3,
}

#[derive(Debug)]
pub(super) struct Gate {
    pub started: SyncSender<()>,
    pub release: Receiver<()>,
    pub settled: SyncSender<()>,
}

#[derive(Default, Debug)]
pub(super) struct Faults {
    next: AtomicU8,
    pub events: Mutex<Vec<Point>>,
    pub gate: Mutex<Option<Gate>>,
}

impl Faults {
    pub fn fail_once(&self, point: Point) {
        self.next.store(point as u8, Ordering::SeqCst);
    }

    pub fn check(&self, point: Point) -> Result<()> {
        self.events
            .lock()
            .map_err(|_| FsError::backend("test boundary state poisoned"))?
            .push(point);
        if self
            .next
            .compare_exchange(point as u8, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Err(FsError::backend("injected filesystem barrier failure"));
        }
        Ok(())
    }

    pub fn pause_before_publication(&self) -> Result<Option<Completion>> {
        let gate = self
            .gate
            .lock()
            .map_err(|_| FsError::backend("test gate state poisoned"))?
            .take();
        let Some(gate) = gate else { return Ok(None) };
        let completion = Completion(gate.settled);
        gate.started
            .send(())
            .map_err(|_| FsError::backend("test publication observer disappeared"))?;
        gate.release
            .recv()
            .map_err(|_| FsError::backend("test publication release disappeared"))?;
        Ok(Some(completion))
    }
}

pub(super) struct Completion(SyncSender<()>);
impl Drop for Completion {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}
