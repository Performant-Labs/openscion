//! Lands: rebase a changeset onto a ref and move the ref.
//!
//! ```text
//! land(req)
//!   1. record the land as pending, or find it by (repo, caller, request key)
//!   2. take the ref's lock: one land at a time per ref
//!   3. if the row is no longer pending, return it (a retry, or a duplicate)
//!   4. check: one parent, base on the ref's first-parent history,
//!      no path changed on the ref after the base
//!   5. one transaction: apply to the head tree, store the rebased changeset,
//!      compare-and-swap the ref, mark the land landed
//! ```
//!
//! A crash at any point leaves either a pending row (a retry finishes it) or a
//! landed one (a retry returns it). The ref and the row move in the same
//! transaction, so they never disagree.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::{Connection, OptionalExtension, params};
use scion_core::{Changeset, ChangesetId, FileState, RepoPath};

use crate::auth::Caller;
use crate::db::{self, RepoId};
use crate::error::{Error, Result};
use crate::store::Store;
use crate::tree;

/// Longest accepted ref name or request key, in bytes.
const MAX_NAME_BYTES: usize = 256;

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

impl LandState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Landed => "landed",
            Self::Conflict => "conflict",
            Self::Rejected => "rejected",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(Self::Pending),
            "landed" => Ok(Self::Landed),
            "conflict" => Ok(Self::Conflict),
            "rejected" => Ok(Self::Rejected),
            other => Err(Error::Corrupt(format!("unknown land state {other:?}"))),
        }
    }
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

/// Per-ref state, guarded by the ref's lock.
type RefSlot = Arc<Mutex<Option<PathIndex>>>;

/// Serializes lands per ref and keeps each ref's [`PathIndex`].
#[derive(Default)]
pub struct Lands {
    refs: Mutex<HashMap<(RepoId, String), RefSlot>>,
}

impl Lands {
    /// Lands a changeset. A conflict or rejection is an outcome, not an error.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] if the repo or changeset does not exist.
    /// [`Error::InvalidArgument`] for a malformed request, or a request key
    /// reused for a different land. Store errors otherwise.
    pub fn land(&self, store: &Store, caller: &Caller, req: &LandRequest) -> Result<LandOutcome> {
        check_name("target_ref", &req.target_ref)?;
        check_name("request_key", &req.request_key)?;
        let (repo, land_id) = store.write(|c| record_pending(c, caller, req))?;

        // SHORTCUT: an in-process lock serializes lands per ref, so this works
        // with one server process. Upgrade: a single lander per ref that
        // rebases a batch and moves the ref once for all of them.
        let slot = self.slot(repo, &req.target_ref);
        let mut index = slot.lock().unwrap_or_else(PoisonError::into_inner);

        let row = store.read(|c| load(c, repo, land_id))?;
        if row.state != LandState::Pending {
            return Ok(row);
        }
        let result = finish(store, repo, land_id, req, &mut index);
        if result.is_err() {
            // The index may be ahead of the database; rebuild it next time.
            *index = None;
        }
        result
    }

    fn slot(&self, repo: RepoId, name: &str) -> RefSlot {
        let mut refs = self.refs.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(refs.entry((repo, name.to_owned())).or_default())
    }
}

/// The current state of a land.
///
/// # Errors
///
/// [`Error::NotFound`] if the repo has no such land.
pub fn get_land(store: &Store, repo: &str, land_id: i64) -> Result<LandOutcome> {
    store.read(|c| {
        let repo = db::repo_id(c, repo)?;
        load(c, repo, land_id)
    })
}

fn check_name(what: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_NAME_BYTES {
        return Err(Error::InvalidArgument(format!(
            "{what} must be 1 to {MAX_NAME_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Inserts a pending row, or finds the row for the same request key.
fn record_pending(c: &Connection, caller: &Caller, req: &LandRequest) -> Result<(RepoId, i64)> {
    let repo = db::repo_id(c, &req.repo)?;
    db::generation(c, repo, req.changeset)?;
    let existing = c
        .query_row(
            "SELECT land_id, input_cs, target_ref FROM lands
             WHERE repo_id = ?1 AND caller = ?2 AND request_key = ?3",
            params![repo.0, caller.0, req.request_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((land_id, input, target)) = existing {
        if input != req.changeset.as_bytes() || target != req.target_ref {
            return Err(Error::InvalidArgument(format!(
                "request key {:?} was already used for a different land",
                req.request_key
            )));
        }
        return Ok((repo, land_id));
    }
    c.execute(
        "INSERT INTO lands (repo_id, caller, request_key, target_ref, input_cs, state)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
        params![
            repo.0,
            caller.0,
            req.request_key,
            req.target_ref,
            req.changeset.as_bytes()
        ],
    )?;
    Ok((repo, c.last_insert_rowid()))
}

fn load(c: &Connection, repo: RepoId, land_id: i64) -> Result<LandOutcome> {
    let (state, result, detail, paths) = c
        .query_row(
            "SELECT state, result_cs, detail, conflict_paths FROM lands
             WHERE repo_id = ?1 AND land_id = ?2",
            params![repo.0, land_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("land {land_id}")))?;
    let result = result
        .map(|bytes| ChangesetId::try_from(bytes.as_slice()))
        .transpose()
        .map_err(|e| Error::Corrupt(format!("land {land_id} result: {e}")))?;
    Ok(LandOutcome {
        land_id,
        state: LandState::parse(&state)?,
        result,
        detail,
        conflict_paths: decode_paths(&paths)
            .ok_or_else(|| Error::Corrupt(format!("land {land_id} conflict paths")))?,
    })
}

/// Moves a pending land to its final state and returns the outcome.
fn set_state(
    c: &Connection,
    repo: RepoId,
    land_id: i64,
    state: LandState,
    result: Option<ChangesetId>,
    detail: String,
    conflict_paths: Vec<RepoPath>,
) -> Result<LandOutcome> {
    let updated = c
        .prepare_cached(
            "UPDATE lands SET state = ?3, result_cs = ?4, detail = ?5, conflict_paths = ?6
             WHERE repo_id = ?1 AND land_id = ?2 AND state = 'pending'",
        )?
        .execute(params![
            repo.0,
            land_id,
            state.as_str(),
            result.as_ref().map(ChangesetId::as_bytes),
            detail,
            encode_paths(&conflict_paths)
        ])?;
    if updated != 1 {
        return Err(Error::Corrupt(format!(
            "land {land_id} is no longer pending"
        )));
    }
    Ok(LandOutcome {
        land_id,
        state,
        result,
        detail,
        conflict_paths,
    })
}

/// What step 4 decided.
enum Plan {
    Reject(String),
    Conflict(String, Vec<RepoPath>),
    Land {
        rebased: Changeset,
        head: db::RefState,
    },
}

/// Runs steps 4 and 5 for a pending land. The caller holds the ref's lock.
fn finish(
    store: &Store,
    repo: RepoId,
    land_id: i64,
    req: &LandRequest,
    index: &mut Option<PathIndex>,
) -> Result<LandOutcome> {
    let target = &req.target_ref;
    let plan = store.read(|c| {
        let cs = db::get_changeset(c, repo, req.changeset)?;
        let [base] = cs.parents[..] else {
            return Ok(Plan::Reject(
                "a land needs a changeset with exactly one parent".into(),
            ));
        };
        let Some(head) = db::get_ref(c, repo, target)? else {
            return Ok(Plan::Reject(format!("ref {target:?} does not exist")));
        };
        let index = index.get_or_insert_with(|| PathIndex::new(head.target));
        index.advance_to(c, repo, head.target)?;
        let Some(base_gen) = index.reach(c, repo, base)? else {
            return Ok(Plan::Reject(format!(
                "base {base} is not on the first-parent history of {target:?}"
            )));
        };
        let conflicts = index.conflicts(&cs.changes, base_gen);
        if !conflicts.is_empty() {
            let detail = format!(
                "{} path(s) changed on {target:?} after the base",
                conflicts.len()
            );
            return Ok(Plan::Conflict(detail, conflicts));
        }
        let rebased = Changeset {
            parents: vec![head.target],
            ..cs
        };
        Ok(Plan::Land { rebased, head })
    })?;
    let (rebased, head) = match plan {
        Plan::Reject(detail) => {
            return store
                .write(|c| set_state(c, repo, land_id, LandState::Rejected, None, detail, vec![]));
        }
        Plan::Conflict(detail, paths) => {
            return store
                .write(|c| set_state(c, repo, land_id, LandState::Conflict, None, detail, paths));
        }
        Plan::Land { rebased, head } => (rebased, head),
    };

    let landed = store.write(|c| {
        let (id, _) = tree::store_changeset(c, repo, &rebased)?;
        if !db::cas_ref(c, repo, target, Some(head.version), id)? {
            return Err(Error::Corrupt(format!(
                "ref {target:?} moved while its land lock was held"
            )));
        }
        let generation = db::generation(c, repo, id)?;
        let outcome = set_state(
            c,
            repo,
            land_id,
            LandState::Landed,
            Some(id),
            String::new(),
            vec![],
        )?;
        Ok((outcome, generation))
    });
    match landed {
        Ok((outcome, generation)) => {
            if let (Some(index), Some(id)) = (index.as_mut(), outcome.result) {
                index.record(id, generation, &rebased.changes);
            }
            tracing::info!(land_id, id = ?outcome.result, target = %target, "landed");
            Ok(outcome)
        }
        // The path check passed, but the changes still clash with the head's
        // tree (for example a delete of a file the head never had). The
        // transaction rolled back; record the conflict.
        Err(Error::Apply(e)) => {
            let detail = format!("does not apply to {target:?}: {e}");
            store.write(|c| set_state(c, repo, land_id, LandState::Conflict, None, detail, vec![]))
        }
        Err(e) => Err(e),
    }
}

/// For one ref's first-parent history: the newest generation at which each
/// path, and each directory, last changed.
///
/// Generations strictly increase along first parents, so a change happened
/// after a base exactly when its generation is greater than the base's.
/// A conflict check then costs one lookup per path part, not a history walk.
///
/// The index covers the changesets after `floor` up to `tip`. It grows
/// forward as the ref moves and backward when a land has an older base.
// SHORTCUT: never pruned, so memory grows with the paths changed since the
// server started. Upgrade: drop entries older than the oldest base in use.
struct PathIndex {
    tip: ChangesetId,
    floor: ChangesetId,
    /// Changesets on the indexed history.
    chain: HashSet<ChangesetId>,
    /// File path → newest generation that put or deleted it.
    files: HashMap<Vec<u8>, i64>,
    /// Directory path → newest generation that changed anything below it.
    dirs: HashMap<Vec<u8>, i64>,
}

impl PathIndex {
    fn new(head: ChangesetId) -> Self {
        Self {
            tip: head,
            floor: head,
            chain: HashSet::new(),
            files: HashMap::new(),
            dirs: HashMap::new(),
        }
    }

    /// Indexes the changesets from the current tip forward to `head`. If
    /// `head` does not descend from the tip (the ref moved to other history),
    /// starts over at `head`.
    fn advance_to(&mut self, c: &Connection, repo: RepoId, head: ChangesetId) -> Result<()> {
        if head == self.tip {
            return Ok(());
        }
        let tip_gen = db::generation(c, repo, self.tip)?;
        let mut walked = Vec::new();
        let mut cur = Some(head);
        while let Some(id) = cur {
            if id == self.tip {
                for (id, generation, changes) in walked {
                    self.add(id, generation, &changes);
                }
                self.tip = head;
                return Ok(());
            }
            let generation = db::generation(c, repo, id)?;
            if generation <= tip_gen {
                break;
            }
            let cs = db::get_changeset(c, repo, id)?;
            cur = cs.parents.first().copied();
            walked.push((id, generation, cs.changes));
        }
        *self = Self::new(head);
        Ok(())
    }

    /// The generation of `base` if it is on the indexed history, extending
    /// the index backward if needed. `None` if `base` is not on the history.
    fn reach(&mut self, c: &Connection, repo: RepoId, base: ChangesetId) -> Result<Option<i64>> {
        let base_gen = db::generation(c, repo, base)?;
        if base == self.floor || self.chain.contains(&base) {
            return Ok(Some(base_gen));
        }
        let mut cur = self.floor;
        loop {
            let generation = db::generation(c, repo, cur)?;
            if generation <= base_gen {
                return Ok(None);
            }
            let cs = db::get_changeset(c, repo, cur)?;
            self.add(cur, generation, &cs.changes);
            match cs.parents.first() {
                Some(&parent) => {
                    self.floor = parent;
                    if parent == base {
                        return Ok(Some(base_gen));
                    }
                    cur = parent;
                }
                None => return Ok(None),
            }
        }
    }

    /// Paths in `changes` that touch a path changed after `base_gen`: the same
    /// path, a directory above it, or anything below it.
    fn conflicts(
        &self,
        changes: &BTreeMap<RepoPath, Option<FileState>>,
        base_gen: i64,
    ) -> Vec<RepoPath> {
        let after =
            |map: &HashMap<Vec<u8>, i64>, key: &[u8]| map.get(key).is_some_and(|&g| g > base_gen);
        changes
            .keys()
            .filter(|path| {
                let path = path.as_bytes();
                after(&self.files, path)
                    || after(&self.dirs, path)
                    || ancestors(path).any(|dir| after(&self.files, dir))
            })
            .cloned()
            .collect()
    }

    /// Records a changeset that just became the tip.
    fn record(
        &mut self,
        id: ChangesetId,
        generation: i64,
        changes: &BTreeMap<RepoPath, Option<FileState>>,
    ) {
        self.add(id, generation, changes);
        self.tip = id;
    }

    fn add(
        &mut self,
        id: ChangesetId,
        generation: i64,
        changes: &BTreeMap<RepoPath, Option<FileState>>,
    ) {
        self.chain.insert(id);
        for path in changes.keys() {
            let path = path.as_bytes();
            bump(&mut self.files, path, generation);
            for dir in ancestors(path) {
                bump(&mut self.dirs, dir, generation);
            }
        }
    }
}

fn bump(map: &mut HashMap<Vec<u8>, i64>, key: &[u8], generation: i64) {
    let slot = map.entry(key.to_vec()).or_insert(generation);
    *slot = (*slot).max(generation);
}

/// The proper directory prefixes of a path: `a/b/c` gives `a`, `a/b`.
fn ancestors(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.iter()
        .enumerate()
        .filter(|&(_, &b)| b == b'/')
        .map(move |(i, _)| &path[..i])
}

/// Paths joined by NUL, which a `RepoPath` cannot contain.
fn encode_paths(paths: &[RepoPath]) -> Vec<u8> {
    paths
        .iter()
        .map(RepoPath::as_bytes)
        .collect::<Vec<_>>()
        .join(&0)
}

fn decode_paths(bytes: &[u8]) -> Option<Vec<RepoPath>> {
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    bytes
        .split(|&b| b == 0)
        .map(|p| RepoPath::new(p).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use scion_core::{BlobId, IdentityTime};

    type Change = (RepoPath, Option<FileState>);
    type Edits<'a> = &'a [Change];

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        repo: RepoId,
        root: ChangesetId,
        lands: Lands,
    }

    fn author() -> IdentityTime {
        IdentityTime {
            name: b"t".to_vec(),
            email: b"t@x".to_vec(),
            unix_secs: 0,
            tz_offset_minutes: 0,
        }
    }

    fn put(path: &str, content: &str) -> Change {
        let state = FileState::Regular {
            content: BlobId::of(content.as_bytes()),
            executable: false,
        };
        (RepoPath::new(path).unwrap(), Some(state))
    }

    fn del(path: &str) -> Change {
        (RepoPath::new(path).unwrap(), None)
    }

    /// A repo whose `main` points at a root with `a/x`, `b/y` and `c`.
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let (repo, root) = store
            .write(|c| {
                let repo = db::create_repo(c, "r")?;
                let root = Changeset {
                    parents: vec![],
                    author: author(),
                    committer: None,
                    message: b"root".to_vec(),
                    changes: BTreeMap::from([put("a/x", "1"), put("b/y", "2"), put("c", "3")]),
                };
                let id = db::insert_changeset(c, repo, &root)?;
                assert!(db::cas_ref(c, repo, "main", None, id)?);
                Ok((repo, id))
            })
            .unwrap();
        Fixture {
            _dir: dir,
            store,
            repo,
            root,
            lands: Lands::default(),
        }
    }

    impl Fixture {
        fn commit(
            &self,
            base: ChangesetId,
            changes: &[(RepoPath, Option<FileState>)],
        ) -> ChangesetId {
            let cs = Changeset {
                parents: vec![base],
                author: author(),
                committer: None,
                message: format!("{changes:?}").into_bytes(),
                changes: changes.iter().cloned().collect(),
            };
            self.store
                .write(|c| db::insert_changeset(c, self.repo, &cs))
                .unwrap()
        }

        fn land(&self, cs: ChangesetId, key: &str) -> LandOutcome {
            self.try_land(cs, key).unwrap()
        }

        fn try_land(&self, cs: ChangesetId, key: &str) -> Result<LandOutcome> {
            let req = LandRequest {
                repo: "r".into(),
                changeset: cs,
                target_ref: "main".into(),
                request_key: key.into(),
            };
            self.lands.land(&self.store, &Caller("t".into()), &req)
        }

        fn main(&self) -> db::RefState {
            self.store
                .read(|c| db::get_ref(c, self.repo, "main"))
                .unwrap()
                .unwrap()
        }

        fn file(&self, cs: ChangesetId, path: &str) -> Option<scion_core::Entry> {
            let root = tree::root_tree(&self.store, self.repo, cs).unwrap();
            self.store
                .read(|c| tree::lookup(c, root, &RepoPath::new(path).unwrap()))
                .unwrap()
        }
    }

    #[test]
    fn lands_on_the_head_without_rebase() {
        let f = fixture();
        let cs = f.commit(f.root, &[put("a/x", "new")]);
        let out = f.land(cs, "k");
        assert_eq!((out.state, out.result), (LandState::Landed, Some(cs)));
        assert_eq!(f.main().target, cs);
        assert_eq!(f.main().version, 2);
    }

    #[test]
    fn rebases_disjoint_changes_from_the_same_base() {
        let f = fixture();
        let one = f.commit(f.root, &[put("a/x", "one")]);
        let two = f.commit(f.root, &[put("b/new", "two"), del("c")]);
        assert_eq!(f.land(one, "1").state, LandState::Landed);
        let out = f.land(two, "2");
        assert_eq!(out.state, LandState::Landed);
        let tip = out.result.unwrap();
        assert_ne!(tip, two, "rebased onto the new head");
        assert_eq!(f.main().target, tip);
        assert!(f.file(tip, "a/x").is_some() && f.file(tip, "b/new").is_some());
        assert!(f.file(tip, "c").is_none());
        let rebased = f.store.read(|c| db::get_changeset(c, f.repo, tip)).unwrap();
        assert_eq!(rebased.parents, vec![one]);
    }

    #[test]
    fn overlapping_paths_conflict_and_leave_the_ref_alone() {
        let cases: [(Edits<'_>, Edits<'_>, &str); 4] = [
            (&[put("a/x", "1")], &[put("a/x", "2")], "a/x"),
            (&[del("c")], &[put("c/d", "2")], "c/d"),
            (&[put("b/z/w", "1")], &[put("b/z", "2")], "b/z"),
            (&[put("b/y", "1")], &[del("b/y")], "b/y"),
        ];
        for (first, second, conflict) in cases {
            let f = fixture();
            assert_eq!(
                f.land(f.commit(f.root, first), "1").state,
                LandState::Landed
            );
            let before = f.main();
            let out = f.land(f.commit(f.root, second), "2");
            assert_eq!(out.state, LandState::Conflict, "{first:?} then {second:?}");
            assert_eq!(out.conflict_paths, vec![RepoPath::new(conflict).unwrap()]);
            assert_eq!(f.main(), before);
        }
    }

    #[test]
    fn retry_with_the_same_key_returns_the_same_land() {
        let f = fixture();
        let cs = f.commit(f.root, &[put("a/x", "new")]);
        let first = f.land(cs, "k");
        assert_eq!(f.land(cs, "k"), first);
        assert_eq!(f.main().version, 2, "the ref moved once");
        let other = f.commit(f.root, &[put("b/y", "other")]);
        assert!(matches!(
            f.try_land(other, "k"),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn a_crash_after_recording_resumes_on_retry() {
        let f = fixture();
        let cs = f.commit(f.root, &[put("a/x", "new")]);
        let req = LandRequest {
            repo: "r".into(),
            changeset: cs,
            target_ref: "main".into(),
            request_key: "k".into(),
        };
        // The first attempt records the land, then the server "crashes".
        let (_, land_id) = f
            .store
            .write(|c| record_pending(c, &Caller("t".into()), &req))
            .unwrap();
        assert_eq!(
            get_land(&f.store, "r", land_id).unwrap().state,
            LandState::Pending
        );
        assert_eq!(f.main().target, f.root);
        // A new process retries with the same key.
        let fresh = Lands::default();
        let out = fresh.land(&f.store, &Caller("t".into()), &req).unwrap();
        assert_eq!((out.land_id, out.state), (land_id, LandState::Landed));
        assert_eq!(f.main().version, 2);
    }

    #[test]
    fn rejects_merges_foreign_bases_and_missing_refs() {
        let f = fixture();
        let side = f.commit(f.root, &[put("s", "1")]);
        let merge = Changeset {
            parents: vec![f.root, side],
            author: author(),
            committer: None,
            message: vec![],
            changes: BTreeMap::new(),
        };
        let merge = f
            .store
            .write(|c| db::insert_changeset(c, f.repo, &merge))
            .unwrap();
        assert_eq!(f.land(merge, "m").state, LandState::Rejected);

        // `side` itself lands; a child of an older sibling of it does not have a
        // base on main.
        let sibling = f.commit(f.root, &[put("t", "1")]);
        let on_sibling = f.commit(sibling, &[put("u", "1")]);
        assert_eq!(f.land(side, "s").state, LandState::Landed);
        let out = f.land(on_sibling, "u");
        assert_eq!(out.state, LandState::Rejected, "{}", out.detail);

        let req = LandRequest {
            repo: "r".into(),
            changeset: side,
            target_ref: "nope".into(),
            request_key: "n".into(),
        };
        let out = f.lands.land(&f.store, &Caller("t".into()), &req).unwrap();
        assert_eq!(out.state, LandState::Rejected);
    }

    #[test]
    fn index_follows_a_ref_moved_outside_lands() {
        let f = fixture();
        assert_eq!(
            f.land(f.commit(f.root, &[put("a/x", "1")]), "1").state,
            LandState::Landed
        );
        // Something else (an import) moves main forward and touches c.
        let head = f.main();
        let external = f.commit(head.target, &[put("c", "external")]);
        assert!(
            f.store
                .write(|c| db::cas_ref(c, f.repo, "main", Some(head.version), external))
                .unwrap()
        );
        let out = f.land(f.commit(f.root, &[put("c", "mine")]), "2");
        assert_eq!(out.state, LandState::Conflict);
        let out = f.land(f.commit(f.root, &[put("b/y", "mine")]), "3");
        assert_eq!(out.state, LandState::Landed);
    }

    #[test]
    fn concurrent_lands_all_land_exactly_once() {
        let f = Arc::new(fixture());
        let threads: Vec<_> = (0..16)
            .map(|t| {
                let f = Arc::clone(&f);
                std::thread::spawn(move || {
                    (0..25)
                        .map(|i| {
                            let cs = f.commit(f.root, &[put(&format!("t{t}/f{i}"), "x")]);
                            f.land(cs, &format!("{t}-{i}"))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let outcomes: Vec<LandOutcome> = threads
            .into_iter()
            .flat_map(|t| t.join().unwrap())
            .collect();
        assert!(outcomes.iter().all(|o| o.state == LandState::Landed));
        assert_eq!(f.main().version, 1 + 400);
        // Every landed result is on main's first-parent history, once.
        let history: Vec<ChangesetId> = crate::read::log(&f.store, "r", f.main().target, 1000)
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(history.len(), 401);
        let mut results: Vec<_> = outcomes.iter().map(|o| o.result.unwrap()).collect();
        results.sort();
        let mut landed: Vec<_> = history[..400].to_vec();
        landed.sort();
        assert_eq!(results, landed);
        assert!(f.file(f.main().target, "t15/f24").is_some());
    }

    #[test]
    fn conflict_paths_round_trip() {
        let paths = vec![RepoPath::new("a").unwrap(), RepoPath::new("b/c").unwrap()];
        assert_eq!(decode_paths(&encode_paths(&paths)), Some(paths));
        assert_eq!(decode_paths(&[]), Some(vec![]));
        assert_eq!(decode_paths(b"a\0\0b"), None, "an empty path is not a path");
    }
}
