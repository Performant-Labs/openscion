# Changelog

All notable changes to this project are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project will follow [Semantic Versioning](https://semver.org/) from its first
release.

## [Unreleased]

New entries go at the top of the `[v0.1.0] - unreleased` section below,
until that section gets its release date.

## [v0.1.0] - unreleased

The first release: the server, its cache tiers, and the `scn` client.

### Added

- [docs/ROADMAP.md](docs/ROADMAP.md), the owner's order of the work: the
  v0.1.1 distribution and portability gate, the v0.2.0 compatibility and
  recovery gate, the v0.3.0 single-node operational beta, and the work that
  stays out until those pass. The README links it.
- Two documents that say what the project stands behind:
  [docs/stability.md](docs/stability.md), one row per surface (the
  crates' APIs, the CLI and its `--json` output, protocol v1, object
  format v1, the SQLite and Postgres schemas, snapshot files, ticket
  bytes, and the `test-hooks` feature) saying what 0.x promises, what a
  patch or a minor release may change, and how to upgrade; and a
  "Supported platforms" section in the README naming the runner images
  and architectures CI tests (Ubuntu 24.04 x86-64, macOS 15 arm64),
  Postgres 17.11, and what is untested or unsupported (Windows,
  FreeBSD, a data directory on a network filesystem) (#312).
- A pull request adds a changelog row as a fragment,
  changes/<slug>.<section>.md, instead of editing CHANGELOG.md: the
  section is one of added, changed, fixed, security, the slug is the
  pull request's number or a short kebab name, and the file is plain
  text, wrapped at 76 columns. At a release,
  scripts/merge-changelog.sh <release> assembles the fragments into
  the changelog and deletes them, and CI refuses a pull request that
  edits CHANGELOG.md itself; only a release/ branch does (#314).
- A release smoke test for the release checklist (#315):
  `scripts/release-smoke.sh` builds the release binaries and walks the
  README quick start on a small git repository it makes — import,
  ticket-key, serve, `scn` reads (log, ls, cat, blame) and writes (commit,
  land, read back), `fsck` clean, and a shutdown by one signal that must
  exit 0. It runs the story once under umask 022 and once under 002, names
  the step that failed, and removes everything it made. A CI job runs it
  on Linux.
- The release smoke joins CI on macOS (#317): the release-smoke job
  runs on macos-15 as well as ubuntu-24.04, so the README quick start
  story — import, ticket-key, serve, `scn` reads and writes, `fsck`, and
  a one-signal shutdown — proves itself on every supported platform on
  every push and pull request, once under umask 022 and once under 002.
- A versioned logical archive: `scion-server export`, `verify`, and
  `restore`. An export writes a whole repository, or the whole store, to a
  `scion-archive-1` directory: one read snapshot holds every metadata
  section (changesets with their bodies and generations, refs with their
  versions, pending lands with their request keys, git provenance), and
  the blob files are copied byte for byte, which is safe beside a live
  server because a blob file is immutable and gc keeps a blob past its
  grace. `verify` checks an archive against docs/archive-format.md with
  the core codec, blake3, and zstd only: it re-hashes every blob file and
  changeset body, and checks that every ref, parent, and land input is
  backed. `restore` replays an archive into an EMPTY store: the blobs go
  through the store's own write path, so every object ID carries over;
  the changesets replay in generation order, so the store derives the
  same generations and rebuilds the trees; the refs go back with their
  versions; the pending lands are recorded again, so a retry with the
  same request key resumes. The e2e exports beside a landing writer,
  verifies, restores, finds fsck clean, and resolves the same main; and
  it proves verify fails: a corrupted blob byte and a ref past the
  changeset stream are both named.
- Readiness, liveness and a bounded drain, tested end to end. The health
  service splits: the service names (scion.v1.Scion, scion.v1.Objects) are
  readiness and answer NOT_SERVING from the start of a drain, while the
  empty name is liveness and stays SERVING while the process answers, so a
  supervisor does not kill a draining server. A stop drains with
  --drain-timeout-secs (default 600): the in-flight calls finish and the
  process exits 0; past the bound they are cut, the log names the count,
  and the process exits 1. The e2e stops the real binary under a parked
  upload and a land: an answered call is durable after the restart, a cut
  land stays pending and a retry with the same request key resumes it, and
  fsck is clean.
- The server records an audit log: an append-only JSON-lines file of
  authentication, writes, ref changes, lands and administrative
  changes, with no tokens and no contents.
  
  The file sits in the data directory (mode 0600, `--audit-file`
  relocates it, no disable flag) and one writer thread owns it: callers
  hand lines to a bounded channel, the writer drains the queue, writes
  and fsyncs once per drain, and wakes each caller when its own lines
  are durable. A state change records two lines: an `intent` line
  fsynced before the change, and a `result` line after it, joined by
  request ID; a read records one. If the audit append fails, the change
  is refused and readiness reports not-ready. Refused authentication is
  aggregated per peer per interval into one counted line, capped.
  
  Blob and tree digests are never logged (a digest is a dictionary key
  to contents), and neither are token bytes or token hashes. Rotation
  is by size, keeps a bounded number of old files, and names what it
  dropped in an admin line.
  
  Not covered yet: nothing - the events above are the whole of the
  issue's list.
- A backup and restore runbook for a server that keeps serving:
  the metadata snapshot first (VACUUM INTO, or pg_dump on Postgres), the
  blob files second, the store's UUID marker with the metadata; what the
  backup promises, why gc must stay out of it, and a test that restores
  a backup taken while writes land.
- The checkout benchmark runs in a workflow_dispatch-only workflow on a shared
  ubuntu-24.04 runner, and docs/benchmarks.md states the runner, the dataset
  and the commands, with the noise in the min and max.
- The OpenSSF Best Practices badge audit: docs/best-practices.md maps the
  passing and silver criteria (read live from bestpractices.dev) to the
  repository's evidence, one table per level, so the badge form can be
  filled from it. Every row says met, gap, or N/A with the file, workflow
  or doc section behind it; the gaps are listed smallest first, split into
  doc changes we can make and project facts only the owner can supply
  (governance and the second access path, the issue-history answers, the
  signed-release channels, the person claims). Nothing is submitted
  anywhere: the badge entry and the README badge are the owner's actions.
- The CI's workflow tokens now run least-privileged everywhere, and code
  scanning comes to the repository.
  
  - The soak workflow, the one workflow without an explicit token grant,
    now declares `permissions: contents: read` at the top level, like the
    other four already do; no job in any workflow needs more.
  - A new CodeQL workflow scans Rust and GitHub Actions workflows on
    pushes to main, on pull requests, and weekly. Rust analyzes in the
    CodeQL build mode `none` (no build), and the analysis job carries the
    single `security-events: write` grant it needs to upload its result.
- CI fails if the test suite leaves the work tree dirty: after every
  `cargo test` in the three test jobs, `git diff --exit-code` runs, and its
  failure names the files a test wrote outside its sandbox.
- A written compatibility policy: which component pairs work at versions
  N and N-1 (the previous release, including the previous minor), what
  "works" means per pair, and what a break needs in 0.x — a minor that
  changes a wire, ticket or snapshot contract serves or reads the old
  version beside the new one and drops it no earlier than the next
  minor, while the store's migrations stay forward-only.
- The statement-coverage gate: a coverage workflow (push to main and
  workflow_dispatch, never pull_request) that measures the workspace with
  cargo-llvm-cov v0.9.1, pinned by version and the release tarball's
  sha256, and fails when the merged report drops under 80 percent lines -
  the roadmap's statement bar, where a Rust statement is an executable
  line. Two measures accumulate into one report: the default backend and
  the Postgres backend, so pg.rs is in the number; the per-crate summary
  prints in the log. The exclusions are checked in (scripts/coverage-exclusions.txt): the
  file holds no patterns today, with the reasons - the generated proto
  code builds into OUT_DIR under target/ and the measure never counts
  it. The test seam is in the measure: a test build compiles the
  test-hooks feature (the crate's own dev-dependency names it), so the
  seam's ~900 lines count, accepted for now as a SHORTCUT whose upgrade
  path is in the file - move the seam's items into their own files and
  one pattern excludes them. A pattern that matches no tracked file
  fails the job, so a vacuous exclusion cannot hide in the file, and the
  report step proves its own filter: no excluded file may appear in its
  printed list.
- docs/operations.md's shell blocks cannot drift from --help: two docs_ops
  tests parse every command the guide writes for scion-server (split at its
  subcommand) and for scion-cache and scion-daemon, and fail when a flag the
  command names is not in that binary's help - the direction docs_flags.rs
  does not cover. A runbook section whose steps a test runs ends with a
  Proof: line naming the test; the test fails when a proof's file or
  function is gone, so renaming or deleting a proof breaks the guide loudly.
  The first five proofs name the tests that already run the key-rotation,
  backup-and-restore, upgrade, and gc steps. docs/drills/ gains the template
  a recorded drill follows, and RELEASING.md gains the line that runs the
  release's drills on the release candidate.
- The collector's operating metrics (#239): `scion-server gc
  --metrics-file PATH` writes the last finished run as a Prometheus
  textfile — the end time, the duration, and the rows and files it
  collected, each with a `dry_run` label — atomically, and only when the
  run succeeds, so a failed run leaves the old file whose timestamp goes
  stale. A long run logs its progress over the repos at most every 10 s.
  The architecture metric table and the operations guide name the file,
  the textfile collector's folder, and the stale-timestamp alert.
- ci.yml's macOS test job runs the same two clippy passes as the Linux
  lint job (lib+bins and all-targets, -D warnings) before the tests.
  The Linux job never compiles macOS-only code - the workspace's one
  unsafe FFI module among it - so those lines were never linted; the
  job's warm cache makes the added cost a couple of minutes, and no
  second macOS runner is booked. The one macOS-only warning this
  exposed (an unused mut in the ACL FFI walk, also fixed by #402) is
  fixed here so the new gate is green.
- Add a mount-reps workflow: on demand only, on macos-15, it builds the
  scn lib test binary once and runs the fork-gated mount tests the
  number of times the reps input gives (100 by default), stopping at
  the first failure and uploading that run's log. This is the repeated
  evidence that the fork gate (#309) fixed the macOS mount flake.
- A store of the previous schema version opens on a new build: the open
  migrates it forward and serves what it held. A test builds a version-1
  store of each backend in place (the SQLite schema of migration 0001, a
  Postgres schema of migrations/postgres/0001, each with a repo, a root
  changeset, its ref and two blob files this build wrote), opens it the
  normal way, and checks the migration level after the open, the ref,
  the changeset, both blobs byte for byte, a file read through the tree
  the upgrade rebuilt, and a clean fsck (#318).
- The toolchain floor is on the record: docs/stability.md states when it
  may rise (only in a minor release, named in the release notes, never
  above the stable toolchain of six months ago, and checked by CI);
  docs/RELEASING.md checks the pushed tag against the release commit
  before the notes are built; CONTRIBUTING.md says who decides and that a
  squash keeps the outside contributor as the commit's author, with the
  maintainer as the committer.
- The crates publish only after their public APIs are on the record:
  scripts/public-api.sh writes, for each of the five publishable crates,
  what cargo-public-api sees as the crate's public API, one file per
  crate under the folder the script takes as its argument. The step for
  the crates.io publish of #21 is to run it on the tagged commit and
  read the diff against the previous release's listings before anything
  publishes. The script installs cargo-public-api and the nightly
  toolchain it needs when they are missing, and prints one line per
  crate with the line count of its listing.
- The release pipeline ([.github/workflows/release.yml](.github/workflows/
  release.yml)): on a pushed `v*` tag it verifies the tag's SSH signature
  through GitHub's API (the workflow holds no key), builds every artifact
  — the four binaries, the source tarball, an SBOM per shipped binary's
  crate — twice per cell from `.github/release-cells.json`, fails on one
  differing byte between the two builds, attests each artifact and SBOM
  with GitHub's build-provenance and SBOM attestations, verifies every
  attestation, and publishes one immutable release whose notes come from
  the tagged `CHANGELOG.md`. A `workflow_dispatch` dry-run runs the whole
  pipeline but publishes nothing, so it is provable before the first tag.
  [docs/slsa.md](docs/slsa.md) is the honest Build L3 assessment;
  [docs/RELEASING.md](docs/RELEASING.md) lists the repository settings
  only the owner can turn on.
- The Scorecard run now enforces a pinned policy: floors are per check,
  not an aggregate score, in .github/scorecard-policy.yml. Ten checks
  carry a floor of 10 (Binary-Artifacts, CI-Tests, Dangerous-Workflow,
  Dependency-Update-Tool, License, Maintained, Pinned-Dependencies,
  Security-Policy, Token-Permissions, Vulnerabilities) and
  Branch-Protection a floor of 8. Every check the project does not meet
  yet - Signed-Releases, SBOM, Packaging, SAST, Fuzzing, Code-Review,
  Contributors, CII-Best-Practices - is a dated exception, each with an
  owner, a reason, and an expiry date; an expired exception fails the
  job like a floor breach.
  
  The job enforces the policy after each scan with
  scripts/check-scorecard-policy.py, against a second Scorecard pass in
  its JSON results format: the JSON lists every check with its score,
  so a check that Scorecard skipped or errored on fails the policy
  instead of hiding behind a missing SARIF entry, and a score of -1
  (Scorecard's "could not run") fails unless an unexpired exception
  covers it.
- The repository now runs the OpenSSF Scorecard itself, weekly and on
  every push to main, and publishes the result: the public Scorecard REST
  API (and any later badge) reads the score from these runs, and the
  SARIF lands in code scanning next to the CodeQL results.
  
  The analysis job carries the four grants the action's docs name —
  security-events write, id-token write, actions read, contents read —
  and nothing else; it checks out with no persisted credentials.
  
  Dependabot already covers the other half of the gap: cargo and
  github-actions updates are weekly and grouped, so the pinned action
  SHAs stay current.
- The support policy is written down in docs/support.md: the four
  severity levels (critical, high, medium, low, with one example from
  this project each), what each level gets (every fix on the current
  minor; critical and high security and data-loss fixes on the previous
  minor while its N-1 compatibility window is open, from v0.4), how a
  release announces the end of its support (from v0.4), and that 0.x has
  no long-term-support minor. SECURITY.md and README link to it.
- A threat model, docs/threat-model.md, replaces the one-line "one
  trusted machine" with the profile the code holds: one active server,
  its host and data directories trusted, the network and the token
  holders not. It names the assets, the actors, the six trust boundaries,
  and a security invariant for each promise the code makes, with the
  test that proves it and the claims that have no test yet.
- The version matrix (#239): a nightly job runs the pairs of the
  compatibility policy at versions N and N-1, building the tagged release
  behind HEAD, so a release that breaks a listed pair is caught by CI.
  The pairs: scn against the server, the cache tier against the server,
  scn against the daemon over its socket, each in both directions, and
  the store, which a newer server opens and migrates and an older one
  refuses. A dispatch can name any commit or tag as N-1, so a pair can be
  proven before the first release exists. Until a release is tagged the
  job says it skipped, in the run summary.
- A Postgres-backed store knows which database and which data directory
  belong together: the first open mints a store UUID, keeps it in the
  database and the data directory's `backend` marker, and an open that
  finds the two different is refused with both UUIDs named.
- A Postgres server that loses the one-server lock to another process is
  fenced: every read and write fails with the loss named, the health
  service answers `NOT_SERVING`, and the process exits after a short
  grace, so a supervisor restart either takes the lock back or refuses.
- `scion-server`, the server: changesets, trees and blobs in a data
  directory (SQLite metadata, one zstd file per blob), one-way git import
  checked against git, `fsck`, `gc`, `derive` and `hash-token` commands.
  The metadata may live in Postgres instead (`--postgres-url`); the blob
  files stay on disk in both backends (`#178`).
- Server-side writes: `PutBlob`, `CreateChangeset` (at most 100,000
  changes, one parent, no committer) and `Land`, which rebases onto a ref
  and moves it by compare-and-swap. Conflicts are path-level; there is no
  text merge. A retry with the same request key returns the stored
  outcome, so a cut-off call never lands the same change twice; a land
  whose ref moved is planned again, up to 3 times. A finished land is
  kept 7 days; one left `PENDING` longer than `--land-stale-secs` (a day)
  is rejected by a sweep at start and then hourly.
- Server-side commits and lands never show state that a power loss can
  undo: concurrent blob syncs and commits share one full drive flush, so
  a successful reply rests on a flushed drive (`#3`).
- One-way git import that can run beside a live server: it moves the ref
  by compare-and-swap, its commits are durable before the server sees
  them, and a re-import never moves a ref off later lands.
- `fsck` checks a data directory and reports one line per problem,
  read-only, safe beside a live server (exit 0 clean, 1 damage, 2 could
  not run). `gc` collects blobs and rows nothing names any more, past a
  grace period of `--grace-secs` (a week), rows before files, safe to
  repeat; `--dry-run` removes nothing. `derive --kind KIND --ref R`
  backfills derived data before the first query.
- The read API: `ResolveRef`, `GetChangeset`, `ReadFile`, `ListDir`,
  `Log` (with a path), `Blame`, `MergeBase`, `IsAncestor` and `Diff`.
  Streamed replies move in 1 MiB parts; a client that stops reading is
  cut off after 120 s.
- Path history is derived data: `Log` with a path returns the changesets
  that changed it, however far apart, and a level above 64 KiB is cut
  into chunks, so a directory that churns through names stays cheap
  (`#159`). `scn log --path` reads it.
- `Blame`: which changeset last changed each line, on first-parent
  history, across renames, on binary files too. All blame calls share a
  1 GiB budget; a file of more than 1,048,576 lines is blamed over
  several calls with a cursor.
- `Diff` finds renames as `git diff` does (same content, then same name
  at 75%, then the most similar at 50%), and names each rename's old
  path, new path and similarity.
- The query API runs on a derived commit-graph index, so `IsAncestor`
  reads a few dozen entries at any depth and `MergeBase` answers in a
  few dozen steps when the histories meet low. They are bounded only by
  the 600 s call limit, and at most 4 run at once.
- Read tickets (`#13`): an object ID alone is no permission. `GetRootTree`
  signs a 15-minute ticket for the repo, principal and root tree;
  `GetObjects` carries the ticket and each object's path, and the server
  and every tier walk the path, so nothing outside it is served. A tier
  refuses `ResolveRef` and `GetRootTree`: the server alone mints
  permissions.
- The metrics of the server and a cache on `--metrics-listen` (off by
  default, loopback only): request counts by RPC and code, a latency
  histogram, reply bytes by RPC, lands by outcome, land replans, view
  derivations, and a tier's cache hits and misses.
- The gRPC health service (`grpc.health.v1.Check` and `Watch`) on the
  serving port, without a token.
- Transport limits: at most 128 connections (`--max-connections`) of 256
  streams (`--max-streams`) per process, a stalled connection capped at
  64 MiB (`--max-stalled-bytes`), a connection closed after 30 min of age
  with a 10 min grace, and Nagle off.
- A caller's deadline passes down through the tiers with `grpc-timeout`,
  each hop keeping a margin, so a busy server's own status reaches the
  caller for any deadline (`#98`).
- Folder read rules (`#184`): a `read` rule may name path prefixes. A
  principal whose read rules are all prefixes reads the derived view of
  its prefixes, a hidden path answers like a missing one, and a partial
  reader cannot write.
- Principals and access rules (`#13`): `--access FILE` maps the BLAKE3
  digest of each token to a principal and its per-repo read and write
  rules; `scion-server hash-token` prints the digest an access file
  wants, so the file never holds a token.
- Per-principal quotas (`#15`): with `--access`, 64 requests in flight,
  512 MiB of upload bytes and 64 pending lands per principal by default,
  each overridable, a call over a cap failing with `RESOURCE_EXHAUSTED`.
- Read keys (`#13`, `#168`): `scion-server ticket-key --out FILE` makes
  the Ed25519 key that signs read tickets, and prints the public key for
  the tiers (`--ticket-public-key`, at most 2 during a rotation;
  `--ticket-key-old` moves the server to a new key).
- Bearer-token auth on every call, loopback-only listening by default,
  request limits and name validation at the RPC boundary.
- `scn`, the client: `cat`, `ls`, `log`, `blame`, `show`, `diff`,
  `merge-base`, `is-ancestor`, `commit`, `land`, `bench`, `checkout`,
  `mount` and `unmount`, with `--json` output. Every `scn` error exits 2,
  and `is-ancestor` answers with exit 0 or 1, so one never reads as the
  other.
- `scn checkout`, into a new or an existing empty directory: every entry
  is checked against its ID, a failure or signal removes what was
  written, and the checkout dies by the signal it caught (130 for
  Ctrl-C).
- `scn mount` and `scn unmount` (`#11`): one ref served as an NFSv3
  filesystem from `scn` itself, so a file loads when it is first read;
  `scn commit --from-mount DIR` commits exactly what the overlay holds.
  For single-user machines only: any local user who can read the mount
  table can reach the mount through the NFS port.
- `scn` reaches a `scion-daemon` over `unix:///PATH`, and checks the
  socket and every directory above it, as the daemon checks its own.
- `scion-tier`, the cache tiers: `scion-cache` keeps objects in memory
  and `scion-daemon` keeps them on disk, each least-recently-used out
  first under `--max-bytes` (1 GiB in memory, 10 GiB on disk). A cache
  listens on TCP, a daemon on a private Unix socket, one daemon per OS
  user. A tier answers only `GetObjects` and checks every object from
  upstream against its ID.
- Snapshots: `scion-cache --snapshot DIR --snapshot-repo R` writes
  `DIR/R.scnsnap` every `--snapshot-every` seconds, and a cold
  `scion-daemon --seed FILE` imports it, each object checked against its
  ID (`#59`).
- The daemon keeps its recency in `<data>/index.db`, so a clean stop
  reopens without reading every stored object, and a crash loses only
  the recency of the last batch, never an object.
- The gRPC health service (`grpc.health.v1`) on every process's serving
  port, without a token, so a probe needs none.
- Prometheus metrics on the server and the cache
  (`--metrics-listen`, off by default, loopback only): request counts by
  RPC and code, a latency histogram, reply bytes, lands by outcome, land
  replans, view derivations, and a tier's cache hits and misses (`#144`).
- One request ID per command, carried in `x-request-id` through every
  hop and logged as `request_id`, so one operation can be followed
  across scn, daemon, cache and server (`#144`).
- TLS and mTLS: the server and a cache serve TLS (`--tls-cert`,
  `--tls-key`) and can require a client certificate
  (`--tls-client-ca`); the tiers trust `--tls-ca` and can present a
  client certificate; `scn` trusts `--tls-ca`.
- `GetObjects` streams objects in parts of 1 MiB, the first part naming
  the object, its encoding and its sizes; a blob travels as its stored
  zstd frame, so it is packed once and unpacked once. A reply that has
  no memory for its first object within 30 s fails with
  `RESOURCE_EXHAUSTED`.
- One-way git import, safe to repeat and safe beside a live server: a
  re-import never moves a ref off later lands, and every root and merge
  commit is checked against git, plus `--verify` (100) others.
- Docs and scripts: the protocol contract (`docs/protocol-v1.md`), the
  operations guide (`docs/operations.md`), the CLI reference
  (`docs/cli.md`), the benchmark method (`docs/benchmarks.md`), the
  fault matrix (`docs/faults.md`, `#264`), `scripts/demo.sh`,
  `scripts/bench-checkout.sh`, a grpcurl conformance
  check (`#163`), a compose example with a server and a cache with TLS
  (`#164`), and a demo of the server on Postgres, with TLS to the
  database (`#195`).

### Changed

- Releasing is for a source release: `docs/RELEASING.md` cuts a release as
  a git tag plus GitHub release notes, with no crates.io publish until the
  crates' public APIs are narrowed (#21). The release PR's checklist
  merges the `changes/` fragments with `scripts/merge-changelog.sh`,
  builds a clean checkout through the README's `Try it` flow, runs the
  umask-002 CI job once #302 lands it, and states the provenance of every
  benchmark number: the commit, OS, filesystem, dataset, command and
  variance, and never the maintainer's machine. It tells how to undo a
  bad tag: redo the tag only while the release had no users, else mark it
  and cut the next patch version.
- scion-proto names its supported surface: `v1`, the wire constants, and
  `ticket` are the contract; everything else the five crates share —
  auth, check, deadline, listen, metrics, object, reply, request_id and
  tls — moves into one `scion_proto::runtime` module that is documented
  as unstable and not the published contract. Constants and helpers with
  no user outside the crate drop to `pub(crate)`, so the crates.io
  publish of #21 carries only what the wire needs.
- The server's Backend no longer says which database it holds: the
  public Sqlite and Postgres variants are gone, and the backend is an
  opaque struct with one private engine behind it, opened by the same
  engine-agnostic constructors as before. A caller matches behavior
  through the methods, never the engine, so a new engine (or a major
  bump of one) is a private change of the server crate, not a break of
  its API. The tests that build a backend from a store they prepared,
  or read the data directory's own store out of one, find both in
  test_support::backend (from_store, and BackendExt::sqlite), where the
  test seams live since the facade became the API.
- The checkout benchmark publishes its first results: one bench.yml run
  on main, the medians with min and max for the five states, and the
  ratios between the tools. A cold checkout through the daemon is 9.3 to
  22.1 times faster than git's clones and moves hundreds of times less;
  git's mirror clone stays 1.7 times faster than the warm daemon, and the
  numbers say why: the server side is idle, the gap is 50 fresh client
  processes.
- The unix-socket serving helper moved to scion_proto::runtime::listen,
  next to the TCP listener it shares the bounds with: scion-daemon and
  the tests that serve a daemon over its socket bind through the proto
  runtime, and scion-tier no longer carries serving plumbing in its
  public surface. The check the bind makes is unchanged: a real
  directory, owned by this user, closed to others, under parents no
  other user can change, and a dead socket file replaced.
- The capacity envelope of docs/operations.md (#387): a grid of writers,
  readers and backends run by scripts/capacity.sh on a GitHub-hosted
  runner, a dispatch job beside the checkout benchmark, and a doc test
  that holds the published table to the envelope's shape - the grid's
  every cell present in order, numbers that parse, no half-filled row,
  a footer that names the run's commit, and the edge in the caption.
- CI stops re-saving a rust-cache for every pull-request branch, which
  had grown to 66 caches and 10.6 GB - past GitHub's 10 GB ceiling, so
  caches were evicting each other and hits were shrinking. Caches save
  from main only now; a pull request restores main's. The three cargo
  test jobs also cache the workspace crates' own artifacts
  (cache-workspace-crates), so the postgres job stops rebuilding the
  bins the SQLite job just built: about five minutes off the job that
  finishes last in 13 of 20 runs. The fuzz smoke runs its ten targets
  four at a time (20 s each) instead of serially (15 s each), which cuts
  the job from ~11 minutes to ~7 on the runs where it sets the tail.
- CONTRIBUTING.md states the test policy the silver level asks for:
  major new functionality lands with tests in the automated suite, and a
  pull request that cannot test its change says why not in the
  description. The README states the interface's accessibility scope: a
  command-line tool and a service whose interface is text - commands,
  plain files, and log lines. best-practices.md closes the rows these
  close (test_policy_mandated, tests_documented_added,
  accessibility_best_practices) and the gap list shrinks to six.
- Waits bound by a call's deadline read what is left from one method,
  Deadline::remaining, instead of each call site computing it by hand;
  after the deadline it is zero, never a panic.
- Every server error that wraps another error returns it from
  `source()`. `Error::Git` stops discarding its cause: it carries the
  import's context and keeps the git library's own error as the source,
  which type is no longer part of the API; `Error::Apply` forwards to the
  apply error again, which `#[error(transparent)]` had dropped. The
  internal-error log line walks the whole source chain, and a test fails
  if a wrapping error's source goes missing.
- Store leaves the scion-server facade: only the test surfaces used it,
  and they read it from test_support now.
- README's Supported platforms section carries the macOS evidence: the
  mount and commit tests that fork passed 100 runs in a row on
  macos-15, run by the mount-reps workflow, after PR #309's fork gate.
- The scion-server modules are private except the fsck and gc
  operations: the crate's API is the facade in lib.rs, and the test
  seams sit in one test_support module behind the test-hooks feature.
- No production secret needs to appear in argv. The tokens and the
  Postgres URL take a checked-file form (`--token-file`,
  `--upstream-token-file`, `--postgres-url-file`, read the way the access
  file is: the owner alone may read it, a symlink is refused), the
  environment stays documented, `hash-token` reads stdin, and every argv
  form prints a deprecation warning naming its replacement. The scripts'
  throwaway tokens move to the environment.
- The storage engines come out of the server's public API. Error::Db,
  Error::Pg and Error::Busy fold into one Error::Storage, which carries a
  class (busy, out of space, or a failure of the engine's own) and keeps
  the engine's own error as the source, where logs and tests read it. A
  major bump of an embedded engine is then a private change, and callers
  match classes, not engine internals. The gRPC codes and reply texts of
  every class are unchanged.
- The runbook states what an acknowledged write has survived - the
  durability point: a blob put is answered after the file fsync, the
  rename, the directory fsyncs and the drive-cache flush; a land is
  answered after its blobs synced and its metadata commit was made
  durable (SQLite's `synchronous = FULL` WAL commit, Postgres's
  `synchronous_commit = on`, which the server checks at every open).
  
  Section 3 also states the recovery point of a backup (its metadata
  snapshot; a backup every N minutes loses about N minutes of
  acknowledged writes) and the restore time, measured: the new
  `restore time` CI job runs `restore_bench.rs` - a store of 20 repos,
  1000 changesets, about 32 MiB of raw blob content, restored and fscked
  - on a fixed `ubuntu-24.04` runner and prints the seconds, past a
  600 s bound.
  
  Section 1 says plainly that one active server has no automatic
  failover: a second server is refused (the lock is the anti-split-brain
  guard, not a failover), the operator restarts the server, and nothing
  takes over by itself.
- The project is now openscion, at github.com/bnsd55/openscion; the old
  URL redirects. The package names stay open-scion-*.
- The roadmap carries the whole line to v1.0: the production boundary at
  v0.4, durability at v0.5, the 1.x contracts at v0.6, operations at
  v0.7, releases and governance at v0.8, the release candidate at v0.9,
  and single-server production at v1.0. v1.0 supports one active server
  and no automatic failover; scale-out stays out. No crate publishes to
  crates.io before a real outside consumer asks for one. v0.1.0 is cut
  as one SSH-signed annotated tag and one immutable GitHub release.
- The scn library is the binary's pieces, not an API to build on: the
  facade in lib.rs is what the scn binary names (the client's Ctx and its
  repo-path parser, each command's run function and arguments), and
  everything else is crate-internal. The materialize and vfs modules are
  private, the mount session file's record moved out of the public
  surface into a crate-internal module, and the one test seam is a
  test_support module behind the test-hooks feature (default off), which
  holds the client preset, the session-file writer and the NFS
  filesystem that the integration tests drive. The encoded bytes, the
  commands and their output are unchanged.
- The three stale SHORTCUT markers now match main. The store's
  directory-flush marker says its upgrade shipped (the dm-log-writes
  suite runs in CI) and names the next one; the power suite's macOS
  marker says plainly that no macOS power-cut tool exists and what
  would lift it; the dead blob read-back helper is gone with its
  undefined-C5 marker, and the crash walk's marker states today's
  reason and trigger.
- The fault half of the soak: scripts/soak-fault.sh runs the soak's
  mixed load against one server and kills it on schedule - kill -9 under
  load, then a restart on the same data directory, one restart of the
  run taking the whole chain (cache and daemon with it). After every
  restart the run checks that the server lock is free, the server
  serves, a client read succeeds, fsck is clean beside the server, and
  every land the server acknowledged is still in the ref's history; the
  resource ceilings apply per restart segment. An optional Postgres
  restart ships behind SCION_FAULT_PG. The nightly gains a soak-fault
  job beside the soak, and the 72-hour run (#239, part 3) stands on it.
- The nightly soak runs 120 minutes instead of 10, so the latency trend's
  thirds mean something and a leak that a short run hides has two hours to
  show; the run stays bounded by the job's own timeout, and the soak's
  150-minute cap keeps a hung client inside it. The three resource
  ceilings are named in the workflow — 150 percent of the warm RSS, 100
  open files over the start, a late p99 under twice the early one — and
  soak.sh takes each from its environment, so a ceiling change is a diff
  in one file. The workflow no longer runs on pull requests: the nightly
  and a manual `workflow_dispatch` carry it, and its actions carry the
  same commit-SHA pins the main workflow got.
- The project's status label is alpha, not proof of concept: the README
  badge and note, SECURITY.md, and the architecture document's security
  limit now say alpha (0.x). The limit itself is unchanged: one trusted
  machine, never an untrusted network.
- The threat model tracks the code: #369 made the directory checks refuse
  every ACL beyond the mode (the sticky bit excuses no group write), so
  that invariant now names its tests instead of sitting in the no-test
  list. Two more claims got tests: a narrowed access file does not cut
  outstanding tickets until the ticket key rotates, and the partial
  reader's view keeps answering real changeset IDs, so a landing on
  hidden paths shows through the ref's ID over an unchanged view. The
  remaining no-test claims say why no test can exist.
- The workspace now denies unreachable `pub` items: `unreachable_pub = "warn"`
  lands in `[workspace.lints.rust]`, and every crate's `[lints]` inherits it.
  
  - Internal items that only the crate and its binaries use drop from `pub` to
    `pub(crate)`: the similar-object limits and helpers, the object/name/graph
    internals, the derived changeset helpers, the access constants, the metrics
    re-exports, and the test common modules' shared helpers.
  - Items the test seam re-exports (`test_support`) keep `pub` with an
    `#[allow(unreachable_pub)]` and a reason: the lint does not see through a
    re-export, and their external reachability is the seam itself.
  - `Metrics` is re-exported from `scion_proto` directly in lib.rs, since the
    `metrics` module is now crate-private.
- A second Ctrl-C or SIGTERM while the server drains ends it at once
  (`#294`): a drain waits for the calls in flight, and one held open by
  a stuck client left the operator with a server that answered nothing
  and ended only under `kill -9`. The first signal's graceful drain is
  as it was; the second cuts the calls in flight, logs that it did,
  and exits 1.
- A git import no longer re-reads and re-fsyncs blobs that a committed
  row of the target repo already names (#238): one row probe per
  distinct blob proves the stored copy durable, so a branch import over
  an existing history skips the read-backs and the drive-cache flushes
  of its unchanged files. Blobs without a row keep the full put path.
- `open-scion-server`'s public API is a small facade: `Backend`,
  `service::{serve, ServeOptions}`, `access::Access`, `tickets::Keys`,
  `quotas::Quotas`, `metrics::Metrics`,
  `import::{import_git, ImportOptions, ImportReport}`, `store::Store`,
  `error::{Error, Result}`, `fsck`, `gc` with `DEFAULT_GRACE_SECS`,
  `graph::{backfill_commit_graph, CommitGraph}`,
  `history::{backfill_path_history, PathHistory}` and
  `tree::{backfill_root_tree, RootTree}`. The modules behind it
  exist for the two binaries and can change without a release; a ref
  read goes through `Backend::resolve_ref`, where code that reached
  past the facade used to go.
- The server's test seams (`open_test`, the import's batch-boundary and
  busy-retry hooks, the commit-failure and busy-timeout seams) sit
  behind a `test-hooks` cargo feature, default off: a build without it
  carries none of them, and nothing behind the feature, and nothing
  `#[doc(hidden)]`, is API.
- `scn mount` now works with the system's NFS client (`#238`): the
  listener serves the export name its printed command mounts, where a
  real kernel client was refused with `MNT3ERR_NOENT`.
- `fsck` and `gc` run against a Postgres-backed store (`#180`), through
  the same metadata interface as on a data directory: both used to
  refuse it. The collector's batches on Postgres hold one advisory key
  per schema and at most 256 rows each, so a writer waits at most one
  batch behind the collector.
- `scn` errors say what to check next: one `check:` line under the error,
  keyed to the failure — a refused address, a certificate the client
  does not trust, a wrong token — and `scn mount` refuses a missing or
  non-directory mount point before it serves anything (#246).
- A partial reader's view is memoized per tree (`#121`): a new changeset
  re-derives only the folders on its changed paths, and rows of older
  projection versions or dead rule sets are swept in bounded steps.
- The server, the cache and the daemon log to stderr (`#179`): stdout
  carries only the output of `ticket-key` and `hash-token`.
- `Diff` finds renames over diffs of up to 1,000,000 entries (`#107`):
  exact renames run at any size in that range, where a diff of more than
  10,000 entries before had none. When the inexact search did not run or
  stopped at its budget, the reply says `renames_incomplete`, and `scn
  diff` prints git's warning about skipped inexact rename detection.
- The git import now documents what it verifies: the blobs it writes are
  read back and checked, and a blob it skips, because a committed row of
  the repo already names it and its file is present, is trusted - damage
  inside an already stored file is `fsck`'s job, and the import no longer
  claims to have caught it. A test pins the trust and fsck's report.
- A git commit with more changed paths than the write path's limit
  (100,000) is refused with an error that names the commit and the count,
  before the import builds the whole changeset in memory.

### Fixed

- The mount is read-only by default, and `scn commit --from-mount` refuses
  a read-only mount: a symlinked parent directory in the overlay can still
  redirect a write outside it, so the writes and the commit live behind
  `scn mount --unstable-writable` alone. The read-only mount answers every
  NFS write with `EROFS`, and the refusal names the flag.
- scion-core's IdentityTime, Changeset and Tree hide their fields
    behind getters and build only through checked constructors, so a
    caller cannot build a value the encoder cannot encode: a field or
    count above u32::MAX is refused at the boundary with a new
    EncodeError::FieldTooLong instead of panicking inside the encoder.
    The encoded bytes and the IDs are unchanged (#324).
- # The macOS deny-only ACL is read through, and the refusal names its entry (#399)
  
  ## Fixed
  
  The check for extended ACLs on macOS (PR #369) read only the *first* ACL
  entry: a descriptor carrying the ambient home-folder ACL —
  `group:everyone deny delete`, one entry — was refused as if it granted.
  Every macOS home folder carries that shape, so the server refused any
  data directory under `$HOME` (issue #399, a real Mac).
  
  The walk now reads **every** entry (`acl_get_entry`, first to last) and
  types each one (`acl_get_tag_type`):
  
  - every entry deny-only → **accepted**: a deny entry grants nobody
    anything, so the mode bits stay the whole story;
  - any allow entry, unknown tag, unreadable entry, or ACL that cannot be
    read or released → **refused**.
  
  The refusal names the entry that granted ("an allow entry for account
  501 grants access") and the platform's fix tool: macOS `chmod -N`,
  Linux `setfacl -b` (unchanged).
  
  Tests (macOS, each able to fail, proven red against the previous code
  first): a file under a `chmod +a "everyone deny delete"` directory is
  read; a file under a parent with a deny-only ACL passes
  `check_ancestors`; an `everyone allow read` entry is refused and the
  refusal names `chmod -N`.
  
  The claim of PR #369 — "deny-only ACLs pass" — is now true.
  Closes #399. Exposed by #345.
- Job starts are staggered 10 ms apart so a burst cannot overflow a server's
  accept queue, a run with SCION_KEEP_LOGS set keeps every server and batch
  log plus the nstat listen-overflow counters around every batch instead of
  losing them with the temp directory, and the stagger carries a SHORTCUT note
  naming its ceiling and its upgrade path.
- A call whose client went now answers cancelled, not deadline_exceeded.
  A job that outlives its call checks whether the call has gone: it stopped
  because the caller went, so the reply maps to Status::cancelled, and only
  a call that ran past its deadline reads deadline_exceeded. A land whose
  call went stays pending, and its counter says cancelled, not internal.
- The docs that walk an operator through making the files the server reads
  said nothing about the file modes the reads now check: the README's
  access-file section showed the format but not the mode, the README's and
  operations' TLS sections told the operator to make certificates with a
  tool and started the server on them, and the Postgres section named the
  CA file without its rule. An operator following them under umask 002 —
  a default Ubuntu account — wrote an access file or a certificate the
  server then refused at start. Each place now states the rule where the
  file is first made, with the fix: a secret (the access file, the TLS
  private keys) is read when its owner alone can read it, chmod 600; a
  file that must stay whole (the TLS certificate, the CAs of --tls-ca,
  --tls-client-ca and --postgres-ca) must not be group- or others-
  writable, chmod go-w, and a copy tool or a umask-002 session can leave
  0664 behind.
- The changelog merge script refused every section that held more than
  one fragment: its file-name check read the newlines between the paths
  of the fragment list as whitespace, so a release whose fragments share
  a section stopped at the merge. The check now runs on each fragment's
  own path, and the merge script's fixture test holds two fragments in
  one section, so the refusal cannot come back. Two changelog rows read
  wrong: the release-smoke rows carried backticks around their issue
  numbers and the older row said macOS stays out of CI for the job,
  which the newer row made untrue; the stability row and the policies
  row both introduced docs/stability.md, so the policies row now leads
  with the toolchain floor; the backup row dropped its commit-style
  prefix.
- A quota test no longer depends on the runner's TCP buffer sizes: its
  unread reply is 64 MiB, about 4x the pipeline a probe must lose to, so
  the slot is still held when the test asserts it.
- The README's claims now match the repo. The fuzz command makes the
  corpus directory that cargo-fuzz refuses to start without. The import
  and lands numbers link docs/benchmarks.md instead of quoting one
  machine's run. The CI list carries the two release smoke jobs and calls
  the disk-full and dm-log-writes runs what they are, steps of `test` (15
  checks, not 13). The contributing section points a private security
  report at SECURITY.md.
- A plain cargo build --release printed about 15 warnings in scion-server:
  unused imports, an unused batch index, and dead test helpers. The users
  of every one of them sit behind cfg(any(test, feature = "test-hooks"))
  while the items themselves did not, and CI never saw it, because the
  workspace clippy with --all-targets unifies the dev-dependency that
  turns the feature on. Each item now carries the same cfg as its users,
  with no allow attribute; the release build of the SyncStep payloads
  rides a unit-typed field instead. The fmt + clippy job gains
  cargo clippy --workspace --lib --bins -- -D warnings, which builds the
  libraries and binaries without dev-dependencies, so the next ungated
  test-only item fails CI instead of a release build's stderr.
- A dry run of the release walk fixed the guide where it read wrong: the
  milestone check passes while the release's own tracking issue is open,
  the unreleased note's fate on release day is stated, the version step
  says the first release checks the five versions equal instead of
  bumping them, the release notes name the command that extracts the
  version's changelog section alone, and the tag check runs locally
  before the push, not only on the remote after it.
- scn no longer panics when the reader of its stdout leaves: a pipeline
  like `scn blame main:README.md | head -1` closed the pipe at the first
  line, and the next print panicked with "failed printing to stdout".
  Every stdout write of scn now goes through one writer, and a broken
  pipe ends the command with status 0 and no message — a closed stdout
  is a normal end for a CLI, the reader chose to stop, and with
  `set -o pipefail` a 141 exit would fail every benign `| head`. Any
  other stdout error still fails the command. A test pipes `scn blame`
  into a reader that leaves after one line and asserts the quiet exit.
  The release smoke reads scn's output from files instead of pipes, so
  its checks no longer race against a reader that leaves mid-write.
- The soak scripts ran on a default Mac and failed at once: `timeout`
  does not exist there (Homebrew installs it as gtimeout) and nothing
  checked, so every client, import, gc and fsck call died with "timeout:
  command not found" and the writers landed 0 (issue #399, Run A). The
  scripts now resolve TIMEOUT once in common.sh - timeout, else
  gtimeout, else a named error - and call it everywhere, including the
  NFS mount script.
  
  The slow client never held a stream on any shell: its drain used
  `read -N`, which exists in neither dash (Ubuntu's /bin/sh) nor bash
  3.2 (macOS's /bin/sh), and the error vanished behind `|| true`. The
  drain is now dd, one 4 KiB block at a time with a pause between -
  portable across dash, bash 3.2 and Linux bash - shared by both soak
  scripts in common.sh, and each drain logs bytes read and seconds
  held. A drain that reads fewer bytes than the file has and does not
  end inside a kill window fails the run: the reader quit while the
  server was still serving. The old reader makes that check red, which
  is the proof it works.
- The nightly soak failed its own RSS bound on 2026-10-19 (run
  39219458903, the schedule event on 2bc9099): 34892 KB -> 54140 KB, over
  the 150 percent ceiling. At that commit the schedule branch of the
  workflow still fell back to the 10-minute default, so the nightly ran
  its planned 10 minutes and failed the check at the end, against a
  baseline taken after 60 seconds; the 120-minute schedule came later on
  main. There was no early abort: the only RSS check in the script runs
  after the sampling loop, and the fixed script's only RSS check needs
  the warm baseline, which exists only after SOAK_WARM_SECONDS. A 35-minute local run of the same load saw RSS climb for about
  30 minutes (32288 -> 59184 KB) and then hold flat for the last 16
  minutes (the last 20 samples: max 59184, average 58137, no trend), and
  the failed CI curve itself rose, stepped once at about 4 minutes, and
  then oscillated 49-57 KB for its last 5 minutes with no climb. That is
  warm-up - sqlite page caches of the 8 read connections and the write
  connection, allocator arenas, prepared statements - not a leak: a leak
  does not plateau on two runs.
  
  The bound's baseline is now the first sample after SOAK_WARM_SECONDS
  (1800 s, named in soak.yml): the nightly still checks 90 minutes of RSS
  after the warm-up. A run shorter than the window checks no RSS bound -
  it cannot tell warm-up from a leak (a 10-minute run, as the nightly of
  that commit was, sees only the climb). The other bounds are
  untouched.
  
  The samples now include the store's size on disk: every 10 s the loop
  appends epoch and `du -sk` of the server data directory to a
  metrics-store.csv, next to metrics.csv in the artifacts, so a run can
  be read for disk growth and for Ben's 72-hour soak decision, not only
  for the RSS it shares a cause with.
- A streamed reply that stalled on a client that stopped reading now gives
  its byte estimate back when its stream ends, instead of holding it until
  the connection closes. A connection that serves many stalled replies no
  longer ratchets toward its stalled-bytes close limit for bytes it no
  longer holds. The new scion_stalled_reply_bytes gauge shows what the
  stalled replies of the process hold now.
- The cache's serving line now names the address the OS bound, not the
  one the flag asked for: with `--listen 127.0.0.1:0` the log shows the
  real port, so a starter can read the line and connect, the way the
  release smoke parses the server's serving line.
- The cache and the daemon log the call's request ID on their GetObjects
  line, the way the server's call lines do: one command is followable
  through every hop's log, which the operations guide and the README
  promise. The reply runs outside the call, so it now stays in the call's
  span, as the server's replies do. A test sends one ID through a daemon
  and a cache and finds it in both logs and on the fetch upstream.
- The version matrix's dispatch could pass green without testing a pair:
  the workflow resolved the N-1 ref in one step, and the step that ran
  the matrix never received it, so the script resolved again, found no
  release tag, and printed its skip. The run step gets the resolved ref
  now, and the script fails when the resolve step left its ref file but
  the run received no ref, so a wiring slip can never be green again.
- A directory that the server, a cache tier or `scn` makes is mode 0700,
  whatever the umask (`#303`). On a default Ubuntu account (umask 002),
  the first command on a new data directory failed: the server refused
  the group-writable directory it had just made. A directory that
  already exists keeps its mode for the existing checks, and a
  group-writable parent that the user made is still refused (#302).
- The mount tests' serve-lock probes no longer race a parallel test's
  fork: a fork copies the process's open lock files to the child until
  the child execs, and macOS is slow to exec, so a parallel test's fork
  could hold a probe's lock for ticks and flip its answer
  (`an_unmount_does_not_signal_an_unrelated_process` failed twice this
  way). The tests that fork or read a serve lock now run one at a time
  under a gate, and each stand-in server they start runs in its own
  process group, killed and reaped whole on drop.
- A blame call whose wait for the memory budget ended as the bytes it
  needed came free is served, where it was refused before: the take
  rechecks the budget after the wait, and refuses only a budget that
  still does not hold the bytes. That refusal now answers
  `DEADLINE_EXCEEDED`, the code every other deadline gives, instead of
  `RESOURCE_EXHAUSTED`; the doc says so.

- docs/architecture.md said a tier forwards `ResolveRef` and caches
  `GetRootTree`; it refuses both with `UNIMPLEMENTED` (`#307`), as its own
  statements elsewhere in the doc already said. The tier tables now
  match the code, the duplicated rename-index sentences are gone, the
  derived-data table names `derived::get_by` and the deadline a cold
  derive now carries, the blame bounds say the walk checks the deadline
  between steps and refuses a file over the diff-line cap, and the fault
  suites name the dm-log-writes suite that the directory-fsync row used
  to list as future work.
- A blame's window doc matched a rule that no longer holds, and the
  code behind it could not run (`#305`): a file of more than
  1,048,576 lines is refused, and a window is that many lines, so one
  call's window holds the whole servable file. The second-window
  branch of the resume, which re-walked the history from the request
  for a file's next window, is gone; the cursor still goes on over the
  walk's versions.

- A derive walk that answers a request holds its frontier of seen and
  pending changesets inside a byte budget the store's walks share:
  before, a walk over every parent held about 40 bytes per cold
  changeset with no bound in repo depth, so 10M commits after a
  derived-version bump held about 3 GB, eight walks at once. Over the
  budget the walk stops at its step boundary and the call answers
  `Unavailable` for a retry; the unbounded forms (fsck, import, the
  derive command) still finish the walk.
- A blame walk checks the call's deadline before each step, the first
  one whose memory share is more than the whole budget does, and a
  version over the cap in the file's history counts as absent, as one
  whose bytes do not fit the budget does. Before, the first step's
  diff ran past every bound: two versions of a file at the blob limit
  that share nothing kept a worker for hours, with no deadline check
  inside the diff.
- A damaged backend marker is refused, in both directions: a marker
  whose first line is neither `postgres` nor absent used to be read as
  absent, so a Postgres-backed directory opened as a fresh, empty
  SQLite store, which looks like a server with no data at all. The
  error says what to check. The fresh-directory claim that preceded
  the durable marker write is gone: `write_marker` writes the same
  first line, durable, so no crash can leave a partial marker behind.
- The `serve` flags are all checked before the data directory opens:
  a bad flag answers with its own error, instead of taking the data
  directory's one-server lock or creating a file in an empty directory.
- `--land-stale-secs` refuses 0 and negative values, and
  `--max-principal-requests`, `--max-principal-upload-bytes` and
  `--max-principal-pending-lands` refuse 0: those values used to mean
  that the sweep rejects every pending land, or that every call and
  land is refused, with no word of it at start.

- A fill of a kind over every parent (the commit graph) no longer holds
  a parent once per child that reached it in its walk: the seen set now
  checks pushes, so the walk's queue is as wide as the graph's frontier
  instead of as wide as its edges.
- A fill step reads the blob lengths its trees carry before its write
  transaction, not under the one write lock: up to a step's changesets
  of frame-header file reads sat between the lock's take and its
  release. Kinds that never read lengths (the commit graph, the path
  history) read no blob files at all.
- A land's check of a base that the path index does not reach kept
  every history node row it read, with no cap on their bytes: a row is
  up to 64 KiB, and one check reads one or more per part of each of the
  land's paths, so only the deadline stopped the read. The check's
  cache now clears its rows once they pass the same byte cap a log
  page's nodes stop at, so the answer never changes and only re-reads
  cost more.
- A silent TLS peer holds its connection's place for at most the
  handshake timeout, not twice that: one deadline from the accept
  bounds the TLS handshake and the HTTP/2 preface that follows it
  together, so the preface timer gets what is left of the deadline
  after the handshake ends.
- A `Diff` call's rename search could index every large added file for
  one deleted file, far past the budget that bounds it: one file's work
  was its index of every added file it could be like, and the budget
  was checked only between files, so a diff of many large added files
  read and indexed all of them for the first one (`#290`). The budget
  is now checked after each added file's slot in the index, so one
  call's cost stays at most the cap, plus one slot, plus one deleted
  file's read and lookup; a file whose indexing crosses the cap is not
  read and adds no scores, and the reply's `renames_incomplete` says
  the search stopped. A resumed search has one target, so its seed
  always completes and the search moves.
- `commit --from-mount` and the mount carry raw file names: a name that
  is not UTF-8 is no longer read from, or written to, the file its
  lossy spelling names.
- `commit --put` refuses anything that is not a regular file: a FIFO
  or a device never sees an end, and the read grew until the OOM
  killer.
- A Postgres read whose connection died inside its transaction left the
  pooled client unusable: the worker that serves the client kept the
  transaction's lost state on every later BEGIN, so every statement of
  that client failed and the read that opened a fresh transaction was
  refused too, with nothing to roll the open one back. The worker now
  follows a BEGIN that ran on the fresh session (the lost transaction
  is gone with the old session, and its caller cannot commit it
  through a new BEGIN), and a read's timeout statements that fail
  after the BEGIN roll the read transaction back before the reader
  returns to the pool.
- The land metrics count lands whose call ended in an error: a land
  that answered `Aborted`, `DeadlineExceeded`, or any other error class
  moved no counter, so an operator could not see failing lands. The
  land counter now moves for the error's class, under the same
  `scion_lands_total{outcome}` series.
- Two reads stop at the caller's deadline and the reply's budget
  (#283): a `Log` call without a path read up to 1,000 changesets, each
  as large as a changeset can be, with the call's deadline never
  checked; a `GetObjects` call's ticket walk decoded every tree on up
  to 1,000 requested paths, however many bytes that is, before the
  reply budget that bounds the reply was even consulted. The log page
  now checks the deadline at each entry, and the walk hands a tree to
  the walk only while the bytes it holds fit the budget's whole size,
  and stops at the deadline, before it decodes one more tree.
- `gc` no longer unlinks the file of a blob that a `PutBlob` in another
  process is writing at that moment: a put of an old orphan kept its
  file with a touch and renamed a replacing one, and a collector whose
  re-stat and unlink ran between those and the put's row commit deleted
  the file, so an acknowledged put named a file that was gone. The put
  and the sweep now hold the fan-out directory's `flock` around their
  file work, which orders them across processes the way the in-process
  stripe orders the writers of one process; a touch or rename that
  lands first changes the mtime the sweep re-stats, and an unlink that
  lands first makes the put's keep see a missing file and write anew.
- `fsck` no longer reports a blob-missing when the file it lists is an
  old orphan the concurrent `gc` collected while the check ran: a file
  that is gone and named by no row is the collector's take, not damage,
  and a file that is back between the check's two looks was rewritten
  by a racing put. A file a row names is still reported missing.

- A git import gives up instead of planning a ref's move again without
  end (#280): a ref that other lands keep moving while the import
  runs made the import check the ref's ancestry again and again, with
  no bound on the tries or on each walk of the history. The import now
  tries the move at most 3 times, answers that the ref kept moving,
  and checks ancestry through the derived commit graph, which reads a
  few nodes and stops at a deadline, instead of walking parents
  without one.

- A disk cache whose index row names more bytes than any object holds
  answers that row as corrupt, naming the object, instead of sizing
  its read buffer for the row.
- A snapshot seeded into a daemon counts an object that the store's
  byte limit refuses as skipped, so the seeded log line says what the
  store really holds.
- Two reads of a Postgres-backed store no longer panic on a column's
  type (`#252`): `GetObjects`' tree-size read and `fsck`'s
  derived-row version read wanted a width Postgres does not give, and
  the database driver panics on that. Both read what the column is
  now, and a derived-row version that overflows answers as corruption,
  not a crash.
- A write that the disk refused no longer looks like a server bug
  (#247): an out-of-space file write, SQLite metadata database or
  Postgres metadata database answers `RESOURCE_EXHAUSTED`, "the server
  is out of disk space; retry later", with no path in the reply; the
  server log carries the path and the operation. A cut-off write leaves
  no torn blob file, and an import that ran out of space is not retried
  while the disk is full.
- The container demo's output folders are git-ignored again (`#164`):
  `.gitignore` named `docker/demo/certs` and `docker/demo/work`, while
  `docker/demo.sh` writes `docker/certs` and `docker/work`, so a demo run
  left the demo CA's private key untracked instead of ignored.
- `Log` no longer repeats a page past the first scan (`#203`): a call
  whose `limit` crossed one internal page of 1,000 entries got those
  entries twice.
- A `Blame` cursor whose run length overflows the line count is refused
  with `INVALID_ARGUMENT` (`#201`), where a release build wrapped the
  count and answered a wrong window.
- `scn` prints each part of an error once, where the printed chain said
  the transport error twice.
- `scion-server hash-token --token` has a help text.
- The mount table does not fail silently (`#240`): an unmount whose kill
  fails keeps the record for a retry; a corrupt `mounts.json` is an
  error, not an empty table, so `scn mount` cannot double-mount a
  directory; the stop path reports on stderr what it could not clean; a
  tier sweep logs a removal it cannot make.

### Security

- The access file of `--access` holds bearer tokens and rules, and was
  read by plain path: no check of the directories above it, no symlink
  refusal, no owner or mode check, so a file another user holds - and can
  replace at will - was taken as the access policy. The TLS private keys
  of `--tls-key` (scion-server) and `--tls-client-key` (scion-cache,
  scion-daemon) were read by plain path too. All of them are read through
  one checked descriptor now, the read the ticket keys gained in #354,
  moved into `scion_proto::runtime::auth` so every binary shares it: the
  directories above are checked, the file is opened once with
  `O_NOFOLLOW`, and kind, owner (the account that runs the process or
  root), and mode run on the descriptor the bytes are read from - a
  secret is read by its owner alone. The ticket keys keep their
  public-key rule (no group or others write) through the same core. A
  symlinked data directory is refused by kind now, not by the link's
  mode: on macOS lchmod can give a symlink any mode, so the mode check
  alone did not catch one. The ELOOP message names a link at the file
  precisely and a loop on the way as what it is.
  
  Files the binaries read at start that were checked and left as they
  are, with why: the TLS certificate and client-CA files (public
  material, but not nothing: a swapped client CA would change whose
  certificates the server accepts, so they stay a listed gap for a
  follow-up decision), the `scn --put` content file (the caller's own
  content, kind-checked already), and the `scn` mount session file (not
  a secret).
- The check that keeps another user from swapping a path on its way had
  two gaps. The sticky bit excused a group-writable directory as it
  excuses the /tmp shape, so 01770 and 02770 passed: a sticky bit bounds
  who may remove a name, not who may add one, so a group that can write
  the directory can still plant and swap its own entries. A
  group-writable ancestor is refused now whatever the sticky bit does;
  the sticky bit keeps its one excuse, the shape where others can write
  too (01777), where the others-write rule is the real wall. And the
  checks read only the mode bits, not POSIX ACLs, so an access ACL that
  grants another account what the mode does not passed everywhere: now
  the checked read of a secret or a shared file refuses one whose access
  ACL is not the exact minimal form (version 2, one USER_OBJ, one
  GROUP_OBJ, one OTHER, undefined ids, perms equal to the mode's bits),
  an ancestor directory with such an ACL or with any default ACL is
  refused on the walk, and an ACL that cannot be read is refused with the
  rest. On macOS the descriptor's extended ACL is read through one private
  ffi module (acl_get_fd_np, the one place the workspace's unsafe-code
  lint lets hold it): a descriptor with no ACL passes, one or more entries
  refuses, a read that fails refuses. The workspace's unsafe_code lint
  drops from forbid to deny, with the lint forbid at every crate root but
  this one, and a test walks the crate's sources and refuses unsafe code
  outside the ffi module. On this Mac the temp directories carry no ACLs
  and the ambient deny-only ACLs of the system pass, as they grant
  nothing; an ACL that grants anything is refused.
- A ticket key was checked by path and then read through the same
  unresolved path again: a directory on the checked path that another
  user could change let them swap the file between the two steps, and a
  key file owned by another user passed the checks at all, so a public
  key they hold could be fed in as the old key of a rotation. The key
  now opens once, through the path the ancestor check resolved and with
  a symlink at the end refused, and the kind, owner (the user that runs
  the server or root), and mode checks run on the open descriptor that
  the key is read from. A symlinked data directory is refused by kind
  now: a symlink's mode is not a rule (on macOS lchmod can set any mode
  on one), so the mode check alone did not catch it.
- The files a tier loads that must stay whole — the TLS certificate and
  the client CA of a server, the CA, client certificate, and seed
  snapshots of a cache or daemon — were read by path with no check: a
  symlink, a file another user owns, or a mode that lets another user
  rewrite the file all passed. A swapped certificate lets someone else
  be the server, a swapped CA changes whose client certificates the
  server accepts, and a swapped seed can seed what the daemon serves.
  These reads go through one checked descriptor now (read_shared_file:
  ancestors checked, O_NOFOLLOW, kind, owner, and a mode no other
  account can write, checked on the open descriptor the bytes are read
  from), as the secrets already did. The checked open is its own step
  now, so a seed the size of a disk streams through a buffered reader
  instead of being held whole in memory.
- The CI workflow pins every third-party action to the commit SHA its
  version tag named, with the version in a comment, runs on fixed
  runner images (`ubuntu-24.04`, `macos-15`) and pins the Postgres
  service to `postgres:17.11`; Dependabot keeps the pins current, and
  a runner or image upgrade is a diff, not a surprise. The MSRV job
  checks everything the CI checks, locked, with every feature, on
  `cargo +1.88`.
- A blame cursor that claims an impossible line count is refused as a
  bad cursor (`#288`): the count is the client's word, and its
  per-line memory cost was multiplied without an overflow check, so a
  count of 2^57 wrapped to zero and the walk went on to size its
  window from the count - one request could stop the server.

- The mount's overlay writes no longer follow a symlink planted in the
  overlay: a write, a truncate, a mode change, and the deletions note
  refuse one. A symlinked parent directory still redirects (a named
  limit: the upgrade is opening every parent through a handle chain,
  as `materialize.rs` does).

- The server refuses a data directory that group or others can write, or
  that sits under a directory another user can change: the blobs and the
  metadata are only as private as their directory (`#197`).
- The ticket-key seed must be readable by its owner alone, a public-key
  file must not be group-writable, both refuse a symlink, and a second
  `ticket-key` run refuses, so a key cannot change under the tiers
  (`#168`).
- `scion-daemon --seed` refuses a snapshot unless `--seed-full-repo` says
  every user of the node may read the repo whole (`#241`): a snapshot is
  a full-repo credential.
