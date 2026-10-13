//! Durable state: `SQLite` for metadata, a folder of zstd files for objects.
//!
//! ```text
//! <data>/scion.db                     metadata (see migrations/)
//! <data>/objects/{blob,tree}/ab/cd..  one zstd file per object, named by ID
//! <data>/tmp/                         staging for atomic object writes
//! ```

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, Transaction, TransactionBehavior};
use scion_core::{BlobId, Tree, TreeId};

use crate::error::{Error, Result};

const SCHEMA: &str = include_str!("../migrations/0001_init.sql");
const SCHEMA_VERSION: u32 = 1;

/// Largest object accepted or read back, uncompressed.
pub const MAX_OBJECT_BYTES: usize = 1 << 30;

/// Handle to one data directory. Cheap to clone.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    // SHORTCUT: one connection behind a mutex, so reads wait for writes.
    // Upgrade: a pool of read-only connections (WAL allows parallel readers).
    db: Mutex<Connection>,
    objects: Objects,
}

impl Store {
    /// Opens or creates a data directory.
    ///
    /// # Errors
    ///
    /// Fails if the directory or database cannot be created, or the schema is
    /// newer than this build.
    pub fn open(dir: &Path) -> Result<Self> {
        let objects = Objects::open(dir)?;
        let db = Connection::open(dir.join("scion.db"))?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;
        migrate(&db)?;
        Ok(Self {
            inner: Arc::new(Inner {
                db: Mutex::new(db),
                objects,
            }),
        })
    }

    /// The object folder.
    #[must_use]
    pub fn objects(&self) -> &Objects {
        &self.inner.objects
    }

    /// Runs `f` on the database, outside any explicit transaction.
    ///
    /// # Errors
    ///
    /// Returns what `f` returns.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        f(&self.lock())
    }

    /// Runs `f` in one write transaction. Commits if `f` returns `Ok`, rolls
    /// back otherwise.
    ///
    /// # Errors
    ///
    /// Returns what `f` returns, or a database error from begin or commit.
    pub fn write<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut db = self.lock();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic mid-call drops its transaction, which rolls back, so the
        // connection is still usable.
        self.inner.db.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn migrate(db: &Connection) -> Result<()> {
    let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match version {
        0 => {
            db.execute_batch(&format!(
                "BEGIN; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
            ))?;
            Ok(())
        }
        SCHEMA_VERSION => Ok(()),
        newer => Err(Error::Corrupt(format!(
            "database schema version {newer} is newer than this build ({SCHEMA_VERSION})"
        ))),
    }
}

/// Immutable objects, one zstd-compressed file per ID.
pub struct Objects {
    root: PathBuf,
    tmp: PathBuf,
}

#[derive(Clone, Copy)]
enum Kind {
    Blob,
    Tree,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
        }
    }
}

impl Objects {
    fn open(dir: &Path) -> Result<Self> {
        let root = dir.join("objects");
        let tmp = dir.join("tmp");
        fs::create_dir_all(&root)?;
        fs::create_dir_all(&tmp)?;
        Ok(Self { root, tmp })
    }

    /// Stores a file's content. Safe to repeat.
    ///
    /// # Errors
    ///
    /// Fails if the content is larger than [`MAX_OBJECT_BYTES`] or the write fails.
    pub fn put_blob(&self, raw: &[u8]) -> Result<BlobId> {
        let id = BlobId::of(raw);
        self.put(Kind::Blob, &id.to_string(), raw)?;
        Ok(id)
    }

    /// Reads a file's content and checks it against its ID.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] if absent, [`Error::Corrupt`] if it does not match.
    pub fn get_blob(&self, id: BlobId) -> Result<Vec<u8>> {
        let raw = self.get(Kind::Blob, &id.to_string())?;
        if BlobId::of(&raw) != id {
            return Err(Error::Corrupt(format!("blob {id} does not match its ID")));
        }
        Ok(raw)
    }

    /// Whether a blob is stored.
    #[must_use]
    pub fn has_blob(&self, id: BlobId) -> bool {
        self.path(Kind::Blob, &id.to_string()).exists()
    }

    /// Stores a tree. Safe to repeat.
    ///
    /// # Errors
    ///
    /// Fails if the write fails.
    pub fn put_tree(&self, tree: &Tree) -> Result<TreeId> {
        let (bytes, id) = tree.encode_with_id();
        self.put(Kind::Tree, &id.to_string(), &bytes)?;
        Ok(id)
    }

    /// Reads a tree and checks it against its ID.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] if absent, [`Error::Corrupt`] if it does not decode
    /// or match.
    pub fn get_tree(&self, id: TreeId) -> Result<Tree> {
        let bytes = self.get(Kind::Tree, &id.to_string())?;
        let tree = Tree::decode(&bytes).map_err(|e| Error::Corrupt(format!("tree {id}: {e}")))?;
        if tree.id() != id {
            return Err(Error::Corrupt(format!("tree {id} does not match its ID")));
        }
        Ok(tree)
    }

    fn path(&self, kind: Kind, hex: &str) -> PathBuf {
        self.root.join(kind.dir()).join(&hex[..2]).join(&hex[2..])
    }

    fn put(&self, kind: Kind, hex: &str, raw: &[u8]) -> Result<()> {
        if raw.len() > MAX_OBJECT_BYTES {
            return Err(Error::InvalidArgument(format!(
                "object is larger than {MAX_OBJECT_BYTES} bytes"
            )));
        }
        let path = self.path(kind, hex);
        if path.exists() {
            return Ok(());
        }
        let compressed = zstd::bulk::compress(raw, 3)?;
        // Write to a temp file, flush it to disk, then rename into place, so a
        // reader never sees a partial object.
        let mut tmp = tempfile::NamedTempFile::new_in(&self.tmp)?;
        tmp.write_all(&compressed)?;
        tmp.as_file().sync_all()?;
        fs::create_dir_all(path.parent().expect("object path has a parent"))?;
        tmp.persist(&path).map_err(|e| e.error)?;
        Ok(())
    }

    fn get(&self, kind: Kind, hex: &str) -> Result<Vec<u8>> {
        let file = match fs::File::open(self.path(kind, hex)) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(Error::NotFound(format!("{} {hex}", kind.dir())));
            }
            Err(e) => return Err(e.into()),
        };
        let mut raw = Vec::new();
        zstd::stream::read::Decoder::new(file)?
            .take(MAX_OBJECT_BYTES as u64 + 1)
            .read_to_end(&mut raw)?;
        if raw.len() > MAX_OBJECT_BYTES {
            return Err(Error::Corrupt(format!(
                "{} {hex} unpacks to more than {MAX_OBJECT_BYTES} bytes",
                kind.dir()
            )));
        }
        Ok(raw)
    }
}
