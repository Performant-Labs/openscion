//! `land`.

use anyhow::Result;

use crate::client::Ctx;

/// Lands a changeset and prints the outcome.
pub async fn land(
    ctx: &mut Ctx,
    changeset: &str,
    onto: &str,
    request_id: Option<String>,
) -> Result<()> {
    let _ = (ctx, changeset, onto, request_id);
    todo!("S3")
}
