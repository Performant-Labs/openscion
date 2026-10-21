# Design: history and writes for a principal with folder read rules

**Status: proposal. Nothing in this file is built.** It answers
[#183] — today a principal whose read rules cover only some folders
gets a checkout of its view ([#121]) and nothing else — `Log`,
`Blame`, `MergeBase`, `IsAncestor`, `GetChangeset`, `Diff` and every
write call answer UNIMPLEMENTED (`check_whole_reader`). This note
designs history first, writes second, and names what the owner must
pick. The one link here is the issue itself: [#183].

[#183]: https://github.com/bnsd55/openscion/issues/183

## 1. The projection: what a view changeset is

A **view changeset** is the changeset a partial reader sees in place
of a real one. It is an ordinary `Changeset` over view trees, stored
beside the real ones and content-addressed by its own ID:

- **Parents.** Each real parent maps to its view changeset ID. A real
  changeset with no view (all its paths hidden) contracts out of the
  graph: its view children take its mapped parents as their own, so
  the projected graph keeps the visible history's order and edges.
- **Body.** The changes list, restricted to paths the rule set covers,
  with each entry's tree replaced by the view tree ([#121]'s
  derivation, reused per directory). A changeset whose list comes out
  empty after restriction is an **all-hidden changeset** and
  contracts: it produces no view changeset at all.
- **Identity.** The view changeset ID hashes the projected body and
  the mapped parent IDs, with the rule-set digest folded into the
  hash domain (`scion\0view-changeset\0v1\0`), so two readers with
  different rules never agree on an ID, and one reader's IDs are
  stable across restarts and servers.

A worked example. Five real changesets on `main`, a reader whose rules
cover only `docs/`:

| real | touches | view |
|---|---|---|
| c1 | `docs/guide` | v1 |
| c2 | `src/main` (all hidden) | — contracts |
| c3 | `docs/api`, `src/lib` | v3, parents [v1] |
| c4 | `src/only` (all hidden) | — contracts |
| c5 | `docs/plan` | v5, parents [v3] |

The reader's history is v1 → v3 → v5. c2 and c4 do not exist; v3's
body lists only `docs/api`; every view changeset's message and author
are the constant ones of §2 (O1), so c3's real message never shows.

## 2. The leak channels, and what closes each

- **Commit metadata.** A real message or author names hidden paths or
  activity. Close: O1 or O2 below.
- **Graph shape.** Merge edges and contracting changesets reveal that
  hidden work happened. Close: the projection keeps only edges the
  mapped graph has after contraction, and merges whose both sides are
  hidden contract with their parents. A merge with one visible side
  keeps that side's edge and the merge shape (two parents) but not the
  hidden side's ID.
- **Counts and sizes.** The number of view changesets, their byte
  sizes and reply timing reveal activity between visible ones. Close:
  document that counts are the projected history's (the reader's
  history is its truth), and keep per-changeset sizes the projected
  bodies' — the alternative, padding to a constant, costs more than it
  buys for a v1 threat model.
- **Timing.** A rule-set change that re-projects on demand takes
  longer for repos with more hidden history, which reveals hidden
  size. Close: the background materialization of §4 answers from the
  cache at a steady cost once built, and while building, the read
  waits on the projection with a deadline, not a variable-time walk.
- **Ancestry.** `MergeBase`/`IsAncestor` over real IDs answer questions
  about hidden work. Close: they answer over the projected graph only;
  a real ID a partial reader sends that is not one of its view IDs
  errors as invalid, never as ancestry.

## 3. The two picks

- **Metadata for partly hidden changesets.**
  - **O1 — constant metadata (recommended).** Every view changeset
    carries the same author and a message that names the repo, the
    rule-set digest prefix and nothing else. Trade-off: readers lose
    who wrote what, in exchange for a leak story that is one sentence.
  - **O2 — repo-global declared metadata.** The repo's owner marks
    which fields are safe to show and the server shows them as they
    are. Trade-off: better history, but every message becomes a
    disclosure surface the owner must audit per repo.
- **The opaque write base.** A partial writer's changeset must name
  the exact real head it built on, without seeing it.
  - **O1 — an authenticated base token (recommended).** The server
    mints, at a read, a MAC over (repo, real head, rule-set digest,
    expiry); the writer sends it back with its upload receipts; the
    server re-derives the view from the token's head. Trade-off: one
    MAC key to keep and rotate; the head is exact and the token is
    useless for another reader or rule set.
  - **O2 — name the view head and resolve server-side.** The writer
    names its view head; the server resolves it to the real head by
    the rule-set digest. Trade-off: no key to keep, but the resolution
    needs the projected graph at hand and is ambiguous across
    materialization versions.

## 4. The cost, storage and invalidation

- **Work per new rule set.** The projection walks each real changeset
  once: map parents, restrict the body, hash. The tree part reuses
  [#121]'s `view_trees` rows; the changeset part is a linear pass.
  Back-of-envelope for 100,000 changesets: 100k rows read (~200 MB of
  bodies at 2 KB each), 100k restricted bodies hashed (~1 s of BLAKE3
  at 1 GB/s for the hashing, the reads dominate) — minutes cold,
  amortized to nothing for rule sets already materialized, and a new
  changeset projects only itself plus its changed folders.
- **Storage.** A `view_changesets` table keyed (repo, rule-set digest,
  projection version, real changeset ID): the view changeset's body
  and ID, its mapped parents, and whether it contracted. Same shape of
  lifecycle as `view_trees`: rows expire with their rule-set digests.
- **Resumption.** The projection is batched by generation order with a
  cursor row; a crashed run continues from the cursor. A read that
  needs an unbuilt history either waits (bounded) or answers from the
  built prefix and says so, per the call's deadline.
- **Invalidation.** A rule-set change hashes to a new digest, so the
  old projection is simply dead weight and sweeps away like
  `view_trees` rows do. A projection-version bump (a code change to
  the projection) invalidates the same way.

## 5. The slices after this design, each with its proving test

1. **Projected history, read-only.** `GetChangeset`, `Log`,
   `MergeBase`, `IsAncestor` over a materialized projection.
   Proving test: a partial reader's replies carry only its folders'
   paths and constant metadata; fuzz the projection against the
   access rules (no hidden path, message, author or merge edge in any
   reply), and a real ID that is not one of the reader's view IDs
   errors as invalid.
2. **Background materialization.** The batched, resumable projection
   and the steady-cost read. Proving test: a rule set's first read
   returns or times out at a bound that does not scale with the
   hidden history's size, and a crash mid-projection resumes from the
   cursor.
3. **Writes.** The base token, upload receipts, and `CreateChangeset`
   into a view. Proving test: a write with a token for another
   reader, an expired token, or a receipt for a hidden blob is
   refused; a good write lands and its view changeset reads back in
   slice 1's history.
4. **Blame and Diff.** Over the projected bodies only. Proving test:
   no hidden path appears in any part, and a diff between two view
   heads never names a hidden file.

The owner's picks (§3) gate slice 3; slices 1–2 stand under either
pick.
