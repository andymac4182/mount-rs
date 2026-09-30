//! Selected root-file structural operations over a previously audited graph.
//! The operation gate owns capture through acknowledged receipt installation.
use super::*;
use mount_rs_core::storage::compact::{
    CompactRootFileCapability, CompactRootFileIntent, CompactRootFileTimes,
    CompactRootFileTransition,
};

fn ordinary_root_name(path: &str) -> Option<&str> {
    let name = path.strip_prefix('/')?;
    (!name.is_empty() && !name.contains('/') && !name.contains('\0') && name != "." && name != "..")
        .then_some(name)
}

pub(super) enum RootFileAttempt {
    Unsupported,
    Cancelled,
    Committed,
}

impl<M, B> ChunkedFs<M, B>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    /// Unsupported shapes use the existing Full operation for pathname
    /// semantics. EAGAIN is a proven retry before acknowledged installation.
    pub(super) async fn try_compact_root_file_mutation(
        &self,
        path: &str,
        destination: Option<&str>,
        phase: GatePhasePermit<'_>,
        reply: Option<&tokio::sync::oneshot::Sender<Result<MutationResult>>>,
    ) -> Result<RootFileAttempt> {
        if !self.inner.options.compact_inode_updates
            || !self.inner.options.concurrent_writes
            || self.inner.options.delegated
            || self.inner.metadata.compact_root_file_capability()
                != CompactRootFileCapability::Supported
        {
            return Ok(RootFileAttempt::Unsupported);
        }
        if reply.is_some_and(|reply| reply.is_closed()) {
            return Ok(RootFileAttempt::Cancelled);
        }
        let Some(name) = ordinary_root_name(path) else {
            return Ok(RootFileAttempt::Unsupported);
        };
        let to = match destination {
            Some(path) => {
                let Some(to) = ordinary_root_name(path) else {
                    return Ok(RootFileAttempt::Unsupported);
                };
                if to == name {
                    return Ok(RootFileAttempt::Unsupported);
                }
                Some(to)
            }
            None => None,
        };
        self.check_inode_runtime()?;
        let (revision, structure, root, inode, cached_file, cached_identity) = {
            let state = self.lock_state()?;
            let compact = state.compact.as_ref().ok_or_else(stale_inode_structure)?;
            let root = state.namespace.root;
            let Some(NodeMetadata {
                data: NodeData::Directory { entries },
                ..
            }) = state.namespace.nodes.get(&root)
            else {
                return Err(stale_inode_structure());
            };
            let Some(entry) = entries.iter().find(|entry| entry.name == name) else {
                return Ok(RootFileAttempt::Unsupported);
            };
            if to.is_some_and(|to| entries.iter().any(|entry| entry.name == to))
                || !compact.structure.root_single_link_file(entry.inode)
            {
                return Ok(RootFileAttempt::Unsupported);
            }
            let inode = entry.inode;
            let cached = state
                .selected_inodes
                .get(&inode)
                .map(Arc::as_ref)
                .or_else(|| state.namespace.nodes.get(&inode))
                .ok_or_else(stale_inode_structure)?;
            let identity = compact
                .physical
                .get(&inode)
                .copied()
                .ok_or_else(stale_inode_structure)?;
            (
                state.revision,
                compact.structure.clone(),
                root,
                inode,
                cached.clone(),
                identity,
            )
        };
        // Only eligible singleton queue requests enter this attempt. Full
        // fallback retains its original accounting and rename has no queue.
        let mut observed = reply.map(|_| {
            let mut observed = BatchAttemptObservation::new();
            observed.considered();
            observed
        });
        let result = async {
            let backing = self
                .inner
                .concurrent_backing
                .ok_or_else(stale_inode_structure)?;
            let read = {
                let _refresh = phase.phase(GatePhase::Refresh);
                self.inner
                    .blocks
                    .verify_concurrent_backing(backing)
                    .await
                    .map_err(|error| self.fail_closed(error))?;
                self.inner
                    .metadata
                    .load_compact_root_file(backing, root, inode)
                    .await
                    .map_err(|error| self.fail_closed(error))?
            };
            if read.anchor().generation != structure.anchor().generation {
                let snapshot = self
                    .inner
                    .metadata
                    .load_compact_snapshot(backing)
                    .await
                    .map_err(|error| self.fail_closed(error))?;
                self.install_compact_snapshot(snapshot, revision, None)?;
                return Err(FsError::new(ErrorCode::Eagain));
            }
            if reply.is_some_and(|reply| reply.is_closed()) {
                return Ok(RootFileAttempt::Cancelled);
            }
            let verified = structure
                .verify_root_file(read, inode, &cached_file, cached_identity)
                .map_err(|error| {
                    self.fail_closed(if error.code == ErrorCode::Eagain {
                        stale_inode_structure()
                    } else {
                        error
                    })
                })?;
            {
                let state = self.lock_state()?;
                if state.revision != revision {
                    return Err(FsError::new(ErrorCode::Eagain));
                }
                let compact = state.compact.as_ref().ok_or_else(stale_inode_structure)?;
                let current_file = state
                    .selected_inodes
                    .get(&inode)
                    .map(Arc::as_ref)
                    .or_else(|| state.namespace.nodes.get(&inode));
                if state.persisted_revision != structure.anchor().generation
                    || !compact.structure.same_witness(&structure)
                    || compact.physical.get(&inode) != Some(&cached_identity)
                    || current_file != Some(&cached_file)
                {
                    return Err(FsError::new(ErrorCode::Eagain));
                }
            }
            let mut parent_times = verified.root().node.stats.clone();
            touch_modified(&mut parent_times, true)?;
            if to.is_some() {
                touch_modified(&mut parent_times, true)?;
            }
            let mut file_times = verified.file().node.stats.clone();
            touch_changed(&mut file_times, true)?;
            let intent = match to {
                Some(to) => CompactRootFileIntent::RenameAbsent {
                    from: name.into(),
                    to: to.into(),
                },
                None => CompactRootFileIntent::UnlinkLastLink { name: name.into() },
            };
            let proposal = CompactRootFileTransition::capture(
                verified,
                intent,
                CompactRootFileTimes {
                    parent_mtime_ms: parent_times.mtime_ms,
                    parent_ctime_ms: parent_times.ctime_ms,
                    file_ctime_ms: file_times.ctime_ms,
                },
            )?;
            if self
                .publish_compact_root_file_mutation(revision, proposal, to.is_some(), reply, phase)
                .await?
            {
                Ok(RootFileAttempt::Committed)
            } else {
                Ok(RootFileAttempt::Cancelled)
            }
        }
        .await;
        if let Some(observed) = &mut observed {
            observed.finish(match &result {
                Ok(RootFileAttempt::Committed) => AttemptOutcome::Success,
                Ok(RootFileAttempt::Cancelled | RootFileAttempt::Unsupported) => {
                    AttemptOutcome::NoPublication
                }
                Err(error) if error.code == ErrorCode::Eagain => AttemptOutcome::Conflict,
                Err(_) => AttemptOutcome::Error,
            });
        }
        result
    }

    async fn publish_compact_root_file_mutation(
        &self,
        revision: u64,
        proposal: CompactRootFileTransition,
        flush_blocks: bool,
        reply: Option<&tokio::sync::oneshot::Sender<Result<MutationResult>>>,
        phase: GatePhasePermit<'_>,
    ) -> Result<bool> {
        let _publication = phase.phase(GatePhase::Publication);
        self.check_inode_runtime()?;
        if self.lock_state()?.revision != revision {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        if flush_blocks {
            self.inner.blocks.flush().await?;
        }
        let backing = self
            .inner
            .concurrent_backing
            .ok_or_else(stale_inode_structure)?;
        self.inner
            .blocks
            .verify_concurrent_backing(backing)
            .await
            .map_err(|error| self.fail_closed(error))?;
        if reply.is_some_and(|reply| reply.is_closed()) {
            return Ok(false);
        }
        let mut publication = PublicationGuard::new(&self.inner.state, &self.inner.failed);
        let receipt = match self
            .inner
            .metadata
            .publish_compact_structure(proposal.delta())
            .await
        {
            Ok(receipt) => receipt,
            Err(error) if error.code == ErrorCode::Eagain => {
                publication.disarm();
                return Err(error);
            }
            Err(error) => return Err(self.fail_closed(error)),
        };
        let next_structure = proposal
            .validate_publication(&receipt)
            .map_err(|error| self.fail_closed(error))?;
        if !self.inner.metadata.publish_includes_flush_barrier() {
            self.inner
                .metadata
                .flush()
                .await
                .map_err(|error| self.fail_closed(error))?;
        }
        self.check_inode_runtime()?;
        {
            let mut state = self.lock_state()?;
            if state.revision != revision {
                return Err(FsError::new(ErrorCode::Estale));
            }
            let next_revision = state
                .revision
                .checked_add(1)
                .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
            let cloned_nodes = if Arc::strong_count(&state.namespace) > 1 {
                state.namespace.nodes.len() as u64
            } else {
                0
            };
            {
                let _clone = Span::new(Event::MutationCandidateCloneNodes).units(cloned_nodes);
                let namespace = Arc::make_mut(&mut state.namespace);
                for (&inode, guard) in &receipt.upserts {
                    namespace.nodes.insert(inode, guard.node.clone());
                }
            }
            let compact = state.compact.as_mut().ok_or_else(stale_inode_structure)?;
            for (&inode, guard) in &receipt.upserts {
                compact.physical.insert(inode, guard.identity);
            }
            compact.structure = next_structure;
            compact.pending_full = None;
            for inode in receipt.upserts.keys() {
                state.selected_inodes.remove(inode);
            }
            state.revision = next_revision;
            state.persisted_revision = receipt.anchor.generation;
            state.inode_revisions.clear();
            // No atime or unrelated selected body was published. Their retained
            // body/physical pairs and pending atimes remain intact.
        }
        publication.disarm();
        Ok(true)
    }

    pub(super) async fn try_compact_root_file_rename(&self, from: &str, to: &str) -> Result<bool> {
        if !self.inner.options.compact_inode_updates
            || !self.inner.options.concurrent_writes
            || self.inner.options.delegated
        {
            return Ok(false);
        }
        let _lifecycle = self.inner.lifecycle.read().await;
        let _gate = self.operation_gate(GateKind::Metadata).await;
        for attempt in 0..MAX_CONCURRENT_CAS_RETRIES {
            match self
                .try_compact_root_file_mutation(from, Some(to), _gate.phase_permit(), None)
                .await
            {
                Ok(RootFileAttempt::Committed) => return Ok(true),
                Ok(RootFileAttempt::Unsupported | RootFileAttempt::Cancelled) => return Ok(false),
                Err(error)
                    if error.code == ErrorCode::Eagain
                        && attempt + 1 < MAX_CONCURRENT_CAS_RETRIES =>
                {
                    let _backoff = _gate.phase_permit().phase(GatePhase::CasBackoff);
                    concurrent_cas_backoff(attempt, &self.inner.options.owner).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(FsError::new(ErrorCode::Eagain))
    }
}
