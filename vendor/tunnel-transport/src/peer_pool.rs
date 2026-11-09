//! Checkout reads a published directory and per-peer slot. Directory changes
//! are rare and serialized. The writer never waits for a dial or shutdown.
use super::{ClientConnection, PeerDestination, PeerTransportError};
use arc_swap::{ArcSwap, ArcSwapOption};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

type Directory = HashMap<PeerDestination, Arc<ArcSwapOption<ClientConnection>>>;

#[derive(Default)]
pub(super) struct PublishedPool {
    directory: ArcSwap<Directory>,
    writer: Mutex<()>,
}

impl PublishedPool {
    pub(super) fn get(&self, destination: &PeerDestination) -> Option<Arc<ClientConnection>> {
        self.directory.load().get(destination)?.load_full()
    }

    pub(super) fn values(&self) -> Vec<Arc<ClientConnection>> {
        self.directory
            .load()
            .values()
            .filter_map(|slot| slot.load_full())
            .collect()
    }

    pub(super) async fn insert(
        &self,
        destination: PeerDestination,
        connection: Arc<ClientConnection>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), PeerTransportError> {
        let _writer = self.writer.lock().await;
        if cancel.is_cancelled() {
            return Err(PeerTransportError::Cancelled);
        }
        let directory = self.directory.load();
        if let Some(slot) = directory.get(&destination) {
            slot.store(Some(connection));
        } else {
            let mut next = (**directory).clone();
            next.insert(destination, Arc::new(ArcSwapOption::from(Some(connection))));
            self.directory.store(Arc::new(next));
        }
        Ok(())
    }

    pub(super) async fn remove(
        &self,
        destination: &PeerDestination,
    ) -> Option<Arc<ClientConnection>> {
        let _writer = self.writer.lock().await;
        let directory = self.directory.load();
        let slot = directory.get(destination)?;
        let removed = slot.swap(None);
        let mut next = (**directory).clone();
        next.remove(destination);
        self.directory.store(Arc::new(next));
        removed
    }

    pub(super) async fn remove_matching(
        &self,
        predicate: impl Fn(&ClientConnection) -> bool,
    ) -> Vec<Arc<ClientConnection>> {
        let _writer = self.writer.lock().await;
        let directory = self.directory.load();
        let mut removed = Vec::new();
        let mut next = (**directory).clone();
        next.retain(|_, slot| {
            if slot.load().as_ref().is_some_and(|c| predicate(c)) {
                removed.extend(slot.swap(None));
                false
            } else {
                true
            }
        });
        if !removed.is_empty() {
            self.directory.store(Arc::new(next));
        }
        removed
    }
}
