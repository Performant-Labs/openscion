//! scn's JSON keeps non-UTF-8 bytes: a name or a message that is not
//! valid UTF-8 prints its lossy text in the field a script already
//! reads, with the exact bytes beside it in a sibling `<field>_b64`
//! (standard base64, with padding), so a script gets the real bytes
//! back and two different names never print as one. A value that is
//! valid UTF-8 prints exactly as before, with no sibling field — the
//! golden (`json_golden.rs`) pins that shape.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Arc;

use base64::Engine as _;
use scion_core::{BlobId, Changeset, FileState, IdentityTime, RepoPath};
use scion_proto::runtime::listen::Limits;
use scion_server::test_support::meta::{Meta as _, MetaTx as _};
use scion_server::test_support::service;
use scion_server::{Backend, Quotas};
use tempfile::TempDir;
use tokio::net::TcpListener;

const TOKEN: &str = "secret";
const REPO: &str = "r";

/// The bytes of the fixture: a name, an author and a message whose bytes
/// are not valid UTF-8 (a `0x80` byte), and one name that is plain.
const RAW_NAME: &[u8] = b"raw\xff.txt";
const RAW_MESSAGE: &[u8] = b"a \xff message";
const RAW_AUTHOR: &[u8] = b"Ad\xffy";
const PLAIN_NAME: &[u8] = b"plain.txt";
/// A name whose base64 encoding needs padding: 13 bytes are 4 groups of
/// 3 minus 2, so the encoding is 20 characters ending in one `=`.
const PADDED_NAME: &[u8] = b"thirteen-byt\x80";

struct Fixture {
    url: String,
    home: TempDir,
    /// The server runs on this runtime; dropped, the server stops.
    _runtime: tokio::runtime::Runtime,
}

impl Fixture {
    /// A store whose `main` names one changeset that puts the plain name
    /// and the raw names (the tree `ls` reads), with the raw author and
    /// message.
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Backend::open_test(dir.path()).unwrap();
        let repo = store.write(|c| c.create_repo(REPO)).unwrap();

        let plain_blob = store.blobs().put(b"plain\n").unwrap();
        let raw_blob = store.blobs().put(b"raw\n").unwrap();
        let pad_blob = store.blobs().put(b"pad\n").unwrap();
        let root = store
            .write(|c| {
                c.insert_changeset(
                    repo,
                    &changeset(
                        b"root",
                        BTreeMap::from([(RepoPath::new(PLAIN_NAME).unwrap(), plain_blob)]),
                    ),
                )
            })
            .unwrap();
        let tip = store
            .write(|c| {
                c.insert_changeset(
                    repo,
                    &Changeset::new(
                        vec![root],
                        IdentityTime::new(
                            RAW_AUTHOR.to_vec(),
                            b"ada@example.com".to_vec(),
                            1_700_000_000,
                            0,
                        )
                        .unwrap(),
                        None,
                        RAW_MESSAGE.to_vec(),
                        BTreeMap::from([
                            (
                                RepoPath::new(RAW_NAME).unwrap(),
                                Some(FileState::Regular {
                                    content: raw_blob,
                                    executable: false,
                                }),
                            ),
                            (
                                RepoPath::new(PADDED_NAME).unwrap(),
                                Some(FileState::Regular {
                                    content: pad_blob,
                                    executable: false,
                                }),
                            ),
                        ]),
                    )
                    .unwrap(),
                )
            })
            .unwrap();
        store.write(|c| c.cas_ref(repo, "main", None, tip)).unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        runtime.spawn(service::serve(
            store,
            listener,
            TOKEN,
            service::ServeOptions {
                data: dir.path().to_path_buf(),
                audit: scion_server::test_support::audit::Audit::off(),
                limits: Limits::default(),
                quotas: Quotas::off(),
                pending_lands: None,
                land_stale_secs: 1 << 40,
                tls: None,
                metrics: Arc::default(),
            },
            None,
            std::future::pending(),
        ));
        // The data directory must outlive the server, as json_golden's
        // fixture does.
        std::mem::forget(dir);
        let home = tempfile::tempdir().unwrap();
        Self {
            url,
            home,
            _runtime: runtime,
        }
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.scn(args);
        assert!(
            out.status.success(),
            "scn {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap()
    }

    fn scn(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_scn"))
            .args(["--server", &self.url, "--repo", REPO])
            .args(args)
            .env("SCION_TOKEN", TOKEN)
            .env("HOME", self.home.path())
            .output()
            .unwrap()
    }
}

/// A changeset on no parent that puts `name` -> `blob`.
fn changeset(message: &[u8], puts: BTreeMap<RepoPath, BlobId>) -> Changeset {
    Changeset::new(
        Vec::new(),
        IdentityTime::new(
            b"Ada".to_vec(),
            b"ada@example.com".to_vec(),
            1_700_000_000,
            0,
        )
        .unwrap(),
        None,
        message.to_vec(),
        puts.into_iter()
            .map(|(name, blob)| {
                (
                    name,
                    Some(FileState::Regular {
                        content: blob,
                        executable: false,
                    }),
                )
            })
            .collect(),
    )
    .unwrap()
}

/// The base64 of `bytes`, standard alphabet with padding.
fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The sibling `<field>_b64` of `value` holds the exact bytes, decodes to
/// them, and the text field is the lossy form of the same bytes.
fn assert_b64(value: &serde_json::Value, field: &str, want: &[u8]) {
    let got = value[format!("{field}_b64")]
        .as_str()
        .unwrap_or_else(|| panic!("{field}_b64 missing: {value}"));
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(got)
            .unwrap(),
        want,
        "{field}_b64 does not decode to the bytes"
    );
    let text = value[field].as_str().unwrap();
    assert_eq!(
        text.as_bytes(),
        String::from_utf8_lossy(want).as_bytes(),
        "{field} is the lossy form of the bytes"
    );
}

#[test]
fn a_non_utf8_name_and_message_round_trip_through_the_json() {
    let fx = Fixture::start();

    // ls: the tree holds the raw names beside the plain one.
    let ls = fx.json(&["ls", "--json", "main"]);
    let entries = ls.as_array().unwrap();
    let raw = entries
        .iter()
        .find(|e| e["name"].as_str() == Some(String::from_utf8_lossy(RAW_NAME).as_ref()))
        .expect("the raw name is in the listing");
    assert_b64(raw, "name", RAW_NAME);
    let padded = entries
        .iter()
        .find(|e| e["name"].as_str() == Some(String::from_utf8_lossy(PADDED_NAME).as_ref()))
        .expect("the padded name is in the listing");
    assert_b64(padded, "name", PADDED_NAME);
    // The padding case: 13 bytes encode to 20 characters ending in
    // exactly one `=`, and decode byte-exact.
    let encoded = padded["name_b64"].as_str().unwrap();
    assert_eq!(encoded.len() % 4, 0, "standard base64 with padding");
    assert!(encoded.ends_with('='));
    assert_eq!(b64(PADDED_NAME), encoded);

    // show: the message, the change's path and the author's name.
    let show = fx.json(&["show", "--json", "main"]);
    assert_b64(&show, "message", RAW_MESSAGE);
    let changes = show["changes"].as_array().unwrap();
    let raw_change = changes
        .iter()
        .find(|c| c["path"].as_str() == Some(String::from_utf8_lossy(RAW_NAME).as_ref()))
        .expect("the raw path is in the changes");
    assert_b64(raw_change, "path", RAW_NAME);
    assert_b64(&show["author"], "name", RAW_AUTHOR);

    // log: the same author and message through the history view.
    let log = fx.json(&["log", "--json", "main"]);
    let tip = log
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["message"].as_str() == Some(String::from_utf8_lossy(RAW_MESSAGE).as_ref()))
        .expect("the tip is in the log");
    assert_b64(&tip["author"], "name", RAW_AUTHOR);
    assert_b64(tip, "message", RAW_MESSAGE);
}

#[test]
fn a_valid_utf8_value_prints_exactly_as_before() {
    // The golden (json_golden.rs) pins the whole shape for valid UTF-8;
    // here the one field: a plain name carries no sibling, and the text
    // is the name.
    let fx = Fixture::start();
    let ls = fx.json(&["ls", "--json", "main"]);
    let plain = ls
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"].as_str() == Some("plain.txt"))
        .unwrap();
    assert_eq!(plain["name"], "plain.txt");
    assert!(plain.get("name_b64").is_none());
}
