//! Metadata queries. Each takes a `&Connection`, so it runs the same inside or
//! outside a transaction (a `Transaction` derefs to a `Connection`).

use rusqlite::{Connection, OptionalExtension, params};
use scion_core::{Changeset, ChangesetId};

use crate::error::{Error, Result};

/// Row ID of a repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RepoId(pub i64);

/// Where a ref points, and the version to compare-and-swap against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefState {
    /// The changeset the ref points to.
    pub target: ChangesetId,
    /// Bumped on every move.
    pub version: i64,
}

/// Creates a repository, or returns the existing one with that name.
///
/// # Errors
///
/// Database errors.
pub fn create_repo(c: &Connection, name: &str) -> Result<RepoId> {
    c.execute("INSERT OR IGNORE INTO repos (name) VALUES (?1)", [name])?;
    repo_id(c, name)
}

/// Looks up a repository by name.
///
/// # Errors
///
/// [`Error::NotFound`] if no repository has that name.
pub fn repo_id(c: &Connection, name: &str) -> Result<RepoId> {
    c.query_row("SELECT repo_id FROM repos WHERE name = ?1", [name], |row| {
        row.get(0).map(RepoId)
    })
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("repo {name:?}")))
}

/// Stores a changeset and adds it to `repo`. Every parent must already be in
/// `repo`. Safe to repeat.
///
/// # Errors
///
/// [`Error::NotFound`] if a parent is not in `repo`.
pub fn insert_changeset(c: &Connection, repo: RepoId, cs: &Changeset) -> Result<ChangesetId> {
    let (body, id) = cs.encode_with_id();
    let mut parent_gen = 0;
    for parent in &cs.parents {
        parent_gen = parent_gen.max(generation(c, repo, *parent)?);
    }
    c.execute(
        "INSERT OR IGNORE INTO changesets (id, gen, body) VALUES (?1, ?2, ?3)",
        params![id.as_bytes(), parent_gen + 1, body],
    )?;
    for (n, parent) in (0_i64..).zip(&cs.parents) {
        c.execute(
            "INSERT OR IGNORE INTO changeset_parents (cs, n, parent) VALUES (?1, ?2, ?3)",
            params![id.as_bytes(), n, parent.as_bytes()],
        )?;
    }
    c.execute(
        "INSERT OR IGNORE INTO repo_changesets (repo_id, cs) VALUES (?1, ?2)",
        params![repo.0, id.as_bytes()],
    )?;
    Ok(id)
}

/// Reads a changeset that `repo` may see.
///
/// # Errors
///
/// [`Error::NotFound`] if `repo` does not have it.
pub fn get_changeset(c: &Connection, repo: RepoId, id: ChangesetId) -> Result<Changeset> {
    let body: Vec<u8> = c
        .query_row(
            "SELECT body FROM changesets JOIN repo_changesets ON cs = id
             WHERE repo_id = ?1 AND id = ?2",
            params![repo.0, id.as_bytes()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("changeset {id}")))?;
    Changeset::decode(&body).map_err(|e| Error::Corrupt(format!("changeset {id}: {e}")))
}

/// The generation of a changeset that `repo` may see: 1 for a root, else 1 +
/// the highest parent generation.
///
/// # Errors
///
/// [`Error::NotFound`] if `repo` does not have it.
pub fn generation(c: &Connection, repo: RepoId, id: ChangesetId) -> Result<i64> {
    c.query_row(
        "SELECT gen FROM changesets JOIN repo_changesets ON cs = id
         WHERE repo_id = ?1 AND id = ?2",
        params![repo.0, id.as_bytes()],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("changeset {id}")))
}

/// The first parent of a changeset, or `None` for a root.
///
/// # Errors
///
/// Database errors.
pub fn first_parent(c: &Connection, id: ChangesetId) -> Result<Option<ChangesetId>> {
    let parent: Option<Vec<u8>> = c
        .query_row(
            "SELECT parent FROM changeset_parents WHERE cs = ?1 AND n = 0",
            [id.as_bytes()],
            |row| row.get(0),
        )
        .optional()?;
    parent.map(|bytes| to_id(&bytes)).transpose()
}

/// Where a ref points, if it exists.
///
/// # Errors
///
/// Database errors.
pub fn get_ref(c: &Connection, repo: RepoId, name: &str) -> Result<Option<RefState>> {
    c.query_row(
        "SELECT target, version FROM refs WHERE repo_id = ?1 AND name = ?2",
        params![repo.0, name],
        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get(1)?)),
    )
    .optional()?
    .map(|(target, version)| {
        Ok(RefState {
            target: to_id(&target)?,
            version,
        })
    })
    .transpose()
}

/// Moves a ref if its version is still `expected`. `None` creates the ref
/// and expects it not to exist. Returns whether the ref moved.
///
/// # Errors
///
/// Database errors, or [`Error::NotFound`] if `repo` does not have `target`.
pub fn cas_ref(
    c: &Connection,
    repo: RepoId,
    name: &str,
    expected: Option<i64>,
    target: ChangesetId,
) -> Result<bool> {
    generation(c, repo, target)?;
    let changed = match expected {
        None => c.execute(
            "INSERT OR IGNORE INTO refs (repo_id, name, target, version) VALUES (?1, ?2, ?3, 1)",
            params![repo.0, name, target.as_bytes()],
        )?,
        Some(version) => c.execute(
            "UPDATE refs SET target = ?3, version = version + 1
             WHERE repo_id = ?1 AND name = ?2 AND version = ?4",
            params![repo.0, name, target.as_bytes(), version],
        )?,
    };
    Ok(changed == 1)
}

/// A derived value, if computed.
///
/// # Errors
///
/// Database errors.
pub fn get_derived(
    c: &Connection,
    kind: &str,
    version: u32,
    cs: ChangesetId,
) -> Result<Option<[u8; 32]>> {
    let value: Option<Vec<u8>> = c
        .query_row(
            "SELECT value FROM derived WHERE kind = ?1 AND version = ?2 AND cs = ?3",
            params![kind, version, cs.as_bytes()],
            |row| row.get(0),
        )
        .optional()?;
    value
        .map(|v| {
            <[u8; 32]>::try_from(v.as_slice())
                .map_err(|_| Error::Corrupt(format!("derived {kind} for {cs}")))
        })
        .transpose()
}

/// Records a derived value. A second write must agree with the first:
/// derivation is deterministic, so a mismatch means corruption.
///
/// # Errors
///
/// [`Error::Corrupt`] if a different value is already stored.
pub fn put_derived(
    c: &Connection,
    kind: &str,
    version: u32,
    cs: ChangesetId,
    value: [u8; 32],
) -> Result<()> {
    let inserted = c.execute(
        "INSERT OR IGNORE INTO derived (kind, version, cs, value) VALUES (?1, ?2, ?3, ?4)",
        params![kind, version, cs.as_bytes(), value],
    )?;
    if inserted == 0 && get_derived(c, kind, version, cs)? != Some(value) {
        return Err(Error::Corrupt(format!(
            "derived {kind} v{version} for {cs} differs from the stored value"
        )));
    }
    Ok(())
}

fn to_id(bytes: &[u8]) -> Result<ChangesetId> {
    ChangesetId::try_from(bytes).map_err(|e| Error::Corrupt(format!("stored changeset ID: {e}")))
}
