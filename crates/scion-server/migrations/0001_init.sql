-- Scion metadata schema, version 1.
-- Objects (blobs, trees) live in the object folder, not here.

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

CREATE TABLE lands (
    land_id      INTEGER PRIMARY KEY,
    repo_id      INTEGER NOT NULL REFERENCES repos (repo_id),
    caller       TEXT NOT NULL,
    request_key  TEXT NOT NULL,
    request_hash BLOB NOT NULL,
    target_ref   TEXT NOT NULL,
    input_cs     BLOB NOT NULL REFERENCES changesets (id),
    result_cs    BLOB REFERENCES changesets (id),
    state        TEXT NOT NULL CHECK (state IN ('pending', 'landed', 'conflict', 'rejected')),
    detail       TEXT,
    UNIQUE (repo_id, caller, request_key)
) STRICT;

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
