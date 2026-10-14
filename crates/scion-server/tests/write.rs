//! `create_changeset`: validation, idempotency under concurrency, and the
//! root tree it records.

use rusqlite::Connection;
use scion_core::{BlobId, Changeset, ChangesetId, Entry, FileState, IdentityTime, RepoPath};
use scion_server::error::ApplyError;
use scion_server::write::{
    MAX_CHANGES, MAX_COMMIT_MESSAGE_BYTES, MAX_IDENTITY_BYTES, create_changeset,
};
use scion_server::{Error, Store, db, tree};
use tempfile::TempDir;

const REPO: &str = "main";

/// A store with repo `main` (a root holding `a`, `dir/b`, `dir/c`, and a
/// `main` ref) and repo `other` (its own root).
struct Fixture {
    _dir: TempDir,
    store: Store,
    root: ChangesetId,
    other_root: ChangesetId,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for content in ["a", "b", "c", "new"] {
        store.blobs().put(content.as_bytes()).unwrap();
    }
    let (root, other_root) = store
        .write(|c| {
            let repo = db::create_repo(c, REPO)?;
            let root = db::insert_changeset(
                c,
                repo,
                &changeset(
                    vec![],
                    &[
                        ("a", Some(regular("a"))),
                        ("dir/b", Some(regular("b"))),
                        ("dir/c", Some(regular("c"))),
                    ],
                ),
            )?;
            assert!(db::cas_ref(c, repo, "main", None, root)?);
            let other = db::create_repo(c, "other")?;
            let other_root =
                db::insert_changeset(c, other, &changeset(vec![], &[("x", Some(regular("a")))]))?;
            Ok((root, other_root))
        })
        .unwrap();
    Fixture {
        _dir: dir,
        store,
        root,
        other_root,
    }
}

fn regular(content: &str) -> FileState {
    FileState::Regular {
        content: BlobId::of(content.as_bytes()),
        executable: false,
    }
}

fn path(p: &str) -> RepoPath {
    RepoPath::new(p).unwrap()
}

fn changeset(parents: Vec<ChangesetId>, changes: &[(&str, Option<FileState>)]) -> Changeset {
    Changeset {
        parents,
        author: IdentityTime {
            name: b"Ada".to_vec(),
            email: b"ada@example.com".to_vec(),
            unix_secs: 1_700_000_000,
            tz_offset_minutes: 0,
        },
        committer: None,
        message: b"change\n".to_vec(),
        changes: changes.iter().map(|(p, s)| (path(p), *s)).collect(),
    }
}

fn count_changesets(c: &Connection) -> scion_server::Result<i64> {
    Ok(c.query_row("SELECT count(*) FROM changesets", [], |row| row.get(0))?)
}

#[test]
fn concurrent_creates_of_one_changeset_store_it_once() {
    let f = fixture();
    let before = f.store.read(count_changesets).unwrap();
    let (bytes, expected) = changeset(
        vec![f.root],
        &[("a", None), ("dir/new", Some(regular("new")))],
    )
    .encode_with_id();
    let ids: Vec<ChangesetId> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..100)
            .map(|_| s.spawn(|| create_changeset(&f.store, REPO, &bytes)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap().unwrap())
            .collect()
    });
    assert!(ids.iter().all(|id| *id == expected), "{ids:?}");
    assert_eq!(f.store.read(count_changesets).unwrap(), before + 1);
}

#[test]
fn bad_requests_are_rejected_and_store_nothing() {
    let f = fixture();
    let before = f.store.read(count_changesets).unwrap();
    let ok = |edit: fn(&mut Changeset)| {
        let mut cs = changeset(vec![f.root], &[("dir/new", Some(regular("new")))]);
        edit(&mut cs);
        cs.encode()
    };
    let invalid = [
        ("undecodable", b"not a changeset".to_vec(), "bad changeset"),
        (
            "no parents",
            ok(|cs| cs.parents.clear()),
            "exactly one parent",
        ),
        (
            "two parents",
            ok(|cs| cs.parents.push(cs.parents[0])),
            "exactly one parent",
        ),
        (
            "parent in another repo",
            changeset(vec![f.other_root], &[("dir/new", Some(regular("new")))]).encode(),
            "not in repo",
        ),
        (
            "missing blob",
            ok(|cs| {
                cs.changes
                    .insert(path("dir/zzz"), Some(regular("never uploaded")));
            }),
            "is not stored",
        ),
        (
            "committer set",
            ok(|cs| cs.committer = Some(cs.author.clone())),
            "committer",
        ),
        (
            "too many changes",
            ok(|cs| {
                cs.changes = (0..=MAX_CHANGES)
                    .map(|i| (path(&format!("f{i}")), None))
                    .collect();
            }),
            "more than",
        ),
        (
            "message too long",
            ok(|cs| cs.message = vec![b'm'; MAX_COMMIT_MESSAGE_BYTES + 1]),
            "message",
        ),
        (
            "email too long",
            ok(|cs| cs.author.email = vec![b'e'; MAX_IDENTITY_BYTES + 1]),
            "author",
        ),
    ];
    for (case, bytes, fragment) in invalid {
        match create_changeset(&f.store, REPO, &bytes) {
            Err(Error::InvalidArgument(why)) => {
                assert!(why.contains(fragment), "{case}: {why}");
            }
            other => panic!("{case}: expected InvalidArgument, got {other:?}"),
        }
    }

    let unapplicable = [
        (
            ("a/b", Some(regular("new"))),
            ApplyError::FileInTheWay {
                path: path("a/b"),
                file: path("a"),
            },
        ),
        (("nope", None), ApplyError::DeleteMissing(path("nope"))),
    ];
    for (change, expected) in unapplicable {
        let bytes = changeset(vec![f.root], &[change]).encode();
        match create_changeset(&f.store, REPO, &bytes) {
            Err(Error::Apply(e)) => assert_eq!(e, expected),
            other => panic!("expected {expected:?}, got {other:?}"),
        }
    }

    let valid = changeset(vec![f.root], &[("dir/new", Some(regular("new")))]).encode();
    assert!(matches!(
        create_changeset(&f.store, "nope", &valid),
        Err(Error::NotFound(_))
    ));
    assert_eq!(f.store.read(count_changesets).unwrap(), before);
}

#[test]
fn created_changesets_have_the_expected_root_tree() {
    let f = fixture();
    let exec = FileState::Regular {
        content: BlobId::of(b"new"),
        executable: true,
    };
    let first = create_changeset(
        &f.store,
        REPO,
        &changeset(vec![f.root], &[("a", None), ("dir/new", Some(exec))]).encode(),
    )
    .unwrap();
    // A file becomes a directory, on top of a created changeset.
    let second = create_changeset(
        &f.store,
        REPO,
        &changeset(
            vec![first],
            &[("dir/b", None), ("dir/b/inner", Some(regular("b")))],
        )
        .encode(),
    )
    .unwrap();

    let repo = f.store.read(|c| db::repo_id(c, REPO)).unwrap();
    let lookup = |cs, p: &str| {
        let root = tree::root_tree(&f.store, repo, cs).unwrap();
        f.store.read(|c| tree::lookup(c, root, &path(p))).unwrap()
    };
    let file = |state| Some(Entry::File(state));
    assert_eq!(lookup(first, "a"), None);
    assert_eq!(lookup(first, "dir/b"), file(regular("b")));
    assert_eq!(lookup(first, "dir/c"), file(regular("c")));
    assert_eq!(lookup(first, "dir/new"), file(exec));
    assert_eq!(lookup(second, "dir/b/inner"), file(regular("b")));
    assert_eq!(lookup(second, "dir/new"), file(exec));
    assert_eq!(
        lookup(f.root, "a"),
        file(regular("a")),
        "the parent is unchanged"
    );
}
