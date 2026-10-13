//! Lands: rebase a changeset onto a ref and move the ref.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use scion_core::{ChangesetId, RepoPath};

use crate::auth::Caller;
use crate::db::RepoId;
use crate::error::Result;
use crate::store::Store;

/// Where a land stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandState {
    /// Recorded, not finished.
    Pending,
    /// The ref points at the rebased changeset.
    Landed,
    /// A changed path also changed on the ref after the base.
    Conflict,
    /// The land can never succeed.
    Rejected,
}

/// A request to land a changeset.
#[derive(Clone, Debug)]
pub struct LandRequest {
    /// Repository name.
    pub repo: String,
    /// The changeset to land. Its first parent is its base.
    pub changeset: ChangesetId,
    /// The ref to move, for example `main`.
    pub target_ref: String,
    /// Caller-chosen key. A retry with the same key returns the same land.
    pub request_key: String,
}

/// The result of a land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandOutcome {
    /// Stable ID of this land.
    pub land_id: i64,
    /// Where it stands.
    pub state: LandState,
    /// The rebased changeset, when landed.
    pub result: Option<ChangesetId>,
    /// Why it conflicted or was rejected.
    pub detail: String,
    /// Paths that conflict.
    pub conflict_paths: Vec<RepoPath>,
}

/// One lock per (repo, ref).
type RefLocks = HashMap<(RepoId, String), Arc<Mutex<()>>>;

/// Serializes lands per ref.
#[derive(Default)]
pub struct Lands {
    locks: Mutex<RefLocks>,
}

impl Lands {
    /// Lands a changeset.
    ///
    /// # Errors
    ///
    /// Store errors. A conflict is an outcome, not an error.
    pub fn land(&self, store: &Store, caller: &Caller, req: &LandRequest) -> Result<LandOutcome> {
        let _ = (&self.locks, store, caller, req);
        todo!("S3")
    }
}

/// The current state of a land.
///
/// # Errors
///
/// [`crate::Error::NotFound`] if the repo has no such land.
pub fn get_land(store: &Store, repo: &str, land_id: i64) -> Result<LandOutcome> {
    let _ = (store, repo, land_id);
    todo!("S3")
}
