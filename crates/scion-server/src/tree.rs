//! Snapshots: apply a changeset to a tree, look up paths, and derive the
//! root tree of any changeset.

use std::collections::BTreeMap;

use scion_core::{ChangesetId, Entry, FileState, PathElem, RepoPath, Tree, TreeId};

use crate::db::{self, RepoId};
use crate::error::{ApplyError, Error, Result};
use crate::store::{Objects, Store};

/// Derived-data kind for the root tree of a changeset.
const ROOT_TREE: &str = "root_tree";
const ROOT_TREE_VERSION: u32 = 1;

/// Applies `changes` to `base` (`None` is the empty tree), stores the new
/// trees, and returns the new root.
///
/// All deletes run before all puts, so a changeset can turn a file into a
/// directory or back. Directories left empty are removed.
///
/// # Errors
///
/// [`Error::Apply`] if a change does not fit the tree. Store errors otherwise.
pub fn apply(
    objects: &Objects,
    base: Option<TreeId>,
    changes: &BTreeMap<RepoPath, Option<FileState>>,
) -> Result<TreeId> {
    let mut root = Dir::Loaded(BTreeMap::new());
    if let Some(id) = base {
        root = Dir::Stored(id);
    }
    for (path, _) in changes.iter().filter(|(_, change)| change.is_none()) {
        root.delete(objects, path, &parts(path))?;
    }
    for (path, state) in changes {
        if let Some(state) = state {
            root.put(objects, path, &parts(path), 0, *state)?;
        }
    }
    root.store(objects)
}

/// Finds the entry at `path` under `root`.
///
/// # Errors
///
/// Store errors.
pub fn lookup(objects: &Objects, root: TreeId, path: &RepoPath) -> Result<Option<Entry>> {
    let mut entry = Entry::Dir(root);
    for name in parts(path) {
        let Entry::Dir(id) = entry else {
            return Ok(None);
        };
        match objects.get_tree(id)?.entries.remove(&name) {
            Some(next) => entry = next,
            None => return Ok(None),
        }
    }
    Ok(Some(entry))
}

/// The root tree of a changeset that `repo` may see, deriving it if needed.
///
/// Walks first parents back to the nearest changeset with a known root tree,
/// then applies changes forward, oldest first. No recursion, so a long
/// underived history is safe.
///
/// # Errors
///
/// [`Error::NotFound`] if `repo` does not have `cs`. [`Error::Corrupt`] if a
/// stored changeset does not apply to its parent tree.
///
/// # Panics
///
/// Never: the walk either finds `cs` already derived or derives it.
pub fn root_tree(store: &Store, repo: RepoId, cs: ChangesetId) -> Result<TreeId> {
    let (mut base, pending) = store.read(|c| {
        db::generation(c, repo, cs)?;
        let mut pending = Vec::new();
        let mut cur = Some(cs);
        while let Some(id) = cur {
            if let Some(tree) = db::get_derived(c, ROOT_TREE, ROOT_TREE_VERSION, id)? {
                return Ok((Some(TreeId::from_bytes(tree)), pending));
            }
            pending.push(id);
            cur = db::first_parent(c, id)?;
        }
        Ok((None, pending))
    })?;
    let mut derived = Vec::with_capacity(pending.len());
    for id in pending.into_iter().rev() {
        let changeset = store.read(|c| db::get_changeset(c, repo, id))?;
        let tree = apply(store.objects(), base, &changeset.changes).map_err(|e| match e {
            Error::Apply(e) => Error::Corrupt(format!("stored changeset {id} does not apply: {e}")),
            other => other,
        })?;
        derived.push((id, tree));
        base = Some(tree);
    }
    // One transaction for the whole chain: one fsync, not one per changeset.
    store.write(|c| {
        derived
            .iter()
            .try_for_each(|&(id, tree)| record_root_tree(c, id, tree))
    })?;
    Ok(base.expect("the walk ends at a derived tree or derives at least `cs`"))
}

/// Records the root tree of a changeset computed elsewhere (for example by a
/// land, which applies the changes to check them anyway).
///
/// # Errors
///
/// [`Error::Corrupt`] if a different tree is already recorded.
pub fn record_root_tree(c: &rusqlite::Connection, cs: ChangesetId, tree: TreeId) -> Result<()> {
    db::put_derived(c, ROOT_TREE, ROOT_TREE_VERSION, cs, *tree.as_bytes())
}

fn parts(path: &RepoPath) -> Vec<PathElem> {
    path.as_bytes()
        .split(|&b| b == b'/')
        .map(|part| PathElem::new(part).expect("a RepoPath part is a valid name"))
        .collect()
}

fn prefix(parts: &[PathElem]) -> RepoPath {
    let bytes: Vec<&[u8]> = parts.iter().map(PathElem::as_bytes).collect();
    RepoPath::new(bytes.join(&b'/')).expect("a prefix of a RepoPath is a RepoPath")
}

/// A directory being edited. Only directories on changed paths are loaded.
enum Dir {
    Stored(TreeId),
    Loaded(BTreeMap<PathElem, Node>),
}

enum Node {
    File(FileState),
    Dir(Dir),
}

impl Dir {
    fn entries(&mut self, objects: &Objects) -> Result<&mut BTreeMap<PathElem, Node>> {
        if let Self::Stored(id) = *self {
            let tree = objects.get_tree(id)?;
            let entries = tree
                .entries
                .into_iter()
                .map(|(name, entry)| {
                    let node = match entry {
                        Entry::File(state) => Node::File(state),
                        Entry::Dir(id) => Node::Dir(Self::Stored(id)),
                    };
                    (name, node)
                })
                .collect();
            *self = Self::Loaded(entries);
        }
        match self {
            Self::Loaded(entries) => Ok(entries),
            Self::Stored(_) => unreachable!("loaded above"),
        }
    }

    /// Deletes the file at `parts`. Returns whether this directory is now empty.
    fn delete(&mut self, objects: &Objects, path: &RepoPath, parts: &[PathElem]) -> Result<bool> {
        let entries = self.entries(objects)?;
        let (name, rest) = parts.split_first().expect("a path has at least one part");
        let remove = match (entries.get_mut(name), rest.is_empty()) {
            (Some(Node::File(_)), true) => true,
            (Some(Node::Dir(_)), true) => return Err(ApplyError::NotAFile(path.clone()).into()),
            (Some(Node::Dir(child)), false) => child.delete(objects, path, rest)?,
            (Some(Node::File(_)) | None, _) => {
                return Err(ApplyError::DeleteMissing(path.clone()).into());
            }
        };
        if remove {
            entries.remove(name);
        }
        Ok(entries.is_empty())
    }

    fn put(
        &mut self,
        objects: &Objects,
        path: &RepoPath,
        parts: &[PathElem],
        depth: usize,
        state: FileState,
    ) -> Result<()> {
        let entries = self.entries(objects)?;
        let name = &parts[depth];
        if depth + 1 == parts.len() {
            if matches!(entries.get(name), Some(Node::Dir(_))) {
                return Err(ApplyError::DirInTheWay(path.clone()).into());
            }
            entries.insert(name.clone(), Node::File(state));
            return Ok(());
        }
        let child = entries
            .entry(name.clone())
            .or_insert_with(|| Node::Dir(Self::Loaded(BTreeMap::new())));
        match child {
            Node::Dir(dir) => dir.put(objects, path, parts, depth + 1, state),
            Node::File(_) => Err(ApplyError::FileInTheWay {
                path: path.clone(),
                file: prefix(&parts[..=depth]),
            }
            .into()),
        }
    }

    /// Stores every loaded directory, bottom up, and returns this one's ID.
    fn store(self, objects: &Objects) -> Result<TreeId> {
        match self {
            Self::Stored(id) => Ok(id),
            Self::Loaded(entries) => {
                let mut tree = Tree::default();
                for (name, node) in entries {
                    let entry = match node {
                        Node::File(state) => Entry::File(state),
                        Node::Dir(dir) => Entry::Dir(dir.store(objects)?),
                    };
                    tree.entries.insert(name, entry);
                }
                objects.put_tree(&tree)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scion_core::BlobId;

    /// `(path, Some(content))` puts a regular file; `(path, None)` deletes.
    type Edits<'a> = &'a [(&'a str, Option<&'a str>)];

    fn regular(content: &str) -> FileState {
        FileState::Regular {
            content: BlobId::of(content.as_bytes()),
            executable: false,
        }
    }

    fn changes(edits: Edits<'_>) -> BTreeMap<RepoPath, Option<FileState>> {
        edits
            .iter()
            .map(|(p, content)| (RepoPath::new(*p).unwrap(), content.map(regular)))
            .collect()
    }

    fn objects() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn files(objects: &Objects, root: TreeId) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![(String::new(), root)];
        while let Some((prefix, id)) = stack.pop() {
            for (name, entry) in objects.get_tree(id).unwrap().entries {
                let path = format!("{prefix}{}", String::from_utf8_lossy(name.as_bytes()));
                match entry {
                    Entry::File(_) => out.push(path),
                    Entry::Dir(child) => stack.push((format!("{path}/"), child)),
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn puts_and_deletes_build_the_expected_tree() {
        let (_dir, store) = objects();
        let o = store.objects();
        let t1 = apply(
            o,
            None,
            &changes(&[("a/b/c", Some("1")), ("a/d", Some("2")), ("e", Some("3"))]),
        )
        .unwrap();
        assert_eq!(files(o, t1), ["a/b/c", "a/d", "e"]);
        let t2 = apply(
            o,
            Some(t1),
            &changes(&[("a/b/c", None), ("f/g", Some("4"))]),
        )
        .unwrap();
        assert_eq!(files(o, t2), ["a/d", "e", "f/g"], "empty a/b is pruned");
        assert_eq!(
            lookup(o, t2, &RepoPath::new("a/d").unwrap()).unwrap(),
            Some(Entry::File(regular("2")))
        );
        assert_eq!(lookup(o, t2, &RepoPath::new("a/b").unwrap()).unwrap(), None);
    }

    #[test]
    fn same_content_gives_same_tree_id() {
        let (_dir, store) = objects();
        let o = store.objects();
        let direct = apply(o, None, &changes(&[("x/y", Some("1"))])).unwrap();
        let t = apply(o, None, &changes(&[("x/y", Some("1")), ("z", Some("2"))])).unwrap();
        let via_delete = apply(o, Some(t), &changes(&[("z", None)])).unwrap();
        assert_eq!(direct, via_delete);
    }

    #[test]
    fn file_and_directory_swap_in_one_changeset() {
        let (_dir, store) = objects();
        let o = store.objects();
        let t = apply(o, None, &changes(&[("a", Some("1"))])).unwrap();
        let t = apply(o, Some(t), &changes(&[("a", None), ("a/b", Some("2"))])).unwrap();
        assert_eq!(files(o, t), ["a/b"]);
        let t = apply(o, Some(t), &changes(&[("a", Some("3")), ("a/b", None)])).unwrap();
        assert_eq!(files(o, t), ["a"]);
    }

    #[test]
    fn structural_conflicts_are_rejected() {
        let (_dir, store) = objects();
        let o = store.objects();
        let t = apply(o, None, &changes(&[("a", Some("1")), ("d/x", Some("2"))])).unwrap();
        let cases: [(Edits<'_>, ApplyError); 5] = [
            (
                &[("a/b", Some("x"))],
                ApplyError::FileInTheWay {
                    path: RepoPath::new("a/b").unwrap(),
                    file: RepoPath::new("a").unwrap(),
                },
            ),
            (
                &[("d", Some("x"))],
                ApplyError::DirInTheWay(RepoPath::new("d").unwrap()),
            ),
            (
                &[("nope", None)],
                ApplyError::DeleteMissing(RepoPath::new("nope").unwrap()),
            ),
            (
                &[("a/b", None)],
                ApplyError::DeleteMissing(RepoPath::new("a/b").unwrap()),
            ),
            (
                &[("d", None)],
                ApplyError::NotAFile(RepoPath::new("d").unwrap()),
            ),
        ];
        for (change, expected) in cases {
            match apply(o, Some(t), &changes(change)) {
                Err(Error::Apply(e)) => assert_eq!(e, expected),
                other => panic!("expected {expected:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn root_tree_derives_a_long_chain_and_caches_it() {
        let (_dir, store) = objects();
        let repo = store.write(|c| db::create_repo(c, "r")).unwrap();
        let author = scion_core::IdentityTime {
            name: b"a".to_vec(),
            email: b"a@x".to_vec(),
            unix_secs: 0,
            tz_offset_minutes: 0,
        };
        // One transaction for the setup; the derivation below is what is tested.
        let tip = store
            .write(|c| {
                let mut parent: Option<ChangesetId> = None;
                for i in 0..2000 {
                    let cs = scion_core::Changeset {
                        parents: parent.into_iter().collect(),
                        author: author.clone(),
                        committer: None,
                        message: vec![],
                        changes: changes(&[(&format!("f{}", i % 7), Some(&i.to_string()))]),
                    };
                    parent = Some(db::insert_changeset(c, repo, &cs)?);
                }
                Ok(parent.unwrap())
            })
            .unwrap();
        let tree = root_tree(&store, repo, tip).unwrap();
        assert_eq!(files(store.objects(), tree).len(), 7);
        assert_eq!(root_tree(&store, repo, tip).unwrap(), tree);
    }
}
