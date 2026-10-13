# Scion

A server-centric version control system for monorepos where most writers are
agents. Written in Rust.

Scion is an independent implementation of the design that Anthropic described
in [Redesigning source control](https://www.anthropic.com/engineering/redesigning-source-control).
It is not affiliated with or endorsed by Anthropic.

> **Status: early proof of concept.** The object format is done. The server,
> client, and checkout are not built yet. Do not use it for real code.

## Idea

In git, every clone is a full copy and every merge starts on the client. Scion
moves the work to the server:

- The server keeps history and creates commits. A client asks only for the
  files it needs. A job can read and write code with no checkout at all.
- A commit (a *changeset*) lists only the files that changed against its
  first parent. When two changes touch different paths, rebasing one onto
  the other only swaps the parent.
- Every object is content-addressed and never changes, so any cache tier can
  serve any object.

```
agent ── "src/a.rs changed" ──> scion-server ── rebase + move main ──> landed
                                     │
                     changesets, trees, blobs (content-addressed)
```

## Layout

| Path | Contents |
|---|---|
| [`crates/scion-core`](crates/scion-core) | Object model and canonical byte format |
| [`docs/format-v1.md`](docs/format-v1.md) | The v1 byte format. Object IDs are hashes of these bytes. |

## Build and test

Requires Rust 1.88 or newer.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Fuzz the decoders (needs nightly and `cargo-fuzz`):

```sh
cd crates/scion-core
cargo +nightly fuzz run decode "$PWD/fuzz/corpus/decode" "$PWD/fuzz/seeds/decode"
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
