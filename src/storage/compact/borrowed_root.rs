//! Complete-root comparison stages for one fresh provider transaction view.
//!
//! The transaction owner must consume every started member/dentry result and
//! settle its terminal state before finishing a stage. Core cannot prove SQL
//! provenance or that a driver reached EOF. All observations must belong to one
//! fresh transaction view. A dropped stage yields no receipt. On a miss, the
//! provider drains its started result and runs ordinary owned validation in the
//! same transaction; that validation remains the authority for domain errors.

use super::{
    CheckedCompactInode, CompactAuthority, CompactDirectoryHeader, CompactInodeExpectation,
    ConcurrentBackingId, NodeData, PhysicalInodeIdentity, ValidatedCompactStructure,
    VerifiedCompactRoot,
};

// Scalars copied only after validation of the provider's fresh authority. The
// defaults and backing are validated there, without copying their owned data.
struct FreshRootAuthority {
    generation: u64,
    current_root: u64,
    next_inode: u64,
    members_count: u64,
}

struct RootCandidate<'audit> {
    witness: &'audit ValidatedCompactStructure,
    fresh: FreshRootAuthority,
    selected_inode: u64,
}

/// Compare all actual members before selected guard interpretation.
///
/// Every field is private. Completing this stage consumes it; no caller can
/// construct, clone, or reset an accepted stage from cached IDs or a Boolean.
pub struct CompactRootMemberCursor<'audit> {
    candidate: RootCandidate<'audit>,
    observed_count: u64,
    previous: Option<u64>,
    audited_child_index: usize,
    saw_current_root: bool,
    saw_selected_root: bool,
    rejected: bool,
}

struct CompleteMemberScan<'audit> {
    candidate: RootCandidate<'audit>,
}

/// Observe the selected physical guard only after complete member validation.
pub struct CompactRootGuardCursor<'audit> {
    members: CompleteMemberScan<'audit>,
}

/// A borrowed logical dentry after the provider validates its physical fields.
///
/// The provider checks raw column type/order/signedness, required non-NULL
/// values, binary header/length bounds, full packet consumption, UTF8, name
/// rules, and the raw 32-byte hash against SHA256 of the full raw name. These
/// checks grant no freshness token. Core repeats logical bounds and exact full
/// name/inode/order comparison. No row borrow is retained between observations.
pub struct BorrowedCompactRootEntry<'row> {
    pub parent: u64,
    pub ordinal: u64,
    pub name: &'row str,
    pub inode: u64,
}

/// Compare every selected-parent dentry before retaining the original audit.
pub struct CompactRootDentryCursor<'audit> {
    members: CompleteMemberScan<'audit>,
    identity: PhysicalInodeIdentity,
    entry_count: u64,
    next_ordinal: u64,
    observed_count: u64,
    previous_ordinal: Option<u64>,
    audited_entry_index: usize,
    rejected: bool,
}

impl<'audit> CompactRootMemberCursor<'audit> {
    /// Begin only for a retained complete-root audit and valid fresh authority.
    ///
    /// The supplied backing must match the fresh authority. Valid fresh
    /// defaults, current root, next inode, unused members, and backing may differ
    /// from the old audit, preserving materialized-guard factory semantics.
    /// None directs the provider to its ordinary owned same-transaction path.
    pub fn begin(
        authority: &CompactAuthority,
        backing: ConcurrentBackingId,
        selected_inode: u64,
        expected: CompactInodeExpectation<'audit>,
    ) -> Option<Self> {
        let witness = expected.root?;
        if authority.backing != backing
            || authority.generation != expected.generation
            || selected_inode == 0
            || selected_inode != witness.anchor.root
            || expected.node.stats.ino != selected_inode
            || authority.validate().is_err()
        {
            return None;
        }
        Some(Self {
            candidate: RootCandidate {
                witness,
                fresh: FreshRootAuthority {
                    generation: authority.generation,
                    current_root: authority.root,
                    next_inode: authority.next_inode,
                    members_count: authority.members_count,
                },
                selected_inode,
            },
            observed_count: 0,
            previous: None,
            audited_child_index: 0,
            saw_current_root: false,
            saw_selected_root: false,
            rejected: false,
        })
    }

    /// Observe each actual member in strictly increasing physical inode order.
    ///
    /// A merge into the audit's sorted unique children checks every child with
    /// constant state. The logical root may contain hardlinks or entries whose
    /// order is unrelated to inode order. Fresh unused members remain allowed.
    pub fn observe_member(&mut self, actual_inode: u64) {
        if self.rejected {
            return;
        }
        let Some(count) = self.observed_count.checked_add(1) else {
            self.reject();
            return;
        };
        if actual_inode == 0
            || actual_inode >= self.candidate.fresh.next_inode
            || self
                .previous
                .is_some_and(|previous| previous >= actual_inode)
            || count > self.candidate.fresh.members_count
        {
            self.reject();
            return;
        }
        if let Some(&child) = self
            .candidate
            .witness
            .root_children
            .get(self.audited_child_index)
        {
            if child < actual_inode {
                // Sorted observation passed a required child: no later member
                // can recover it, including when the body contains hardlinks.
                self.reject();
                return;
            }
            if child == actual_inode {
                self.audited_child_index += 1;
            }
        }
        self.observed_count = count;
        self.previous = Some(actual_inode);
        self.saw_current_root |= actual_inode == self.candidate.fresh.current_root;
        self.saw_selected_root |= actual_inode == self.candidate.selected_inode;
    }

    /// Record a provider parse or physical check miss; rejection is sticky.
    /// The provider must still drain its result before falling back.
    pub fn reject(&mut self) {
        self.rejected = true;
    }

    /// Call only after the complete member result and its terminal state settle.
    ///
    /// A miss returns before selected guard interpretation, allowing the owned
    /// fallback to preserve membership-before-guard error precedence.
    pub fn finish_members(self) -> Option<CompactRootGuardCursor<'audit>> {
        let last = self.previous?;
        if self.rejected
            || self.observed_count != self.candidate.fresh.members_count
            || !self.saw_current_root
            || !self.saw_selected_root
            || self.audited_child_index != self.candidate.witness.root_children.len()
            || last == u64::MAX
            || last >= self.candidate.fresh.next_inode
        {
            return None;
        }
        Some(CompactRootGuardCursor {
            members: CompleteMemberScan {
                candidate: self.candidate,
            },
        })
    }
}

impl<'audit> CompactRootGuardCursor<'audit> {
    /// Compare the selected physical guard/header after complete members.
    ///
    /// The provider must retain its existing selected-row cardinality and owned
    /// guard/body decoding. Missing, malformed, duplicate, or unequal guards
    /// use ordinary same-transaction validation. Every Stats field and the
    /// logical count must match; valid ordinal holes/allocator drift may remain.
    pub fn observe_guard(
        self,
        actual_inode: u64,
        identity: PhysicalInodeIdentity,
        header: &CompactDirectoryHeader,
    ) -> Option<CompactRootDentryCursor<'audit>> {
        let candidate = &self.members.candidate;
        let NodeData::Directory { entries } = &candidate.witness.root.node.data else {
            return None;
        };
        if actual_inode != candidate.selected_inode
            || identity != candidate.witness.root.identity
            || identity
                .logical_version(candidate.fresh.generation)
                .is_err()
            || header.validate(actual_inode).is_err()
            || header.next_ordinal > i64::MAX as u64
            || header.stats != candidate.witness.root.node.stats
            || header.entry_count != u64::try_from(entries.len()).ok()?
        {
            return None;
        }
        Some(CompactRootDentryCursor {
            members: self.members,
            identity,
            entry_count: header.entry_count,
            next_ordinal: header.next_ordinal,
            observed_count: 0,
            previous_ordinal: None,
            audited_entry_index: 0,
            rejected: false,
        })
    }
}

impl CompactRootDentryCursor<'_> {
    /// Observe every selected-parent row in strictly increasing ordinal order.
    ///
    /// Compare full names immediately with the next original audited entry;
    /// retain no row borrow. Exact ordered equality inherits valid unique names
    /// from the complete audit. Every mismatch stays sticky for owned fallback.
    pub fn observe_entry(&mut self, actual: BorrowedCompactRootEntry<'_>) {
        if self.rejected {
            return;
        }
        let Some(count) = self.observed_count.checked_add(1) else {
            self.reject();
            return;
        };
        let candidate = &self.members.candidate;
        if actual.parent != candidate.selected_inode
            || actual.inode == 0
            || actual.inode >= candidate.fresh.next_inode
            || actual.ordinal >= self.next_ordinal
            || self
                .previous_ordinal
                .is_some_and(|previous| previous >= actual.ordinal)
            || count > self.entry_count
        {
            self.reject();
            return;
        }
        let NodeData::Directory { entries } = &candidate.witness.root.node.data else {
            self.reject();
            return;
        };
        let Some(expected) = entries.get(self.audited_entry_index) else {
            self.reject();
            return;
        };
        if actual.name != expected.name || actual.inode != expected.inode {
            self.reject();
            return;
        }
        self.observed_count = count;
        self.previous_ordinal = Some(actual.ordinal);
        self.audited_entry_index += 1;
    }

    /// Record a parse/physical miss without inventing a competing domain error.
    /// The provider keeps draining and preserves its owned validator's errors.
    pub fn reject(&mut self) {
        self.rejected = true;
    }

    /// Call only after every dentry and terminal driver state have been checked.
    ///
    /// Core cannot prove the absence of unread SQL rows. Full result completion
    /// is the transaction owner's obligation; cancellation or dropping an
    /// earlier stage never issues a receipt. The provider awaits explicit
    /// rollback before returning a successful checked or owned result.
    ///
    /// The private receipt retains the ORIGINAL audit's existing Arcs without
    /// cloning names, entries, membership, or constructing a fresh cached graph.
    pub fn finish_dentries(self) -> Option<CheckedCompactInode> {
        let candidate = self.members.candidate;
        let NodeData::Directory { entries } = &candidate.witness.root.node.data else {
            return None;
        };
        if self.rejected
            || self.observed_count != self.entry_count
            || self.audited_entry_index != entries.len()
        {
            return None;
        }
        Some(CheckedCompactInode {
            generation: candidate.fresh.generation,
            inode: candidate.selected_inode,
            identity: self.identity,
            root: Some(VerifiedCompactRoot {
                structure: candidate.witness.clone(),
            }),
        })
    }
}

#[cfg(all(test, not(kani)))]
mod tests;
