//! Writes: store blobs and create changesets. Neither moves a ref.

use scion_core::{BlobId, ChangesetId};

use crate::db;
use crate::error::Result;
use crate::store::Store;

/// Stores one file's content for later use in a changeset. Safe to repeat.
///
/// # Errors
///
/// [`crate::Error::NotFound`] if the repo does not exist.
pub fn put_blob(store: &Store, repo: &str, data: &[u8]) -> Result<BlobId> {
    store.read(|c| db::repo_id(c, repo))?;
    store.objects().put_blob(data)
}

/// Stores a changeset sent as `ChangesetV1` bytes. Safe to repeat: the ID is the
/// content.
///
/// # Errors
///
/// [`crate::Error::InvalidArgument`] unless the bytes decode, the changeset has
/// exactly one parent that the repo has, every blob exists, and the changes
/// apply to the parent's tree.
pub fn create_changeset(store: &Store, repo: &str, bytes: &[u8]) -> Result<ChangesetId> {
    let _ = (store, repo, bytes);
    todo!("S2")
}
