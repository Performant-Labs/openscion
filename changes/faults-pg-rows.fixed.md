ms: "the fault matrix says where each Postgres row stands"
---

Two rows of the fault matrix now say what covers them on Postgres:
a really full database is covered by the reply-mapping test (#480),
with the end-to-end run that would close it named (the CI Postgres
service with its data directory on a small tmpfs); the block-level
replay row is Postgres by design - the drive below the metadata is
Postgres's own, and the server asserts synchronous_commit=on at
open - so it is proven for the blob files and left to Postgres for
the metadata.
