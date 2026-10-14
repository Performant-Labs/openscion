-- Scion metadata schema, version 1.
-- Blobs live in the object folder. Trees live here, so a derivation that
-- writes many trees commits them in one transaction.

CREATE TABLE repos (
    repo_id INTEGER PRIMARY KEY,
    name    TEXT NOT NULL UNIQUE
) STRICT;

-- Changesets are global and content-addressed. `gen` is graph metadata
-- (1 + the highest parent gen) and is not part of the hash.
CREATE TABLE changesets (
    id   BLOB PRIMARY KEY CHECK (length(id) = 32),
    gen  INTEGER NOT NULL CHECK (gen >= 1),
    body BLOB NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE changeset_parents (
    cs     BLOB NOT NULL REFERENCES changesets (id),
    n      INTEGER NOT NULL CHECK (n >= 0),
    parent BLOB NOT NULL REFERENCES changesets (id),
    PRIMARY KEY (cs, n)
) STRICT, WITHOUT ROWID;

CREATE INDEX changeset_parents_by_parent ON changeset_parents (parent);

-- Which changesets a repository may read. Authorization goes through here.
CREATE TABLE repo_changesets (
    repo_id INTEGER NOT NULL REFERENCES repos (repo_id),
    cs      BLOB NOT NULL REFERENCES changesets (id),
    PRIMARY KEY (repo_id, cs)
) STRICT, WITHOUT ROWID;

-- A ref moves only by compare-and-swap on `version`.
CREATE TABLE refs (
    repo_id INTEGER NOT NULL,
    name    TEXT NOT NULL,
    target  BLOB NOT NULL,
    version INTEGER NOT NULL,
    PRIMARY KEY (repo_id, name),
    FOREIGN KEY (repo_id, target) REFERENCES repo_changesets (repo_id, cs)
) STRICT, WITHOUT ROWID;

-- One row per land request. A retry with the same (repo, caller, key) finds
-- this row. `conflict_paths` holds the paths joined by NUL bytes, which a
-- path cannot contain.
CREATE TABLE lands (
    land_id        INTEGER PRIMARY KEY,
    repo_id        INTEGER NOT NULL REFERENCES repos (repo_id),
    caller         TEXT NOT NULL,
    request_key    TEXT NOT NULL,
    target_ref     TEXT NOT NULL,
    input_cs       BLOB NOT NULL REFERENCES changesets (id),
    state          TEXT NOT NULL CHECK (state IN ('pending', 'landed', 'conflict', 'rejected')),
    result_cs      BLOB REFERENCES changesets (id),
    detail         TEXT NOT NULL DEFAULT '',
    conflict_paths BLOB NOT NULL DEFAULT x'',
    UNIQUE (repo_id, caller, request_key),
    CHECK ((state = 'landed') = (result_cs IS NOT NULL))
) STRICT;

-- TreeV1 objects (docs/format-v1.md), keyed by TreeId.
CREATE TABLE trees (
    id   BLOB PRIMARY KEY CHECK (length(id) = 32),
    body BLOB NOT NULL
) STRICT, WITHOUT ROWID;

-- Derived data: one value per (kind, version, changeset). Rebuildable.
CREATE TABLE derived (
    kind    TEXT NOT NULL,
    version INTEGER NOT NULL,
    cs      BLOB NOT NULL REFERENCES changesets (id),
    value   BLOB NOT NULL CHECK (length(value) = 32),
    PRIMARY KEY (kind, version, cs)
) STRICT, WITHOUT ROWID;

-- Git commits already imported, for incremental re-import.
CREATE TABLE git_import (
    repo_id  INTEGER NOT NULL REFERENCES repos (repo_id),
    git_algo INTEGER NOT NULL CHECK (git_algo IN (1, 2)),
    git_oid  BLOB NOT NULL,
    cs       BLOB NOT NULL REFERENCES changesets (id),
    PRIMARY KEY (repo_id, git_algo, git_oid)
) STRICT, WITHOUT ROWID;
