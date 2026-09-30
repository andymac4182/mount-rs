//! Private, per-store authority checkpoints; never compiled into production.
use super::{FsError, Result};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender};

#[derive(Debug)]
pub(super) struct Gate {
    pub started: SyncSender<()>,
    pub release: Receiver<()>,
    pub settled: SyncSender<()>,
}

#[derive(Default, Debug)]
pub(super) struct Faults {
    pub gate: Mutex<Option<Gate>>,
    pub post_publication_gate: Mutex<Option<Gate>>,
    pub flush_gate: Mutex<Option<Gate>>,
}

impl Faults {
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

    pub fn pause_after_publication(&self) -> Result<Option<Completion>> {
        let gate = self
            .post_publication_gate
            .lock()
            .map_err(|_| FsError::backend("test publication gate state poisoned"))?
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

    pub fn pause_after_flush_first_verification(&self) -> Result<Option<Completion>> {
        let gate = self
            .flush_gate
            .lock()
            .map_err(|_| FsError::backend("test flush gate state poisoned"))?
            .take();
        let Some(gate) = gate else { return Ok(None) };
        let completion = Completion(gate.settled);
        gate.started
            .send(())
            .map_err(|_| FsError::backend("test flush observer disappeared"))?;
        gate.release
            .recv()
            .map_err(|_| FsError::backend("test flush release disappeared"))?;
        Ok(Some(completion))
    }
}

pub(super) struct Completion(SyncSender<()>);
impl Drop for Completion {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}
