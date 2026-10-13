//! Durable state: `SQLite` for metadata and trees, a folder of zstd files for
//! blobs.
//!
//! ```text
//! <data>/scion.db                  metadata and trees (see migrations/)
//! <data>/objects/blob/ab/cd..      one zstd file per blob, named by ID
//! <data>/tmp/                      staging for atomic blob writes
//! ```

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, TransactionBehavior};
use scion_core::BlobId;

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
    blobs: Blobs,
}

impl Store {
    /// Opens or creates a data directory.
    ///
    /// # Errors
    ///
    /// Fails if the directory or database cannot be created, or the schema is
    /// newer than this build.
    pub fn open(dir: &Path) -> Result<Self> {
        let blobs = Blobs::open(dir)?;
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
                blobs,
            }),
        })
    }

    /// The blob folder.
    #[must_use]
    pub fn blobs(&self) -> &Blobs {
        &self.inner.blobs
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
    pub fn write<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
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

/// Immutable file contents, one zstd-compressed file per [`BlobId`].
pub struct Blobs {
    root: PathBuf,
    tmp: PathBuf,
}

impl Blobs {
    fn open(dir: &Path) -> Result<Self> {
        let root = dir.join("objects").join("blob");
        let tmp = dir.join("tmp");
        fs::create_dir_all(&root)?;
        fs::create_dir_all(&tmp)?;
        Ok(Self { root, tmp })
    }

    /// Stores a file's content durably. Safe to repeat.
    ///
    /// # Errors
    ///
    /// Fails if the content is larger than [`MAX_OBJECT_BYTES`] or the write fails.
    pub fn put(&self, raw: &[u8]) -> Result<BlobId> {
        if raw.len() > MAX_OBJECT_BYTES {
            return Err(Error::InvalidArgument(format!(
                "blob is larger than {MAX_OBJECT_BYTES} bytes"
            )));
        }
        let id = BlobId::of(raw);
        let (dir, path) = self.location(id);
        if path.exists() {
            return Ok(id);
        }
        let compressed = zstd::bulk::compress(raw, 3)?;
        // Write to a temp file, flush it to disk, then rename into place, so a
        // reader never sees a partial blob and no metadata can point at a
        // blob that a crash could lose.
        let mut tmp = tempfile::NamedTempFile::new_in(&self.tmp)?;
        tmp.write_all(&compressed)?;
        tmp.as_file().sync_all()?;
        fs::create_dir_all(dir)?;
        tmp.persist(&path).map_err(|e| e.error)?;
        Ok(id)
    }

    /// Reads a file's content and checks it against its ID.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] if absent, [`Error::Corrupt`] if it does not match.
    pub fn get(&self, id: BlobId) -> Result<Vec<u8>> {
        let file = match fs::File::open(self.location(id).1) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(Error::NotFound(format!("blob {id}")));
            }
            Err(e) => return Err(e.into()),
        };
        let mut raw = Vec::new();
        zstd::stream::read::Decoder::new(file)?
            .take(MAX_OBJECT_BYTES as u64 + 1)
            .read_to_end(&mut raw)?;
        if raw.len() > MAX_OBJECT_BYTES {
            return Err(Error::Corrupt(format!(
                "blob {id} unpacks to more than {MAX_OBJECT_BYTES} bytes"
            )));
        }
        if BlobId::of(&raw) != id {
            return Err(Error::Corrupt(format!("blob {id} does not match its ID")));
        }
        Ok(raw)
    }

    /// Whether a blob is stored.
    #[must_use]
    pub fn contains(&self, id: BlobId) -> bool {
        self.location(id).1.exists()
    }

    /// The fan-out directory and the file path of a blob.
    fn location(&self, id: BlobId) -> (PathBuf, PathBuf) {
        let hex = id.to_string();
        let dir = self.root.join(&hex[..2]);
        let path = dir.join(&hex[2..]);
        (dir, path)
    }
}
