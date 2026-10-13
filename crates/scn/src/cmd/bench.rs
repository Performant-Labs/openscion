//! `bench`: many agents commit and land at once against a running server.
//!
//! Each agent writes its own files, so no change should conflict. The bench
//! then walks `main` back to where it started and checks that every landed
//! change is there exactly once.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use scion_core::{BlobId, Changeset, ChangesetId, FileState, IdentityTime, RepoPath};
use scion_proto::v1::{CreateChangesetRequest, LandRequest, LandState, LogRequest, PutBlobRequest};
use serde_json::json;
use tokio::task::JoinSet;

use crate::client::{Ctx, id_from};

/// How each agent picks its base.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Base {
    /// Keep the base the agent started from. Every land rebases.
    Shared,
    /// Re-read `main` before each change.
    Fresh,
}

#[derive(Default)]
struct Stats {
    landed: Vec<ChangesetId>,
    conflicts: usize,
    rejected: usize,
    /// Time of the Land RPC alone.
    land: Vec<Duration>,
    /// Upload + create + land.
    total: Vec<Duration>,
}

/// Runs the benchmark and prints throughput and latency.
pub async fn bench(ctx: &Ctx, agents: usize, lands: usize, base: Base) -> Result<()> {
    ensure!(
        agents > 0 && lands > 0,
        "--agents and --lands must be positive"
    );
    let start = ctx.clone().resolve("main").await?;
    let per_agent = lands.div_ceil(agents);
    let started = Instant::now();
    let mut tasks = JoinSet::new();
    for agent in 0..agents {
        let count = per_agent.min(lands.saturating_sub(agent * per_agent));
        let ctx = ctx.clone();
        tasks.spawn(run_agent(ctx, agent, count, start, base));
    }
    let mut stats = Stats::default();
    while let Some(agent) = tasks.join_next().await {
        let agent = agent.context("agent task panicked")??;
        stats.landed.extend(agent.landed);
        stats.conflicts += agent.conflicts;
        stats.rejected += agent.rejected;
        stats.land.extend(agent.land);
        stats.total.extend(agent.total);
    }
    let elapsed = started.elapsed();
    let on_main = verify(ctx.clone(), start, &stats.landed).await?;
    report(ctx.json, agents, &mut stats, elapsed, on_main);
    Ok(())
}

async fn run_agent(
    mut ctx: Ctx,
    agent: usize,
    count: usize,
    start: ChangesetId,
    base: Base,
) -> Result<Stats> {
    let mut stats = Stats::default();
    for i in 0..count {
        let began = Instant::now();
        let parent = match base {
            Base::Shared => start,
            Base::Fresh => ctx.resolve("main").await?,
        };
        let content = format!("agent {agent} change {i}\n");
        let blob = upload(&mut ctx, content.as_bytes()).await?;
        let path = RepoPath::new(format!("bench/agent-{agent:04}/{i:06}.txt"))?;
        let changeset = Changeset {
            parents: vec![parent],
            author: author(),
            committer: None,
            message: format!("bench: agent {agent} change {i}\n").into_bytes(),
            changes: BTreeMap::from([(
                path,
                Some(FileState::Regular {
                    content: blob,
                    executable: false,
                }),
            )]),
        };
        let id: ChangesetId = id_from(
            &ctx.client
                .create_changeset(CreateChangesetRequest {
                    repo: ctx.repo.clone(),
                    changeset: changeset.encode(),
                })
                .await?
                .into_inner()
                .changeset_id,
        )?;
        let landing = Instant::now();
        let reply = ctx
            .client
            .land(LandRequest {
                repo: ctx.repo.clone(),
                changeset_id: id.as_bytes().to_vec(),
                target_ref: "main".into(),
                request_key: format!("bench:{id}"),
            })
            .await?
            .into_inner();
        stats.land.push(landing.elapsed());
        stats.total.push(began.elapsed());
        match reply.state() {
            LandState::Landed => stats.landed.push(id_from(&reply.result_changeset_id)?),
            LandState::Conflict => stats.conflicts += 1,
            _ => stats.rejected += 1,
        }
    }
    Ok(stats)
}

async fn upload(ctx: &mut Ctx, data: &[u8]) -> Result<BlobId> {
    let request = PutBlobRequest {
        repo: ctx.repo.clone(),
        data: data.to_vec(),
    };
    let reply = ctx
        .client
        .put_blob(tokio_stream::iter([request]))
        .await?
        .into_inner();
    id_from(&reply.blob_id)
}

fn author() -> IdentityTime {
    let unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    IdentityTime {
        name: b"scn bench".to_vec(),
        email: b"bench@localhost".to_vec(),
        unix_secs,
        tz_offset_minutes: 0,
    }
}

/// Walks `main` back to `start` and checks that it holds exactly the landed
/// changesets. Returns how many changesets were added to `main`.
async fn verify(mut ctx: Ctx, start: ChangesetId, landed: &[ChangesetId]) -> Result<usize> {
    let mut cur = Some(ctx.resolve("main").await?);
    let mut history = HashSet::new();
    'walk: while let Some(from) = cur {
        let page = ctx
            .client
            .log(LogRequest {
                repo: ctx.repo.clone(),
                changeset_id: from.as_bytes().to_vec(),
                limit: 1000,
            })
            .await?
            .into_inner();
        cur = None;
        for entry in page.entries {
            let id: ChangesetId = id_from(&entry.changeset_id)?;
            if id == start {
                break 'walk;
            }
            ensure!(history.insert(id), "changeset {id} appears twice on main");
            cur = Changeset::decode(&entry.changeset)?
                .parents
                .first()
                .copied();
        }
    }
    let missing = landed.iter().filter(|id| !history.contains(*id)).count();
    ensure!(missing == 0, "{missing} landed changesets are not on main");
    ensure!(
        history.len() == landed.len(),
        "main gained {} changesets but {} lands reported success",
        history.len(),
        landed.len()
    );
    Ok(history.len())
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let rank = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[rank]
}

fn report(json: bool, agents: usize, stats: &mut Stats, elapsed: Duration, on_main: usize) {
    stats.land.sort();
    stats.total.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    #[allow(clippy::cast_precision_loss)]
    let rate = stats.landed.len() as f64 / elapsed.as_secs_f64();
    let (p50, p99, max) = (
        percentile(&stats.land, 0.50),
        percentile(&stats.land, 0.99),
        stats.land.last().copied().unwrap_or_default(),
    );
    if json {
        let out = json!({
            "agents": agents,
            "landed": stats.landed.len(),
            "conflicts": stats.conflicts,
            "rejected": stats.rejected,
            "seconds": elapsed.as_secs_f64(),
            "lands_per_second": rate,
            "land_ms": { "p50": ms(p50), "p99": ms(p99), "max": ms(max) },
            "end_to_end_ms": { "p50": ms(percentile(&stats.total, 0.50)), "p99": ms(percentile(&stats.total, 0.99)) },
            "verified_on_main": on_main,
        });
        println!("{out}");
        return;
    }
    println!(
        "{} lands by {agents} agents in {:.1} s: {rate:.0} lands/s",
        stats.landed.len(),
        elapsed.as_secs_f64()
    );
    println!("conflicts {}, rejected {}", stats.conflicts, stats.rejected);
    println!(
        "land RPC     p50 {:.1} ms  p99 {:.1} ms  max {:.1} ms",
        ms(p50),
        ms(p99),
        ms(max)
    );
    println!(
        "end to end   p50 {:.1} ms  p99 {:.1} ms  (upload + create + land)",
        ms(percentile(&stats.total, 0.50)),
        ms(percentile(&stats.total, 0.99))
    );
    println!("verified: main gained exactly the {on_main} landed changesets");
}
