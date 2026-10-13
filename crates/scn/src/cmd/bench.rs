//! `bench`: many agents commit and land at once.

use anyhow::Result;

use crate::client::Ctx;

/// How each agent picks its base.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Base {
    /// Keep the base the agent started from. Every land rebases.
    Shared,
    /// Re-read `main` before each change.
    Fresh,
}

/// Runs the benchmark and prints throughput and latency.
pub async fn bench(ctx: &Ctx, agents: usize, lands: usize, base: Base) -> Result<()> {
    let _ = (ctx, agents, lands, base);
    todo!("S3")
}
