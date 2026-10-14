//! Writes: store blobs and create changesets. Neither moves a ref.

use rusqlite::Connection;
use scion_core::{BlobId, Changeset, ChangesetId, FileState, RepoPath};

use crate::db::{self, RepoId};
use crate::error::{Error, Result};
use crate::store::Store;
use crate::tree;

/// Most path changes in one created changeset.
pub const MAX_CHANGES: usize = 100_000;
/// Longest commit message in a created changeset, in bytes.
pub const MAX_COMMIT_MESSAGE_BYTES: usize = 1 << 20;
/// Longest author name or email in a created changeset, in bytes.
pub const MAX_IDENTITY_BYTES: usize = 1 << 10;

/// Stores one file's content for later use in a changeset. Safe to repeat.
///
/// # Errors
///
/// [`crate::Error::NotFound`] if the repo does not exist.
pub fn put_blob(store: &Store, repo: &str, data: &[u8]) -> Result<BlobId> {
    store.read(|c| db::repo_id(c, repo))?;
    store.blobs().put(data)
}

/// Stores a changeset sent as `ChangesetV1` bytes, with the root tree of its
/// parent plus its changes. Safe to repeat: the ID is the content.
///
/// # Errors
///
/// [`Error::NotFound`] if the repo does not exist. [`Error::InvalidArgument`]
/// unless the bytes decode, the changeset has exactly one parent that the repo
/// has, no committer, fits the request limits, and every blob it names is
/// stored. [`Error::Apply`] if the changes do not apply to the parent's tree.
pub fn create_changeset(store: &Store, repo: &str, bytes: &[u8]) -> Result<ChangesetId> {
    // SHORTCUT: limits are checked after a full decode, so one request can
    // still decode as many changes as fit in one gRPC message. Upgrade: a
    // decoder that stops at a count limit.
    let cs = Changeset::decode(bytes)
        .map_err(|e| Error::InvalidArgument(format!("bad changeset: {e}")))?;
    let parent = check_request(&cs)?;
    // The blob check touches only the object folder, so it runs before the
    // database lock is taken.
    if let Some((path, blob)) = first_missing_blob(store, &cs) {
        return Err(Error::InvalidArgument(format!(
            "blob {blob} for {path:?} is not stored; upload it with PutBlob first"
        )));
    }
    let id = cs.id();
    store.write(|c| {
        let repo_id = db::repo_id(c, repo)?;
        if has_changeset(c, repo_id, id)? {
            // A retry. Its tree was checked and recorded the first time.
            return Ok(id);
        }
        if !has_changeset(c, repo_id, parent)? {
            return Err(Error::InvalidArgument(format!(
                "parent {parent} is not in repo {repo:?}"
            )));
        }
        let base = tree::derive_root_tree(c, repo_id, parent)?;
        let root = tree::apply(c, Some(base), &cs.changes)?;
        db::insert_changeset(c, repo_id, &cs)?;
        tree::record_root_tree(c, id, root)?;
        Ok(id)
    })
}

/// Checks what a client may send, and returns the one parent.
fn check_request(cs: &Changeset) -> Result<ChangesetId> {
    let &[parent] = cs.parents.as_slice() else {
        return Err(Error::InvalidArgument(format!(
            "a changeset needs exactly one parent, got {}",
            cs.parents.len()
        )));
    };
    if cs.committer.is_some() {
        return Err(Error::InvalidArgument(
            "committer must be empty: only git import sets it".into(),
        ));
    }
    if cs.changes.len() > MAX_CHANGES {
        return Err(Error::InvalidArgument(format!(
            "more than {MAX_CHANGES} changes"
        )));
    }
    if cs.message.len() > MAX_COMMIT_MESSAGE_BYTES {
        return Err(Error::InvalidArgument(format!(
            "message is longer than {MAX_COMMIT_MESSAGE_BYTES} bytes"
        )));
    }
    if cs.author.name.len() > MAX_IDENTITY_BYTES || cs.author.email.len() > MAX_IDENTITY_BYTES {
        return Err(Error::InvalidArgument(format!(
            "author name or email is longer than {MAX_IDENTITY_BYTES} bytes"
        )));
    }
    Ok(parent)
}

/// Whether `repo` has changeset `id`.
fn has_changeset(c: &Connection, repo: RepoId, id: ChangesetId) -> Result<bool> {
    match db::generation(c, repo, id) {
        Ok(_) => Ok(true),
        Err(Error::NotFound(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// The first put, in path order, whose blob is not stored.
fn first_missing_blob<'a>(store: &Store, cs: &'a Changeset) -> Option<(&'a RepoPath, BlobId)> {
    cs.changes.iter().find_map(|(path, state)| match state {
        Some(FileState::Regular { content: blob, .. } | FileState::Symlink { target: blob })
            if !store.blobs().contains(*blob) =>
        {
            Some((path, *blob))
        }
        _ => None,
    })
}
