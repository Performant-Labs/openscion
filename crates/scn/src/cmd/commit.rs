//! `commit`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use scion_core::{BlobId, Changeset, ChangesetId, FileState, IdentityTime, RepoPath};
use scion_proto::v1::{CreateChangesetRequest, PutBlobRequest};

use crate::client::{Ctx, id_from};

/// Size of each `PutBlob` message.
const CHUNK_BYTES: usize = 1 << 20;

/// Parses `PATH=FILE`.
pub fn parse_put(spec: &str) -> Result<(String, PathBuf)> {
    let (path, file) = spec
        .split_once('=')
        .with_context(|| format!("--put {spec:?}: expected PATH=FILE"))?;
    Ok((path.to_owned(), PathBuf::from(file)))
}

/// Uploads the files, creates the changeset, and prints its ID.
pub async fn commit(
    ctx: &mut Ctx,
    base: &str,
    message: &str,
    puts: &[(String, PathBuf)],
    executable: bool,
    deletes: &[String],
    author: &str,
) -> Result<()> {
    let (name, email) = parse_author(author)?;
    // Each path and the local file to put there, or `None` to delete it.
    let mut edits = BTreeMap::new();
    let puts = puts.iter().map(|(path, file)| (path, Some(file)));
    for (path, file) in puts.chain(deletes.iter().map(|path| (path, None))) {
        let path = RepoPath::new(path.as_str()).with_context(|| format!("bad path {path:?}"))?;
        ensure!(
            !edits.contains_key(&path),
            "{path:?} is given more than once"
        );
        edits.insert(path, file);
    }

    let parent = ctx.resolve(base).await?;
    let mut changes = BTreeMap::new();
    for (path, file) in edits {
        let state = match file {
            None => None,
            Some(file) => {
                // SHORTCUT: reads each file whole, so memory caps the file
                // size (the server caps it at 1 GiB too). Upgrade: stream
                // chunks from the file.
                let data = std::fs::read(file)
                    .with_context(|| format!("cannot read {}", file.display()))?;
                let content = put_blob(ctx, data)
                    .await
                    .with_context(|| format!("cannot upload {}", file.display()))?;
                Some(FileState::Regular {
                    content,
                    executable,
                })
            }
        };
        changes.insert(path, state);
    }
    let changeset = Changeset {
        parents: vec![parent],
        author: IdentityTime {
            name,
            email,
            unix_secs: now_unix_secs()?,
            // SHORTCUT: always UTC, since std cannot read the local time zone.
            // Upgrade: a time zone crate.
            tz_offset_minutes: 0,
        },
        committer: None,
        message: message.as_bytes().to_vec(),
        changes,
    };
    let reply = ctx
        .client
        .create_changeset(CreateChangesetRequest {
            repo: ctx.repo.clone(),
            changeset: changeset.encode(),
        })
        .await
        .context("cannot create the changeset")?
        .into_inner();
    let id: ChangesetId = id_from(&reply.changeset_id)?;
    if ctx.json {
        println!("{}", serde_json::json!({ "changeset": id.to_string() }));
    } else {
        println!("{id}");
    }
    Ok(())
}

/// Uploads one file's content. The first message names the repo, and an empty
/// file still sends one (empty) message.
async fn put_blob(ctx: &mut Ctx, data: Vec<u8>) -> Result<BlobId> {
    let repo = ctx.repo.clone();
    let messages = data.len().div_ceil(CHUNK_BYTES).max(1);
    // Copies one chunk at a time, when the stream asks for it.
    let requests = (0..messages).map(move |i| {
        let end = data.len().min((i + 1) * CHUNK_BYTES);
        PutBlobRequest {
            repo: if i == 0 { repo.clone() } else { String::new() },
            data: data[i * CHUNK_BYTES..end].to_vec(),
        }
    });
    let reply = ctx
        .client
        .put_blob(tokio_stream::iter(requests))
        .await?
        .into_inner();
    id_from(&reply.blob_id)
}

/// Splits `Name <email>` into name and email bytes.
fn parse_author(author: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let well_formed = |part: &str| !part.is_empty() && !part.contains(['<', '>', '\n']);
    match author
        .strip_suffix('>')
        .and_then(|rest| rest.split_once(" <"))
    {
        Some((name, email)) if well_formed(name) && well_formed(email) => {
            Ok((name.as_bytes().to_vec(), email.as_bytes().to_vec()))
        }
        _ => bail!("--author {author:?}: expected \"Name <email>\""),
    }
}

fn now_unix_secs() -> Result<i64> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("the system clock is before 1970")?
        .as_secs();
    i64::try_from(secs).context("the system clock is too far in the future")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn author_splits_name_and_email() {
        let (name, email) = parse_author("Ada Lovelace <ada@example.com>").unwrap();
        assert_eq!(name, b"Ada Lovelace");
        assert_eq!(email, b"ada@example.com");
    }

    #[test]
    fn malformed_author_is_rejected() {
        for bad in [
            "Ada",
            "ada@example.com",
            "<ada@example.com>",
            "Ada <>",
            "Ada <ada@example.com",
            "Ada<ada@example.com>",
            "Ada <ada@example.com> x",
            "Ada <a <b>",
            "Ada\nEve <ada@example.com>",
        ] {
            assert!(parse_author(bad).is_err(), "{bad:?} was accepted");
        }
    }
}
