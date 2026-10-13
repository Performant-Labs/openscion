//! `cat`, `ls`, `log`, `show`.

use anyhow::Result;

use crate::client::Ctx;

/// Prints a file.
pub async fn cat(ctx: &mut Ctx, spec: &str) -> Result<()> {
    let _ = (ctx, spec);
    todo!("S1")
}

/// Lists a directory.
pub async fn ls(ctx: &mut Ctx, spec: &str) -> Result<()> {
    let _ = (ctx, spec);
    todo!("S1")
}

/// Prints first-parent history.
pub async fn log(ctx: &mut Ctx, rev: &str, limit: u32) -> Result<()> {
    let _ = (ctx, rev, limit);
    todo!("S1")
}

/// Prints one changeset.
pub async fn show(ctx: &mut Ctx, rev: &str) -> Result<()> {
    let _ = (ctx, rev);
    todo!("S1")
}
