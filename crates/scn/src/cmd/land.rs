//! `land`.

use anyhow::{Context, Result, bail};
use scion_core::ChangesetId;
use scion_proto::v1::{LandRequest, LandResponse, LandState};
use serde_json::json;

use crate::client::{Ctx, id_from, lossy};

/// Arguments of `scn land`.
#[derive(clap::Args)]
pub struct LandArgs {
    /// Changeset ID.
    changeset: ChangesetId,
    /// Ref to move.
    #[arg(long, default_value = "main")]
    onto: String,
    /// Retry key. Defaults to the changeset ID and ref, so a retry of the
    /// same land is safe.
    #[arg(long)]
    request_id: Option<String>,
}

/// Lands a changeset and prints the outcome. Fails unless it landed, so a
/// script or an agent can branch on the exit code.
pub async fn land(ctx: &mut Ctx, args: LandArgs) -> Result<()> {
    let LandArgs {
        changeset: id,
        onto,
        request_id,
    } = args;
    let request_key = request_id.unwrap_or_else(|| format!("{id}:{onto}"));
    let reply = ctx
        .client
        .land(LandRequest {
            repo: ctx.repo.clone(),
            changeset_id: id.as_bytes().to_vec(),
            target_ref: onto,
            request_key,
        })
        .await
        .context("land failed")?
        .into_inner();
    let state = reply.state();
    print_outcome(ctx.json, &reply)?;
    match state {
        LandState::Landed => Ok(()),
        LandState::Conflict => bail!("land conflicted: {}", reply.detail),
        LandState::Rejected => bail!("land rejected: {}", reply.detail),
        LandState::Pending | LandState::Unspecified => {
            bail!("land {} did not finish", reply.land_id)
        }
    }
}

fn print_outcome(json: bool, reply: &LandResponse) -> Result<()> {
    let result = if reply.result_changeset_id.is_empty() {
        None
    } else {
        Some(id_from::<ChangesetId>(&reply.result_changeset_id)?)
    };
    let paths: Vec<String> = reply
        .conflict_paths
        .iter()
        .map(|p| lossy(p).into_owned())
        .collect();
    if json {
        let out = json!({
            "land_id": reply.land_id,
            "state": reply.state().as_str_name(),
            "result": result.map(|id| id.to_string()),
            "detail": reply.detail,
            "conflict_paths": paths,
        });
        println!("{out}");
        return Ok(());
    }
    match (reply.state(), result) {
        (LandState::Landed, Some(id)) => println!("landed {id}"),
        (LandState::Conflict, _) => {
            println!("conflict: {}", reply.detail);
            for path in &paths {
                println!("  {path}");
            }
            println!("re-read those paths on the target, redo the change, commit again");
        }
        (state, _) => println!("{}: {}", state.as_str_name(), reply.detail),
    }
    Ok(())
}
