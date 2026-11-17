-- Postgres schema, version 7: proposals and their review (#2 of
-- the Performant-Labs fork). A proposal names a changeset and the ref it
-- should land on; it moves no ref. `Land` on a protected ref looks a
-- proposal up by (repo, ref, head), and marks it merged in the
-- transaction that moves the ref.

CREATE TABLE proposals (
    repo_id     BIGINT NOT NULL REFERENCES repos (repo_id),
    number      BIGINT NOT NULL CHECK (number > 0),
    target_ref  TEXT NOT NULL,
    title       TEXT NOT NULL,
    description TEXT NOT NULL,
    author      TEXT NOT NULL,
    head_cs     BYTEA NOT NULL REFERENCES changesets (id),
    state       TEXT NOT NULL CHECK (state IN ('open', 'merged', 'closed')),
    merged_cs   BYTEA REFERENCES changesets (id),
    created_at  BIGINT NOT NULL,
    updated_at  BIGINT NOT NULL,
    PRIMARY KEY (repo_id, number),
    CHECK ((state = 'merged') = (merged_cs IS NOT NULL))
);

-- The land gate's lookup: the open proposal of a ref with a given head.
CREATE INDEX proposals_open_by_head ON proposals (repo_id, target_ref, head_cs) WHERE state = 'open';

-- Every head a proposal has had, from 1.
CREATE TABLE proposal_heads (
    repo_id   BIGINT NOT NULL,
    number    BIGINT NOT NULL,
    seq       BIGINT NOT NULL,
    cs        BYTEA NOT NULL REFERENCES changesets (id),
    pushed_by TEXT NOT NULL,
    at        BIGINT NOT NULL,
    PRIMARY KEY (repo_id, number, seq),
    FOREIGN KEY (repo_id, number) REFERENCES proposals (repo_id, number)
);

-- Every review of a proposal, from 1. A review is of one head.
CREATE TABLE proposal_reviews (
    repo_id  BIGINT NOT NULL,
    number   BIGINT NOT NULL,
    seq      BIGINT NOT NULL,
    reviewer TEXT NOT NULL,
    verdict  TEXT NOT NULL CHECK (verdict IN ('approve', 'request_changes', 'comment')),
    cs       BYTEA NOT NULL REFERENCES changesets (id),
    body     TEXT NOT NULL,
    at       BIGINT NOT NULL,
    PRIMARY KEY (repo_id, number, seq),
    FOREIGN KEY (repo_id, number) REFERENCES proposals (repo_id, number)
);

-- Review threads, from 1 per proposal. `path` is empty and `line` 0 for a
-- general thread. `old_side` is 1 when `line` counts in the base file.
CREATE TABLE review_threads (
    repo_id    BIGINT NOT NULL,
    number     BIGINT NOT NULL,
    thread     BIGINT NOT NULL,
    path       BYTEA NOT NULL,
    line       BIGINT NOT NULL CHECK (line >= 0),
    old_side   BIGINT NOT NULL CHECK (old_side IN (0, 1)),
    anchor_cs  BYTEA NOT NULL REFERENCES changesets (id),
    resolved   BIGINT NOT NULL CHECK (resolved IN (0, 1)),
    created_at BIGINT NOT NULL,
    PRIMARY KEY (repo_id, number, thread),
    FOREIGN KEY (repo_id, number) REFERENCES proposals (repo_id, number)
);

-- Comments, from 1 per proposal, each in one thread.
CREATE TABLE review_comments (
    repo_id BIGINT NOT NULL,
    number  BIGINT NOT NULL,
    comment BIGINT NOT NULL,
    thread  BIGINT NOT NULL,
    author  TEXT NOT NULL,
    body    TEXT NOT NULL,
    at      BIGINT NOT NULL,
    PRIMARY KEY (repo_id, number, comment),
    FOREIGN KEY (repo_id, number, thread) REFERENCES review_threads (repo_id, number, thread)
);
