//! `proposal`: propose a changeset for a ref, review it, and see whether
//! it may land. `scn land` lands it; on a protected ref the server lands
//! only an approved proposal's head.

use crate::client::{Ctx, id_from, lossy, parse_repo_path};
use crate::outln;
use anyhow::{Context, Result, bail};
use scion_core::{ChangesetId, RepoPath};
use scion_proto::v1::{
    AddCommentRequest, CloseProposalRequest, CreateProposalRequest, DiffLineKind,
    GetProposalDiffRequest, GetProposalRequest, ListProposalsRequest, ListThreadsRequest, Proposal,
    ProposalGate, ProposalState, ReviewThread, SetThreadResolvedRequest, SubmitReviewRequest,
    UpdateProposalRequest, Verdict,
};
use serde_json::{Value, json};

/// Arguments of `scn proposal`.
#[derive(clap::Args)]
pub struct ProposalArgs {
    #[command(subcommand)]
    command: ProposalCommand,
}

#[derive(clap::Subcommand)]
enum ProposalCommand {
    /// Propose a changeset for a ref. Moves no ref.
    Create {
        /// Ref name or changeset ID: the changeset to propose.
        rev: String,
        /// The ref it should land on.
        #[arg(long, default_value = "main")]
        onto: String,
        /// One line for people.
        #[arg(long)]
        title: String,
        /// More words for people.
        #[arg(long, default_value = "")]
        description: String,
    },
    /// List proposals, lowest number first.
    List {
        /// `open`, `merged`, `closed` or `all`.
        #[arg(long, default_value = "open")]
        state: String,
        /// Only proposals for this ref.
        #[arg(long)]
        onto: Option<String>,
        /// How many to show.
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: u32,
    },
    /// Show a proposal: its head, reviews, and whether it may land.
    Show {
        /// Proposal number.
        number: u64,
    },
    /// Print the lines a proposal's head changes, as a unified diff.
    Diff {
        /// Proposal number.
        number: u64,
        /// One of the proposal's heads, by changeset ID; the current one
        /// when not given.
        #[arg(long)]
        head: Option<ChangesetId>,
        /// Lines of context around each hunk.
        #[arg(short = 'U', long, default_value_t = 3)]
        context: u32,
    },
    /// Give a proposal a new head, title or description.
    Update {
        /// Proposal number.
        number: u64,
        /// Ref name or changeset ID: the new head.
        #[arg(long)]
        head: Option<String>,
        /// The new title.
        #[arg(long)]
        title: Option<String>,
        /// The new description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Close a proposal without landing it.
    Close {
        /// Proposal number.
        number: u64,
    },
    /// Comment on a proposal: a general thread, a thread on a file or a
    /// line, or a reply to a thread.
    Comment {
        /// Proposal number.
        number: u64,
        /// The comment.
        #[arg(long)]
        body: String,
        /// A thread on this file.
        #[arg(long, value_parser = parse_repo_path, conflicts_with = "reply")]
        path: Option<RepoPath>,
        /// A thread on this line of the file, from 1, in the head.
        #[arg(long, requires = "path")]
        line: Option<u32>,
        /// The line counts in the base file, not the head.
        #[arg(long, requires = "line")]
        old_side: bool,
        /// Reply to this thread.
        #[arg(long)]
        reply: Option<u64>,
    },
    /// Mark a thread resolved, or active again.
    Resolve {
        /// Proposal number.
        number: u64,
        /// Thread number.
        thread: u64,
        /// Mark it active again.
        #[arg(long)]
        reopen: bool,
    },
    /// Print a proposal's threads and comments.
    Threads {
        /// Proposal number.
        number: u64,
    },
    /// Approve a proposal's head, ask for changes, or comment.
    Review {
        /// Proposal number.
        number: u64,
        /// Approve the head.
        #[arg(long, group = "verdict")]
        approve: bool,
        /// Block the land until you approve.
        #[arg(long, group = "verdict")]
        request_changes: bool,
        /// Neither approve nor block.
        #[arg(long, group = "verdict")]
        comment: bool,
        /// The head you reviewed, by changeset ID. Without it, the
        /// proposal's current head, which this prints.
        #[arg(long)]
        head: Option<ChangesetId>,
        /// Words for the author.
        #[arg(long, default_value = "")]
        body: String,
    },
}

/// Runs one `scn proposal` subcommand.
///
/// # Errors
///
/// A failed call or a bad argument.
#[allow(clippy::too_many_lines)] // one arm per subcommand
pub async fn proposal(ctx: &mut Ctx, args: ProposalArgs) -> Result<()> {
    let repo = ctx.repo.clone();
    match args.command {
        ProposalCommand::Create {
            rev,
            onto,
            title,
            description,
        } => {
            let head = ctx.resolve(&rev).await?;
            let p = ctx
                .review
                .create_proposal(CreateProposalRequest {
                    repo,
                    target_ref: onto,
                    changeset_id: head.as_bytes().to_vec(),
                    title,
                    description,
                })
                .await
                .context("proposal failed")?
                .into_inner();
            print_proposal(ctx.json, &p)
        }
        ProposalCommand::List { state, onto, limit } => list(ctx, &state, onto, limit).await,
        ProposalCommand::Show { number } => {
            let p = get(ctx, number).await?;
            print_proposal(ctx.json, &p)
        }
        ProposalCommand::Diff {
            number,
            head,
            context,
        } => diff(ctx, number, head, context).await,
        ProposalCommand::Update {
            number,
            head,
            title,
            description,
        } => {
            let head = match head {
                Some(rev) => ctx.resolve(&rev).await?.as_bytes().to_vec(),
                None => Vec::new(),
            };
            let p = ctx
                .review
                .update_proposal(UpdateProposalRequest {
                    repo,
                    number,
                    changeset_id: head,
                    title,
                    description,
                })
                .await
                .context("update failed")?
                .into_inner();
            print_proposal(ctx.json, &p)
        }
        ProposalCommand::Close { number } => {
            let p = ctx
                .review
                .close_proposal(CloseProposalRequest { repo, number })
                .await
                .context("close failed")?
                .into_inner();
            print_proposal(ctx.json, &p)
        }
        ProposalCommand::Comment {
            number,
            body,
            path,
            line,
            old_side,
            reply,
        } => {
            let t = ctx
                .review
                .add_comment(AddCommentRequest {
                    repo,
                    number,
                    thread_id: reply.unwrap_or(0),
                    path: path.map(|p| p.as_bytes().to_vec()).unwrap_or_default(),
                    line: line.unwrap_or(0),
                    old_side,
                    body,
                })
                .await
                .context("comment failed")?
                .into_inner();
            print_threads(ctx.json, &[t])
        }
        ProposalCommand::Resolve {
            number,
            thread,
            reopen,
        } => {
            let t = ctx
                .review
                .set_thread_resolved(SetThreadResolvedRequest {
                    repo,
                    number,
                    thread_id: thread,
                    resolved: !reopen,
                })
                .await
                .context("resolve failed")?
                .into_inner();
            print_threads(ctx.json, &[t])
        }
        ProposalCommand::Threads { number } => {
            let threads = ctx
                .review
                .list_threads(ListThreadsRequest { repo, number })
                .await
                .context("threads failed")?
                .into_inner()
                .threads;
            print_threads(ctx.json, &threads)
        }
        ProposalCommand::Review {
            number,
            approve,
            request_changes,
            comment,
            head,
            body,
        } => {
            let verdict = match (approve, request_changes, comment) {
                (true, _, _) => Verdict::Approve,
                (_, true, _) => Verdict::RequestChanges,
                (_, _, true) => Verdict::Comment,
                _ => bail!("give --approve, --request-changes or --comment"),
            };
            let seen = match head {
                Some(head) => head,
                None => id_from::<ChangesetId>(&get(ctx, number).await?.head_changeset_id)?,
            };
            if !ctx.json {
                outln!("reviewing head {seen}");
            }
            let p = ctx
                .review
                .submit_review(SubmitReviewRequest {
                    repo,
                    number,
                    verdict: verdict.into(),
                    changeset_id: seen.as_bytes().to_vec(),
                    body,
                })
                .await
                .context("review failed")?
                .into_inner();
            print_proposal(ctx.json, &p)
        }
    }
}

async fn get(ctx: &mut Ctx, number: u64) -> Result<Proposal> {
    Ok(ctx
        .review
        .get_proposal(GetProposalRequest {
            repo: ctx.repo.clone(),
            number,
        })
        .await
        .with_context(|| format!("proposal #{number}"))?
        .into_inner())
}

async fn list(ctx: &mut Ctx, state: &str, onto: Option<String>, limit: u32) -> Result<()> {
    let state = match state {
        "open" => ProposalState::Open,
        "merged" => ProposalState::Merged,
        "closed" => ProposalState::Closed,
        "all" => ProposalState::Unspecified,
        other => bail!("--state is open, merged, closed or all, not {other:?}"),
    };
    let mut after = 0;
    let mut shown: Vec<Proposal> = Vec::new();
    while shown.len() < limit as usize {
        let page = ctx
            .review
            .list_proposals(ListProposalsRequest {
                repo: ctx.repo.clone(),
                state: state.into(),
                target_ref: onto.clone().unwrap_or_default(),
                after_number: after,
                limit: u32::try_from((limit as usize - shown.len()).min(100)).unwrap_or(100),
            })
            .await
            .context("list failed")?
            .into_inner();
        shown.extend(page.proposals);
        if page.next_after_number == 0 {
            break;
        }
        after = page.next_after_number;
    }
    if ctx.json {
        let rows: Vec<Value> = shown.iter().map(proposal_json).collect();
        outln!("{}", Value::Array(rows));
        return Ok(());
    }
    for p in &shown {
        let gate = p.gate.clone().unwrap_or_default();
        let mark = match (p.state(), gate.approved) {
            (ProposalState::Open, true) => "ready",
            (ProposalState::Open, false) => "waiting",
            (state, _) => state_name(state),
        };
        outln!(
            "#{}\t{mark}\t{}\t{}\t{}",
            p.number,
            p.target_ref,
            p.author,
            p.title
        );
    }
    Ok(())
}

async fn diff(ctx: &mut Ctx, number: u64, head: Option<ChangesetId>, context: u32) -> Result<()> {
    let mut after = Vec::new();
    let mut files = Vec::new();
    loop {
        let page = ctx
            .review
            .get_proposal_diff(GetProposalDiffRequest {
                repo: ctx.repo.clone(),
                number,
                changeset_id: head.map(|h| h.as_bytes().to_vec()).unwrap_or_default(),
                path: Vec::new(),
                after: after.clone(),
                context_lines: Some(context),
            })
            .await
            .context("diff failed")?
            .into_inner();
        files.extend(page.files);
        if page.next_after.is_empty() {
            break;
        }
        after = page.next_after;
    }
    if ctx.json {
        let rows: Vec<Value> = files
            .iter()
            .map(|f| {
                let entry = f.entry.clone().unwrap_or_default();
                json!({
                    "path": lossy(&entry.path),
                    "rename_from": lossy(&entry.rename_from),
                    "binary": f.binary,
                    "too_large": f.too_large,
                    "hunks": f.hunks.iter().map(|h| json!({
                        "old_start": h.old_start,
                        "old_lines": h.old_lines,
                        "new_start": h.new_start,
                        "new_lines": h.new_lines,
                        "lines": h.lines.iter().map(|l| json!({
                            "kind": l.kind().as_str_name(),
                            "text": lossy(&l.text),
                            "old_line": l.old_line,
                            "new_line": l.new_line,
                        })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        outln!("{}", Value::Array(rows));
        return Ok(());
    }
    for f in &files {
        let entry = f.entry.clone().unwrap_or_default();
        let path = lossy(&entry.path);
        // The deleting half of a rename: its added half shows the change.
        if entry.new.is_none() && !entry.rename_to.is_empty() {
            continue;
        }
        let old = if !entry.rename_from.is_empty() {
            format!("a/{}", lossy(&entry.rename_from))
        } else if entry.old.is_some() {
            format!("a/{path}")
        } else {
            "/dev/null".to_owned()
        };
        let new = if entry.new.is_some() {
            format!("b/{path}")
        } else {
            "/dev/null".to_owned()
        };
        outln!("--- {old}");
        outln!("+++ {new}");
        if f.binary {
            outln!("Binary files differ");
        } else if f.too_large {
            outln!("File too large to diff");
        }
        for h in &f.hunks {
            outln!(
                "@@ -{},{} +{},{} @@",
                h.old_start,
                h.old_lines,
                h.new_start,
                h.new_lines
            );
            for l in &h.lines {
                let sign = match l.kind() {
                    DiffLineKind::Added => '+',
                    DiffLineKind::Removed => '-',
                    _ => ' ',
                };
                outln!("{sign}{}", lossy(&l.text));
            }
        }
    }
    Ok(())
}

fn state_name(state: ProposalState) -> &'static str {
    match state {
        ProposalState::Open => "open",
        ProposalState::Merged => "merged",
        ProposalState::Closed => "closed",
        ProposalState::Unspecified => "unknown",
    }
}

fn hex(bytes: &[u8]) -> String {
    id_from::<ChangesetId>(bytes).map_or_else(|_| String::new(), |id| id.to_string())
}

fn gate_json(gate: &ProposalGate) -> Value {
    json!({
        "protected": gate.protected,
        "required_approvals": gate.required_approvals,
        "approved_by": gate.approved_by,
        "changes_requested_by": gate.changes_requested_by,
        "approved": gate.approved,
        "reason": gate.reason,
    })
}

fn proposal_json(p: &Proposal) -> Value {
    json!({
        "number": p.number,
        "target_ref": p.target_ref,
        "title": p.title,
        "description": p.description,
        "author": p.author,
        "state": state_name(p.state()),
        "head": hex(&p.head_changeset_id),
        "merged": (!p.merged_changeset_id.is_empty()).then(|| hex(&p.merged_changeset_id)),
        "created_unix_secs": p.created_unix_secs,
        "updated_unix_secs": p.updated_unix_secs,
        "heads": p.heads.iter().map(|h| json!({
            "changeset": hex(&h.changeset_id),
            "pushed_by": h.pushed_by,
            "unix_secs": h.unix_secs,
        })).collect::<Vec<_>>(),
        "reviews": p.reviews.iter().map(|r| json!({
            "reviewer": r.reviewer,
            "verdict": r.verdict().as_str_name(),
            "changeset": hex(&r.changeset_id),
            "body": r.body,
            "unix_secs": r.unix_secs,
        })).collect::<Vec<_>>(),
        "gate": gate_json(&p.gate.clone().unwrap_or_default()),
    })
}

fn print_proposal(json: bool, p: &Proposal) -> Result<()> {
    if json {
        outln!("{}", proposal_json(p));
        return Ok(());
    }
    outln!("#{} {}", p.number, p.title);
    outln!(
        "{} by {} for {}",
        state_name(p.state()),
        p.author,
        p.target_ref
    );
    outln!("head {}", hex(&p.head_changeset_id));
    if !p.merged_changeset_id.is_empty() {
        outln!("merged as {}", hex(&p.merged_changeset_id));
    }
    if !p.description.is_empty() {
        outln!();
        for line in p.description.lines() {
            outln!("    {line}");
        }
    }
    for r in &p.reviews {
        let verdict = match r.verdict() {
            Verdict::Approve => "approved",
            Verdict::RequestChanges => "requested changes",
            _ => "commented",
        };
        let on = if r.changeset_id == p.head_changeset_id {
            "the head".to_owned()
        } else {
            format!("an older head {}", hex(&r.changeset_id))
        };
        outln!("{} {verdict} on {on}", r.reviewer);
    }
    let gate = p.gate.clone().unwrap_or_default();
    if gate.approved {
        outln!(
            "may land{}",
            if gate.protected {
                ""
            } else {
                " (the ref is not protected)"
            }
        );
    } else {
        outln!("may not land: {}", gate.reason);
    }
    Ok(())
}

fn print_threads(json: bool, threads: &[ReviewThread]) -> Result<()> {
    if json {
        let rows: Vec<Value> = threads
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "path": lossy(&t.path),
                    "line": t.line,
                    "old_side": t.old_side,
                    "changeset": hex(&t.changeset_id),
                    "resolved": t.resolved,
                    "outdated": t.outdated,
                    "comments": t.comments.iter().map(|c| json!({
                        "id": c.id,
                        "author": c.author,
                        "body": c.body,
                        "unix_secs": c.unix_secs,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        outln!("{}", Value::Array(rows));
        return Ok(());
    }
    for t in threads {
        let place = if t.path.is_empty() {
            "general".to_owned()
        } else if t.line == 0 {
            lossy(&t.path).into_owned()
        } else {
            format!(
                "{}:{}{}",
                lossy(&t.path),
                t.line,
                if t.old_side { " (base)" } else { "" }
            )
        };
        let mut marks = Vec::new();
        if t.resolved {
            marks.push("resolved");
        }
        if t.outdated {
            marks.push("outdated");
        }
        let marks = if marks.is_empty() {
            String::new()
        } else {
            format!(" [{}]", marks.join(", "))
        };
        outln!("thread {} on {place}{marks}", t.id);
        for c in &t.comments {
            outln!("  {}: {}", c.author, c.body);
        }
    }
    Ok(())
}
