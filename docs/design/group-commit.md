# Group commit: many lands, one commit

Draft, on the way to v0.1.0's post-release list. Status: design note, no
PR, nothing built. The "Measured" section is placeholders until the lock
hold time lands there.

## The problem

One land holds the writer for its whole write. On SQLite that is the one
write connection's mutex, from `BEGIN IMMEDIATE` to the commit's end
(`Store::commit`, `crates/scion-server/src/store.rs:575`; the immediate
transaction at store.rs:587; the mutex at store.rs:636). On Postgres it
is the one writer client, from `BEGIN` to `COMMIT` (`Pg::commit`,
`crates/scion-server/src/pg.rs:965`; the single client from
`lock_writer`, pg.rs:1043). While land A runs its statements and waits
for its commit, land B on another ref waits for the writer to come back,
even though the two never touch the same rows.

The hold has two parts: the caller's work `w` (the statements: record,
derive results, tree build, CAS) and the commit `f` (the durable write:
SQLite's WAL fsync under `synchronous = FULL`, store.rs:336; Postgres's
WAL flush, which `synchronous_commit = on` asks for, pg.rs:13). Group
commit lets N waiting lands share one commit: one fsync on SQLite, one
commit round on Postgres, carrying all N.

## The path today

- `Lands::land` (land.rs:292) runs `land_committed` (land.rs:316), then
  flushes after the per-ref slot lock is released (land.rs:310): lands
  on one ref already share one flush.
- The land is two transactions: `record_land` commits first (land.rs:324;
  pending or conflict is decided there), then the plan loop takes the
  ref's slot (land.rs:332, `slot` at land.rs:375) and the real land
  commits (land.rs:701: `tree::store_changeset` plus the `cas_ref` move
  of the ref, guarded by the CAS predicate inside the same transaction).
- The per-ref slot serializes lands on one ref in-process
  (land.rs:330-332 SHORTCUT: one server process; a single lander per ref
  is the named upgrade).
- `Backend::commit` dispatches to the backend (backend.rs:500); `flush`
  (backend.rs:514) to the full flush that makes every earlier commit
  durable (store.rs:622). `writers_waiting` (backend.rs:291) counts
  writers stacked on the writer right now.

So the flush is already shared. What is not shared: the writer hold
itself. N lands on N refs cost N × (w + f) of writer time.

## The model

Little's law: steady-state throughput = concurrency / latency. One
writer, so concurrency is 1 and land throughput is 1 / (w + f). Batch B
lands into one commit and the writer serves the batch in B·w + f, so
throughput rises to B / (B·w + f), approaching 1/w as B grows. The fsync
disappears into the batch; the statements do not.

The gain is therefore bounded by the f/w ratio. If f is a tenth of w,
the ceiling on the writer's throughput gain is 1.1× — not worth the
machinery. If f dominates (slow drive, small fast statements), the
ceiling is B× with B waiting lands. This is why the decision waits on
numbers, not taste (see Measured).

The queue wait is not free: a land that joins a group waits for the
batch to fill (or the batch deadline) before its commit even starts.
Little's law again: latency_l = queue + w + f/B. Throughput goes up,
per-land latency can too. The design keeps the deadline semantics: the
land's deadline covers the queue wait, and cancel still answers.

## What must stay true

1. A land's reply goes out only after its own commit is durable. Today
   that is `write` = commit + flush (store.rs:563) and the land's flush
   after the slot (land.rs:310). Under group commit the land replies
   after its group's commit and flush, never before: durability of the
   group's fsync covers every land in it (the WAL's checksum chain makes
   a crash-recovered log a prefix of committed groups).
2. An error in one land fails only that land. A conflict, a CAS loss
   (`cas_ref` returning false, land.rs:703), a quota rejection: the
   other lands in the group commit. This rules out "one transaction for
   the whole batch" as-is: in SQLite one `BEGIN` carries all-or-nothing,
   and a mid-batch `Err` rolls back every land's work.
3. The CAS predicates read the newest state of the rows they guard, in
   the same transaction as the writes (the whole point of `cas_ref`).
   Lands on different refs touch disjoint rows, so one group transaction
   sees a consistent snapshot for all of them. Lands on the same ref are
   already serialized by the slot (land.rs:332), so a group holds at
   most one land per ref — no two lands in one group can race on a ref.
4. `record_land`'s pending path, the stale-land rejection (land.rs:270),
   and `set_land_state` keep their transaction boundaries: the group
   batches the land commits (the second transaction), not the bookkeeping
   that decided to attempt them.
5. The flush contract of `Store::flush` (store.rs:611-620) is unchanged:
   a caller that must reply durable flushes, and the flush covers every
   commit made before it. Group commit changes who batches, not what
   durability means.
6. A panic or deadline inside one land's `f` must not poison the group:
   savepoint isolation per land (below), and the group's writer lock is
   never held across a panic (the guard drop rolls back to the savepoint,
   the group continues).

## Options

### O1: savepoint batching inside one transaction (SQLite and Postgres)

The committer takes the writer, opens one transaction, and for each
queued land runs `f` inside a `SAVEPOINT land_i`. A land that finishes
cleanly releases its savepoint; one that errors rolls back to it and
reports only its own error. The group ends with one `COMMIT` (one WAL
fsync on SQLite; one commit round on Postgres) and one `flush`. Lands on
the same ref cannot meet in a group (invariant 3), so savepoint order is
land order, and each land's statements see the group's earlier lands'
writes — which are all on other refs.

- Postgres: `SAVEPOINT` is native; the advisory key (pg.rs:991) is taken
  once for the group, which also batches the collector's serialization.
- SQLite: `SAVEPOINT` is native; the immediate transaction (store.rs:587)
  is taken once per group instead of once per land.
- The batch boundary: `min(waiting lands, B_max)` or a deadline
  (`batch_wait`, e.g. 5-10 ms), whichever first. `writers_waiting`
  (backend.rs:291) is the natural fill signal: a committer that finds
  zero waiters commits immediately.

Cost: the writer-hold structure changes shape (a queue, a committer, a
reply channel per land), which touches `Store::commit`'s signature or
adds a `commit_group` beside it. Risk: the deadline and cancel paths
must unwind a queued land cleanly; a bug here turns a slow writer into a
stuck land.

### O2: batch at the land level, per ref (the SHORTCUT's single lander)

The named upgrade at land.rs:330-332: one lander task per ref takes the
whole pending queue of that ref, rebases the batch, moves the ref once
(`cas_ref` once for the batch's tip), and commits the changesets in one
transaction. Throughput per ref becomes batch-shaped instead of
land-shaped.

- Smallest blast radius: it lives inside `Lands`, not `Store`; the
  backend's `commit`/`flush` signatures stay.
- It does not help lands on different refs: the one writer still takes
  one commit per ref per batch. It shortens the per-ref path (one derive
  for N changesets, one ref move), which is a different win.
- Cross-ref contention on the writer remains; this option composes with
  O1 (the landers feed the group committer).

### O3: widen the writer (connection pool, synchronous_commit tuning)

Not group commit, but the lazy rung on the ladder: SQLite is one write
connection because one data directory takes one writer at a time — a
pool is not available there. On Postgres, `lock_writer` funnels every
writer through one client (pg.rs:1043); a small pool of writer clients
lets Postgres's own group commit (its WAL layer already batches
concurrent backends' fsyncs) work as designed, with no queue of ours.
The SQLite half keeps its hold; the pg half likely needs nothing else.
This is the option to price first if the Measured numbers say the hold
is mostly `f` on Postgres.

## The trigger to build it

Build when all three hold on measured numbers, not projections:

1. The fsync share: f / (w + f) above 0.3 at p50 on the target drive —
   below that, O1's ceiling is 1.4× and the queue risk buys nothing.
2. The wait is real: `writers_waiting` above zero for a meaningful share
   of a soak run (say 10% of samples) with lands queued on the writer
   from more than one ref.
3. Users feel it: land p99 wait-for-writer above the deadline budget's
   comfortable share (the deadline exists for a reason; a land that
   spends its budget waiting for the writer starves the derive).

O2 stands on its own trigger: when a single ref's pending queue grows
under one client (the retry path at land.rs:270 fires), because there
the batching win is derive work, not fsync.

## Measured

Placeholders — another coder is measuring the lock hold time; these fill
from that work:

- w, the statement time per land commit: p50 __ ms, p99 __ ms
- f, the commit's durable write: p50 __ ms, p99 __ ms (per backend,
  per drive class)
- the whole writer hold, w + f: p50 __ ms, p99 __ ms
- `writers_waiting` during the soak: share of samples above zero __ %,
  max depth __
- lands per second today (one ref, many refs): __ / __
- the f / (w + f) ratio, per backend: __

With those: the O1 ceiling is 1 / (1 - f/(w+f)) ×, and the decision
rules above do the rest.
