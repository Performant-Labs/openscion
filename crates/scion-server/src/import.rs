//! One-way import from a local git repository.

use std::path::Path;

use scion_core::ChangesetId;

use crate::error::Result;
use crate::store::Store;

/// What to import.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// Git revision to import, for example `HEAD` or `main`.
    pub rev: String,
    /// Scion ref to point at the imported tip.
    pub target_ref: String,
    /// How many non-root, non-merge commits to check against git, besides
    /// every root and merge.
    pub verify_sample: usize,
}

/// What an import did.
#[derive(Clone, Debug)]
pub struct ImportReport {
    /// Commits reachable from the revision.
    pub commits: usize,
    /// Commits newly stored (the rest were imported before).
    pub imported: usize,
    /// Commits whose Scion tree was checked against the git tree.
    pub verified: usize,
    /// The changeset for the imported revision.
    pub tip: ChangesetId,
}

/// Imports every commit reachable from `opts.rev` and points
/// `opts.target_ref` at the tip. Safe to repeat.
///
/// # Errors
///
/// Fails if git cannot be read, a path is not valid in Scion, or a verified
/// commit's tree differs from git's.
pub fn import_git(
    store: &Store,
    repo: &str,
    git_dir: &Path,
    opts: &ImportOptions,
) -> Result<ImportReport> {
    let _ = (store, repo, git_dir, opts);
    todo!("S1")
}
