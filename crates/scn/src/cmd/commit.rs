//! `commit`.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::client::Ctx;

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
    let _ = (ctx, base, message, puts, executable, deletes, author);
    todo!("S2")
}
