//! `scn`: command-line client for a Scion server.

mod client;
mod cmd;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::client::Ctx;

#[derive(Parser)]
#[command(version, about = "Client for the Scion version control system")]
struct Cli {
    /// Server address.
    #[arg(
        long,
        env = "SCION_SERVER",
        default_value = "http://127.0.0.1:7878",
        global = true
    )]
    server: String,
    /// Bearer token for the server.
    #[arg(
        long,
        env = "SCION_TOKEN",
        hide_env_values = true,
        global = true,
        default_value = ""
    )]
    token: String,
    /// Repository name.
    #[arg(long, env = "SCION_REPO", default_value = "main", global = true)]
    repo: String,
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print a file: `scn cat main:src/lib.rs`.
    Cat {
        /// `REV:PATH`. REV is a ref name or a changeset ID.
        spec: String,
    },
    /// List a directory: `scn ls main:src`, or `scn ls main` for the root.
    Ls {
        /// `REV[:DIR]`.
        spec: String,
    },
    /// Show first-parent history, newest first.
    Log {
        /// Ref name or changeset ID.
        #[arg(default_value = "main")]
        rev: String,
        /// How many changesets to show.
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: u32,
    },
    /// Show one changeset and its changed paths.
    Show {
        /// Ref name or changeset ID.
        rev: String,
    },
    /// Create a changeset on the server. Moves no ref; use `scn land` next.
    Commit {
        /// Base: ref name or changeset ID.
        #[arg(long, default_value = "main")]
        base: String,
        /// Commit message.
        #[arg(short, long)]
        message: String,
        /// `PATH=FILE`: put the content of local FILE at PATH. Repeatable.
        #[arg(long = "put", value_name = "PATH=FILE")]
        puts: Vec<String>,
        /// Mark the put files executable.
        #[arg(long)]
        executable: bool,
        /// PATH to delete. Repeatable.
        #[arg(long = "delete", value_name = "PATH")]
        deletes: Vec<String>,
        /// Author as `Name <email>`.
        #[arg(long, env = "SCION_AUTHOR", default_value = "scn <scn@localhost>")]
        author: String,
    },
    /// Rebase a changeset onto a ref and move the ref.
    Land {
        /// Changeset ID.
        changeset: String,
        /// Ref to move.
        #[arg(long, default_value = "main")]
        onto: String,
        /// Retry key. Defaults to the changeset ID and ref, so a retry of the
        /// same land is safe.
        #[arg(long)]
        request_id: Option<String>,
    },
    /// Many agents commit and land at the same time; print throughput.
    Bench {
        /// Concurrent agents.
        #[arg(long, default_value_t = 200)]
        agents: usize,
        /// Total lands across all agents.
        #[arg(long, default_value_t = 10_000)]
        lands: usize,
        /// Each agent re-reads `main` before every change (fresh base),
        /// or keeps the base it started from (shared base).
        #[arg(long, value_enum, default_value_t = cmd::bench::Base::Shared)]
        base: cmd::bench::Base,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("scn: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let mut ctx = Ctx::connect(&cli.server, &cli.token, cli.repo, cli.json).await?;
    match cli.command {
        Command::Cat { spec } => cmd::read::cat(&mut ctx, &spec).await,
        Command::Ls { spec } => cmd::read::ls(&mut ctx, &spec).await,
        Command::Log { rev, limit } => cmd::read::log(&mut ctx, &rev, limit).await,
        Command::Show { rev } => cmd::read::show(&mut ctx, &rev).await,
        Command::Commit {
            base,
            message,
            puts,
            executable,
            deletes,
            author,
        } => {
            let puts = puts
                .iter()
                .map(|p| cmd::commit::parse_put(p))
                .collect::<anyhow::Result<Vec<(String, PathBuf)>>>()?;
            cmd::commit::commit(
                &mut ctx, &base, &message, &puts, executable, &deletes, &author,
            )
            .await
        }
        Command::Land {
            changeset,
            onto,
            request_id,
        } => cmd::land::land(&mut ctx, &changeset, &onto, request_id).await,
        Command::Bench {
            agents,
            lands,
            base,
        } => cmd::bench::bench(&ctx, agents, lands, base).await,
    }
}
