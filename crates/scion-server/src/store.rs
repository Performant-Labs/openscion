//! Durable state: `SQLite` for metadata and trees, a folder of zstd files for
//! blobs.
//!
//! ```text
//! <data>/scion.db                  metadata and trees (see migrations/)
//! <data>/objects/blob/ab/cd..      one zstd file per blob, named by ID
//! <data>/tmp/                      staging for atomic blob writes
//! ```

use std::collections::BTreeSet;
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
        // Enough for every hot statement, so none is prepared twice.
        db.set_prepared_statement_cache_capacity(64);
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

    /// Stores one file's content durably. Safe to repeat.
    ///
    /// # Errors
    ///
    /// Fails if the content is larger than [`MAX_OBJECT_BYTES`] or the write fails.
    pub fn put(&self, raw: &[u8]) -> Result<BlobId> {
        let mut batch = self.batch();
        let id = batch.put(raw)?;
        batch.sync()?;
        Ok(id)
    }

    /// Starts a batch of writes that become durable together.
    pub fn batch(&self) -> BlobBatch<'_> {
        BlobBatch {
            blobs: self,
            dirs: BTreeSet::new(),
        }
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

/// Blobs written but not yet durable. Call [`BlobBatch::sync`] before
/// committing any metadata that names them.
///
/// Each blob is flushed to the drive when written (a plain `fsync`, cheap);
/// `sync` then flushes the directories that changed and issues one full flush
/// of the drive cache for the whole batch. On macOS a full flush
/// (`F_FULLFSYNC`) costs about 3 ms, so one per batch, not one per blob, is
/// what makes a large import fast.
#[must_use = "blobs in a batch are not durable until `sync`"]
pub struct BlobBatch<'a> {
    blobs: &'a Blobs,
    dirs: BTreeSet<PathBuf>,
}

impl BlobBatch<'_> {
    /// Writes one blob. Safe to repeat.
    ///
    /// # Errors
    ///
    /// Fails if the content is larger than [`MAX_OBJECT_BYTES`] or the write fails.
    pub fn put(&mut self, raw: &[u8]) -> Result<BlobId> {
        if raw.len() > MAX_OBJECT_BYTES {
            return Err(Error::InvalidArgument(format!(
                "blob is larger than {MAX_OBJECT_BYTES} bytes"
            )));
        }
        let id = BlobId::of(raw);
        let (dir, path) = self.blobs.location(id);
        // SHORTCUT: an existing file is trusted. After a power loss between a
        // write and its batch's `sync`, a torn file can remain; reads detect
        // it by hash. Upgrade: rewrite on a failed read.
        if path.exists() {
            return Ok(id);
        }
        let compressed = zstd::bulk::compress(raw, 3)?;
        // Write a temp file and flush it before the rename, so the name never
        // points at data that is not on the drive.
        let mut tmp = tempfile::NamedTempFile::new_in(&self.blobs.tmp)?;
        tmp.write_all(&compressed)?;
        fsync(tmp.as_file())?;
        fs::create_dir_all(&dir)?;
        tmp.persist(&path).map_err(|e| e.error)?;
        self.dirs.insert(dir);
        Ok(id)
    }

    /// Makes every blob written in this batch durable.
    ///
    /// # Errors
    ///
    /// Fails if a flush fails.
    pub fn sync(self) -> Result<()> {
        if self.dirs.is_empty() {
            return Ok(());
        }
        for dir in &self.dirs {
            fsync(&fs::File::open(dir)?)?;
        }
        fsync(&fs::File::open(&self.blobs.root)?)?;
        // One full flush of the drive cache covers everything above.
        fs::File::open(&self.blobs.root)?.sync_all()?;
        Ok(())
    }
}

/// A plain `fsync`: data reaches the drive, not necessarily past its cache.
/// (`File::sync_all` is a full cache flush on macOS, about 20x slower.)
fn fsync(file: &fs::File) -> std::io::Result<()> {
    rustix::fs::fsync(file).map_err(Into::into)
}
