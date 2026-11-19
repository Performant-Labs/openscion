//! Proposals and their review: a change is proposed, reviewed and approved
//! before it lands on a protected ref (the fork's issue #2).
//!
//! A proposal names a changeset (its *head*) and the ref it should land
//! on. It moves no ref: `Land` does. On a ref that the access file
//! protects, `Land` lands a changeset only while an open proposal for
//! that ref has it as its head and the proposal passes its [`gate`]:
//! enough approvals of that head from principals with a `review` rule
//! (its author's own never counts), and no reviewer whose latest
//! approving-or-blocking review asks for changes. The refusal is
//! `PERMISSION_DENIED` before the land records anything, so a retry with
//! the same request key lands once the proposal is approved. The land
//! that moves the ref marks the proposal merged in the same transaction.
//!
//! Reviews bind to a changeset ID, which names immutable bytes: what was
//! approved is what lands. A new head starts the approvals over; threads
//! stay, and show as outdated once the head moved past the one they were
//! started on.

// Every pub item of this module is re-exported by the test seam
// (`test_support` in lib.rs), and unreachable_pub does not see through a
// re-export, as in `meta.rs`.
#![allow(unreachable_pub)]

use std::collections::{BTreeMap, BTreeSet};

use gix::diff::blob::sources::byte_lines;
use gix::diff::blob::{Algorithm, Diff, InternedInput};
use scion_core::{ChangesetId, FileState, RepoPath};

use crate::auth::Caller;
use crate::backend::Backend;
use crate::deadline::Deadline;
use crate::error::{Error, Result};
use crate::meta::{Meta as _, MetaTx, RepoId};
use crate::read::{self, DiffPage};

/// Longest proposal title, in bytes.
pub const MAX_TITLE_BYTES: usize = 256;
/// Longest proposal description, in bytes.
pub const MAX_DESCRIPTION_BYTES: usize = 64 << 10;
/// Longest comment or review body, in bytes.
pub const MAX_BODY_BYTES: usize = 16 << 10;
/// Most comments one proposal holds: with [`MAX_BODY_BYTES`] each, a
/// `ListThreads` reply stays far below the message limit.
pub const MAX_COMMENTS: usize = 2000;
/// Most heads one proposal may have had.
pub const MAX_HEADS: usize = 1000;
/// Most reviews one proposal holds.
pub const MAX_REVIEWS: usize = 1000;
/// Most proposals one `ListProposals` page returns, and the default.
pub const MAX_LIST: usize = 100;
/// The page size when the request names none.
pub const DEFAULT_LIST: usize = 50;
/// The context lines of a hunk when the request names none, and the most
/// it may ask for.
pub const DEFAULT_CONTEXT: u32 = 3;
/// The most context lines a request may ask for.
pub const MAX_CONTEXT: u32 = 100;
/// A file side larger than this gets no hunks (`too_large`).
pub const MAX_DIFF_FILE_BYTES: usize = 1 << 20;
/// The hunk text one `GetProposalDiff` reply carries at most. The page
/// stops after the file that crosses it, and `next_after` goes on.
pub const MAX_DIFF_REPLY_BYTES: usize = 16 << 20;

/// Where a proposal is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalStatus {
    /// It may still land.
    Open,
    /// A land moved its ref to its head, rebased.
    Merged,
    /// Closed without landing.
    Closed,
}

impl ProposalStatus {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }

    /// The stored spelling back.
    ///
    /// # Errors
    ///
    /// [`Error::Corrupt`] for a spelling no build wrote.
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "open" => Ok(Self::Open),
            "merged" => Ok(Self::Merged),
            "closed" => Ok(Self::Closed),
            other => Err(Error::Corrupt(format!("proposal state {other:?}"))),
        }
    }
}

/// What a review says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerdictKind {
    /// The head may land.
    Approve,
    /// Blocks the land until the same reviewer approves.
    RequestChanges,
    /// Neither approves nor blocks.
    Comment,
}

impl VerdictKind {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::RequestChanges => "request_changes",
            Self::Comment => "comment",
        }
    }

    /// The stored spelling back.
    ///
    /// # Errors
    ///
    /// [`Error::Corrupt`] for a spelling no build wrote.
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "approve" => Ok(Self::Approve),
            "request_changes" => Ok(Self::RequestChanges),
            "comment" => Ok(Self::Comment),
            other => Err(Error::Corrupt(format!("review verdict {other:?}"))),
        }
    }
}

/// One proposal row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalRow {
    /// From 1 in its repo.
    pub number: i64,
    /// The ref it should land on.
    pub target_ref: String,
    /// For people.
    pub title: String,
    /// For people.
    pub description: String,
    /// The principal that opened it.
    pub author: String,
    /// The changeset that would land.
    pub head: ChangesetId,
    /// Open, merged or closed.
    pub state: ProposalStatus,
    /// When MERGED: what the land put at the tip of the ref.
    pub merged: Option<ChangesetId>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

/// One head a proposal has had.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadRow {
    /// From 1 per proposal.
    pub seq: i64,
    /// The head.
    pub changeset: ChangesetId,
    /// The principal that set it.
    pub pushed_by: String,
    /// Unix seconds.
    pub at: i64,
}

/// One review of a proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewRow {
    /// From 1 per proposal.
    pub seq: i64,
    /// Who reviewed.
    pub reviewer: String,
    /// What the review says.
    pub verdict: VerdictKind,
    /// The head it was of.
    pub changeset: ChangesetId,
    /// For people.
    pub body: String,
    /// Unix seconds.
    pub at: i64,
}

/// One review thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadRow {
    /// From 1 per proposal.
    pub thread: i64,
    /// The file; empty for a general thread.
    pub path: Vec<u8>,
    /// The line from 1; 0 for a general thread or a whole file.
    pub line: u32,
    /// `line` counts in the base file.
    pub old_side: bool,
    /// The head the thread was started on.
    pub anchor: ChangesetId,
    /// Resolved, or still active.
    pub resolved: bool,
    /// Unix seconds.
    pub created_at: i64,
}

/// One comment of a thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentRow {
    /// From 1 per proposal.
    pub comment: i64,
    /// The thread it is in.
    pub thread: i64,
    /// Who wrote it.
    pub author: String,
    /// For people.
    pub body: String,
    /// Unix seconds.
    pub at: i64,
}

/// The review rules of one repo, from the access file: which principals'
/// reviews count, and how many approvals a protected ref needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoRules {
    /// The principals with a `review` rule for the repo.
    pub reviewers: BTreeSet<String>,
    /// The protected refs, each with the approvals it needs.
    pub protected: BTreeMap<String, u32>,
}

impl RepoRules {
    /// Whether `principal`'s approvals and blocks count.
    #[must_use]
    pub fn is_reviewer(&self, principal: &str) -> bool {
        self.reviewers.contains(principal)
    }

    /// The approvals `target_ref` needs, or `None` if it is not protected.
    #[must_use]
    pub fn protection(&self, target_ref: &str) -> Option<u32> {
        self.protected.get(target_ref).copied()
    }
}

/// What `Land` would decide about a proposal's head now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gate {
    /// The target ref has a protect rule.
    pub protected: bool,
    /// The approvals that rule asks for.
    pub required: u32,
    /// The reviewers whose approval of the head counts, sorted.
    pub approved_by: Vec<String>,
    /// The reviewers whose latest blocking-or-approving review asks for
    /// changes, sorted.
    pub changes_requested_by: Vec<String>,
    /// Open, enough approvals, nothing blocking.
    pub approved: bool,
    /// Why not, for people; empty when `approved`.
    pub reason: String,
}

/// The gate of `proposal`, given its reviews and the repo's rules. A pure
/// function, so the `GetProposal` answer and the land's decision are the
/// same computation.
#[must_use]
pub fn gate(proposal: &ProposalRow, reviews: &[ReviewRow], rules: &RepoRules) -> Gate {
    let required = rules.protection(&proposal.target_ref);
    // The latest approve-or-block review per counted reviewer. A comment
    // review changes nothing, as on GitHub.
    let mut latest: BTreeMap<&str, &ReviewRow> = BTreeMap::new();
    for review in reviews {
        if review.verdict == VerdictKind::Comment
            || review.reviewer == proposal.author
            || !rules.is_reviewer(&review.reviewer)
        {
            continue;
        }
        latest.insert(review.reviewer.as_str(), review);
    }
    let approved_by: Vec<String> = latest
        .values()
        .filter(|r| r.verdict == VerdictKind::Approve && r.changeset == proposal.head)
        .map(|r| r.reviewer.clone())
        .collect();
    let changes_requested_by: Vec<String> = latest
        .values()
        .filter(|r| r.verdict == VerdictKind::RequestChanges)
        .map(|r| r.reviewer.clone())
        .collect();
    let needed = required.unwrap_or(0);
    let reason = if proposal.state != ProposalStatus::Open {
        format!(
            "proposal #{} is {}",
            proposal.number,
            proposal.state.as_str()
        )
    } else if !changes_requested_by.is_empty() {
        format!(
            "proposal #{}: changes requested by {}",
            proposal.number,
            changes_requested_by.join(", ")
        )
    } else if (approved_by.len() as u64) < u64::from(needed) {
        format!(
            "proposal #{} needs {needed} approval(s) of its head from reviewers other than \
             its author, and has {}",
            proposal.number,
            approved_by.len()
        )
    } else {
        String::new()
    };
    Gate {
        protected: required.is_some(),
        required: needed,
        approved: reason.is_empty(),
        approved_by,
        changes_requested_by,
        reason,
    }
}

/// The land gate of a protected ref, inside the land's transaction: the
/// open proposal of `target_ref` whose head is `changeset` must pass its
/// [`gate`]. Returns that proposal's number, or `None` when the ref is
/// not protected.
///
/// # Errors
///
/// [`Error::PermissionDenied`] naming why the land may not move the ref.
/// Store errors otherwise.
pub fn check_land<T: MetaTx + ?Sized>(
    c: &mut T,
    repo: RepoId,
    target_ref: &str,
    changeset: ChangesetId,
    rules: &RepoRules,
) -> Result<Option<i64>> {
    if rules.protection(target_ref).is_none() {
        return Ok(None);
    }
    let Some(number) = c.open_proposal_with_head(repo, target_ref, changeset)? else {
        return Err(Error::PermissionDenied(format!(
            "ref {target_ref:?} is protected: changeset {} is not the head of an open \
             proposal for it; propose it, get it approved, then land it",
            hex(changeset.as_bytes())
        )));
    };
    let proposal = c
        .proposal(repo, number)?
        .ok_or_else(|| Error::Corrupt(format!("proposal #{number} vanished")))?;
    let reviews = c.proposal_reviews(repo, number)?;
    let gate = gate(&proposal, &reviews, rules);
    if gate.approved {
        Ok(Some(number))
    } else {
        Err(Error::PermissionDenied(format!(
            "ref {target_ref:?} is protected: {}",
            gate.reason
        )))
    }
}

/// Marks every open proposal of `target_ref` whose head is `changeset`
/// merged into `result`, in the land's transaction.
///
/// # Errors
///
/// Store errors.
pub fn mark_merged<T: MetaTx + ?Sized>(
    c: &mut T,
    repo: RepoId,
    target_ref: &str,
    changeset: ChangesetId,
    result: ChangesetId,
    now: i64,
) -> Result<()> {
    c.merge_open_proposals(repo, target_ref, changeset, result, now)?;
    Ok(())
}

/// The review part of the transaction that moves a ref: the gate of a
/// protected ref again (a review that blocked the proposal after the land
/// was recorded stops the move, and the land stays pending), then every
/// open proposal of the ref whose head this land moved is marked merged,
/// protected ref or not.
///
/// # Errors
///
/// [`Error::PermissionDenied`] from the gate; store errors.
pub fn settle<T: MetaTx + ?Sized>(
    c: &mut T,
    repo: RepoId,
    target_ref: &str,
    changeset: ChangesetId,
    result: ChangesetId,
    rules: Option<&RepoRules>,
) -> Result<()> {
    if let Some(rules) = rules {
        check_land(c, repo, target_ref, changeset, rules)?;
    }
    mark_merged(c, repo, target_ref, changeset, result, now())
}

/// One proposal with everything `GetProposal` answers.
#[derive(Clone, Debug)]
pub struct ProposalView {
    /// The row.
    pub row: ProposalRow,
    /// Every head, oldest first.
    pub heads: Vec<HeadRow>,
    /// Every review, oldest first.
    pub reviews: Vec<ReviewRow>,
    /// What `Land` would decide now.
    pub gate: Gate,
}

/// One thread with its comments.
#[derive(Clone, Debug)]
pub struct ThreadView {
    /// The thread.
    pub row: ThreadRow,
    /// Oldest first.
    pub comments: Vec<CommentRow>,
    /// The proposal's head moved past the thread's anchor.
    pub outdated: bool,
}

/// What a new or updated proposal names.
#[derive(Clone, Debug)]
pub struct NewProposal {
    /// The ref it should land on.
    pub target_ref: String,
    /// The head.
    pub head: ChangesetId,
    /// For people.
    pub title: String,
    /// For people.
    pub description: String,
}

/// The current time, in Unix seconds.
#[must_use]
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn check_text(what: &str, text: &str, min: usize, max: usize) -> Result<()> {
    if text.len() < min || text.len() > max {
        return Err(Error::InvalidArgument(format!(
            "{what} is {} bytes; it must be {min} to {max}",
            text.len()
        )));
    }
    Ok(())
}

/// A head must be a changeset of the repo with one parent: the parent is
/// the base its review diffs against, and the land rebases it from there.
fn check_head<T: MetaTx + ?Sized>(c: &mut T, repo: RepoId, head: ChangesetId) -> Result<()> {
    let cs = c.get_changeset(repo, head)?;
    if cs.parents().len() != 1 {
        return Err(Error::InvalidArgument(format!(
            "changeset {} has {} parents; a proposal's head has exactly one",
            hex(head.as_bytes()),
            cs.parents().len()
        )));
    }
    Ok(())
}

/// No two open proposals of one ref share a head, so the land gate's
/// lookup names one proposal.
fn check_head_unclaimed<T: MetaTx + ?Sized>(
    c: &mut T,
    repo: RepoId,
    target_ref: &str,
    head: ChangesetId,
) -> Result<()> {
    if let Some(other) = c.open_proposal_with_head(repo, target_ref, head)? {
        return Err(Error::FailedPrecondition(format!(
            "proposal #{other} already proposes changeset {} for ref {target_ref:?}",
            hex(head.as_bytes())
        )));
    }
    Ok(())
}

fn load<T: MetaTx + ?Sized>(c: &mut T, repo: RepoId, number: i64) -> Result<ProposalRow> {
    c.proposal(repo, number)?
        .ok_or_else(|| Error::NotFound(format!("proposal #{number}")))
}

fn must_be_open(row: &ProposalRow) -> Result<()> {
    if row.state == ProposalStatus::Open {
        Ok(())
    } else {
        Err(Error::FailedPrecondition(format!(
            "proposal #{} is {}",
            row.number,
            row.state.as_str()
        )))
    }
}

/// Only its author or a reviewer may change a proposal's head, text or
/// state.
fn may_manage(row: &ProposalRow, caller: &Caller, rules: &RepoRules) -> Result<()> {
    if row.author == caller.0 || rules.is_reviewer(&caller.0) {
        Ok(())
    } else {
        Err(Error::PermissionDenied(format!(
            "principal {:?} is neither the author of proposal #{} nor a reviewer",
            caller.0, row.number
        )))
    }
}

/// Opens a proposal; returns its number.
///
/// # Errors
///
/// [`Error::NotFound`] for a missing repo, ref or changeset;
/// [`Error::InvalidArgument`] for a bad title, description or head;
/// [`Error::FailedPrecondition`] if an open proposal of the ref has this
/// head already.
pub fn create(store: &Backend, caller: &Caller, repo: &str, new: NewProposal) -> Result<i64> {
    check_text("title", &new.title, 1, MAX_TITLE_BYTES)?;
    check_text("description", &new.description, 0, MAX_DESCRIPTION_BYTES)?;
    let at = now();
    store.write(|c| {
        let repo = c.repo_id(repo)?;
        if c.get_ref(repo, &new.target_ref)?.is_none() {
            return Err(Error::NotFound(format!("ref {:?}", new.target_ref)));
        }
        check_head(c, repo, new.head)?;
        check_head_unclaimed(c, repo, &new.target_ref, new.head)?;
        c.insert_proposal(
            repo,
            &ProposalRow {
                number: 0,
                target_ref: new.target_ref,
                title: new.title,
                description: new.description,
                author: caller.0.clone(),
                head: new.head,
                state: ProposalStatus::Open,
                merged: None,
                created_at: at,
                updated_at: at,
            },
        )
    })
}

/// Gives an open proposal a new head, title or description.
///
/// # Errors
///
/// As [`create`]; [`Error::NotFound`] for a missing proposal;
/// [`Error::FailedPrecondition`] if it is not open;
/// [`Error::PermissionDenied`] if the caller is neither its author nor a
/// reviewer.
#[allow(clippy::too_many_arguments)] // one per request field
pub fn update(
    store: &Backend,
    caller: &Caller,
    repo: &str,
    rules: &RepoRules,
    number: i64,
    head: Option<ChangesetId>,
    title: Option<String>,
    description: Option<String>,
) -> Result<()> {
    if let Some(title) = &title {
        check_text("title", title, 1, MAX_TITLE_BYTES)?;
    }
    if let Some(description) = &description {
        check_text("description", description, 0, MAX_DESCRIPTION_BYTES)?;
    }
    let at = now();
    store.write(|c| {
        let repo = c.repo_id(repo)?;
        let mut row = load(c, repo, number)?;
        may_manage(&row, caller, rules)?;
        must_be_open(&row)?;
        if let Some(head) = head.filter(|head| *head != row.head) {
            check_head(c, repo, head)?;
            check_head_unclaimed(c, repo, &row.target_ref, head)?;
            if c.proposal_heads(repo, number)?.len() >= MAX_HEADS {
                return Err(Error::ResourceExhausted(format!(
                    "proposal #{number} has had {MAX_HEADS} heads; open a new one"
                )));
            }
            c.add_proposal_head(
                repo,
                number,
                &HeadRow {
                    seq: 0,
                    changeset: head,
                    pushed_by: caller.0.clone(),
                    at,
                },
            )?;
            row.head = head;
        }
        if let Some(title) = title {
            row.title = title;
        }
        if let Some(description) = description {
            row.description = description;
        }
        row.updated_at = at;
        c.put_proposal(repo, &row)
    })
}

/// Closes an open proposal without landing it.
///
/// # Errors
///
/// As [`update`].
pub fn close(
    store: &Backend,
    caller: &Caller,
    repo: &str,
    rules: &RepoRules,
    number: i64,
) -> Result<()> {
    let at = now();
    store.write(|c| {
        let repo = c.repo_id(repo)?;
        let mut row = load(c, repo, number)?;
        may_manage(&row, caller, rules)?;
        must_be_open(&row)?;
        row.state = ProposalStatus::Closed;
        row.updated_at = at;
        c.put_proposal(repo, &row)
    })
}

/// Records a review of the proposal's head.
///
/// # Errors
///
/// [`Error::NotFound`] for a missing proposal;
/// [`Error::FailedPrecondition`] if it is not open, if `seen` is not its
/// head, or if its author approves or blocks it;
/// [`Error::PermissionDenied`] if a principal without a `review` rule
/// approves or blocks; [`Error::InvalidArgument`] for a bad body.
#[allow(clippy::too_many_arguments)] // one per request field
pub fn submit_review(
    store: &Backend,
    caller: &Caller,
    repo: &str,
    rules: &RepoRules,
    number: i64,
    verdict: VerdictKind,
    seen: ChangesetId,
    body: String,
) -> Result<()> {
    check_text("body", &body, 0, MAX_BODY_BYTES)?;
    let at = now();
    store.write(|c| {
        let repo = c.repo_id(repo)?;
        let row = load(c, repo, number)?;
        must_be_open(&row)?;
        if seen != row.head {
            return Err(Error::FailedPrecondition(format!(
                "proposal #{number}'s head is {}, not {}: review the head",
                hex(row.head.as_bytes()),
                hex(seen.as_bytes())
            )));
        }
        if verdict != VerdictKind::Comment {
            if row.author == caller.0 {
                return Err(Error::FailedPrecondition(format!(
                    "principal {:?} opened proposal #{number}, and may not approve or block it",
                    caller.0
                )));
            }
            if !rules.is_reviewer(&caller.0) {
                return Err(Error::PermissionDenied(format!(
                    "principal {:?} has no review rule for this repo",
                    caller.0
                )));
            }
        }
        if c.proposal_reviews(repo, number)?.len() >= MAX_REVIEWS {
            return Err(Error::ResourceExhausted(format!(
                "proposal #{number} holds {MAX_REVIEWS} reviews"
            )));
        }
        c.add_proposal_review(
            repo,
            number,
            &ReviewRow {
                seq: 0,
                reviewer: caller.0.clone(),
                verdict,
                changeset: seen,
                body,
                at,
            },
        )
    })
}

/// Where a new thread goes.
#[derive(Clone, Debug, Default)]
pub struct Anchor {
    /// The file; `None` for a general thread.
    pub path: Option<RepoPath>,
    /// The line from 1, or 0 for the whole file.
    pub line: u32,
    /// `line` counts in the base file.
    pub old_side: bool,
}

/// Starts a thread (`thread == 0`) or replies to one; returns the thread's
/// number.
///
/// # Errors
///
/// [`Error::NotFound`] for a missing proposal or thread;
/// [`Error::InvalidArgument`] for a bad body or anchor;
/// [`Error::ResourceExhausted`] once the proposal holds
/// [`MAX_COMMENTS`].
pub fn comment(
    store: &Backend,
    caller: &Caller,
    repo: &str,
    number: i64,
    thread: i64,
    anchor: &Anchor,
    body: String,
) -> Result<i64> {
    check_text("body", &body, 1, MAX_BODY_BYTES)?;
    if anchor.path.is_none() && (anchor.line != 0 || anchor.old_side) {
        return Err(Error::InvalidArgument(
            "a line or a side needs a path; a general thread has neither".into(),
        ));
    }
    let at = now();
    store.write(|c| {
        let repo = c.repo_id(repo)?;
        let row = load(c, repo, number)?;
        if usize::try_from(c.review_comment_count(repo, number)?).unwrap_or(usize::MAX)
            >= MAX_COMMENTS
        {
            return Err(Error::ResourceExhausted(format!(
                "proposal #{number} holds {MAX_COMMENTS} comments"
            )));
        }
        let thread = if thread == 0 {
            c.add_review_thread(
                repo,
                number,
                &ThreadRow {
                    thread: 0,
                    path: anchor
                        .path
                        .as_ref()
                        .map(|p| p.as_bytes().to_vec())
                        .unwrap_or_default(),
                    line: anchor.line,
                    old_side: anchor.old_side,
                    anchor: row.head,
                    resolved: false,
                    created_at: at,
                },
            )?
        } else {
            if !c
                .review_threads(repo, number)?
                .iter()
                .any(|t| t.thread == thread)
            {
                return Err(Error::NotFound(format!(
                    "thread {thread} of proposal #{number}"
                )));
            }
            thread
        };
        c.add_review_comment(
            repo,
            number,
            &CommentRow {
                comment: 0,
                thread,
                author: caller.0.clone(),
                body,
                at,
            },
        )?;
        Ok(thread)
    })
}

/// Marks a thread resolved or active.
///
/// # Errors
///
/// [`Error::NotFound`] for a missing proposal or thread.
pub fn set_resolved(
    store: &Backend,
    repo: &str,
    number: i64,
    thread: i64,
    resolved: bool,
) -> Result<()> {
    store.write(|c| {
        let repo = c.repo_id(repo)?;
        load(c, repo, number)?;
        if c.set_review_thread_resolved(repo, number, thread, resolved)? {
            Ok(())
        } else {
            Err(Error::NotFound(format!(
                "thread {thread} of proposal #{number}"
            )))
        }
    })
}

/// One proposal with its heads, reviews and gate.
///
/// # Errors
///
/// [`Error::NotFound`] for a missing repo or proposal.
pub fn view(store: &Backend, repo: &str, rules: &RepoRules, number: i64) -> Result<ProposalView> {
    store.read(|c| {
        let repo = c.repo_id(repo)?;
        let row = load(c, repo, number)?;
        let heads = c.proposal_heads(repo, number)?;
        let reviews = c.proposal_reviews(repo, number)?;
        let gate = gate(&row, &reviews, rules);
        Ok(ProposalView {
            row,
            heads,
            reviews,
            gate,
        })
    })
}

/// A page of proposals, with each one's gate, and whether more may follow.
///
/// # Errors
///
/// [`Error::NotFound`] for a missing repo.
pub fn list(
    store: &Backend,
    repo: &str,
    rules: &RepoRules,
    state: Option<ProposalStatus>,
    target_ref: Option<&str>,
    after: i64,
    limit: usize,
) -> Result<(Vec<(ProposalRow, Gate)>, bool)> {
    let limit = match limit {
        0 => DEFAULT_LIST,
        n => n.min(MAX_LIST),
    };
    store.read(|c| {
        let repo = c.repo_id(repo)?;
        let mut rows = c.proposals(repo, state, target_ref, after, limit + 1)?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let mut page = Vec::with_capacity(rows.len());
        for row in rows {
            let reviews = c.proposal_reviews(repo, row.number)?;
            let gate = gate(&row, &reviews, rules);
            page.push((row, gate));
        }
        Ok((page, more))
    })
}

/// Every thread of a proposal, with its comments.
///
/// # Errors
///
/// [`Error::NotFound`] for a missing repo or proposal.
pub fn threads(store: &Backend, repo: &str, number: i64) -> Result<Vec<ThreadView>> {
    store.read(|c| {
        let repo = c.repo_id(repo)?;
        let row = load(c, repo, number)?;
        let threads = c.review_threads(repo, number)?;
        let mut comments: BTreeMap<i64, Vec<CommentRow>> = BTreeMap::new();
        for comment in c.review_comments(repo, number)? {
            comments.entry(comment.thread).or_default().push(comment);
        }
        Ok(threads
            .into_iter()
            .map(|thread| ThreadView {
                outdated: thread.anchor != row.head,
                comments: comments.remove(&thread.thread).unwrap_or_default(),
                row: thread,
            })
            .collect())
    })
}

/// One thread, for the reply of a call that changed it.
///
/// # Errors
///
/// [`Error::NotFound`] for a missing proposal or thread.
pub fn thread(store: &Backend, repo: &str, number: i64, thread: i64) -> Result<ThreadView> {
    threads(store, repo, number)?
        .into_iter()
        .find(|t| t.row.thread == thread)
        .ok_or_else(|| Error::NotFound(format!("thread {thread} of proposal #{number}")))
}

/// What one line of a hunk is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// On both sides.
    Context,
    /// In the head only.
    Added,
    /// In the base only.
    Removed,
}

/// One line of a hunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HunkLine {
    /// Context, added or removed.
    pub kind: LineKind,
    /// Without its line ending.
    pub text: Vec<u8>,
    /// From 1 in the base; 0 for an added line.
    pub old_line: u32,
    /// From 1 in the head; 0 for a removed line.
    pub new_line: u32,
}

/// One hunk, as a unified diff numbers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// First base line (from 1; 0 when the base side is empty).
    pub old_start: u32,
    /// Base lines in the hunk.
    pub old_lines: u32,
    /// First head line (from 1; 0 when the head side is empty).
    pub new_start: u32,
    /// Head lines in the hunk.
    pub new_lines: u32,
    /// The lines, in order.
    pub lines: Vec<HunkLine>,
}

/// One file of a proposal's diff.
#[derive(Clone, Debug)]
pub struct FileHunks {
    /// The path entry, as `Diff` gives it.
    pub entry: read::DiffEntry,
    /// The hunks of a text file that changed.
    pub hunks: Vec<Hunk>,
    /// A side is not text.
    pub binary: bool,
    /// A side is over [`MAX_DIFF_FILE_BYTES`].
    pub too_large: bool,
}

/// One page of a proposal's diff.
#[derive(Clone, Debug)]
pub struct ProposalDiff {
    /// The head's first parent.
    pub base: ChangesetId,
    /// The head diffed.
    pub head: ChangesetId,
    /// The files, in tree order.
    pub files: Vec<FileHunks>,
    /// Where the next page starts.
    pub next_after: Option<RepoPath>,
    /// As [`DiffPage::renames_incomplete`].
    pub renames_incomplete: bool,
}

fn line_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn strip_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// The hunks between two texts, with `context` lines around each change,
/// and hunks whose context touches merged, as `diff -U` does.
#[must_use]
pub fn hunks(old: &[u8], new: &[u8], context: u32) -> Vec<Hunk> {
    let old_lines: Vec<&[u8]> = byte_lines(old).collect();
    let new_lines: Vec<&[u8]> = byte_lines(new).collect();
    let input = InternedInput::new(old, new);
    let mut diff = Diff::compute(Algorithm::Myers, &input);
    diff.postprocess_lines(&input);
    let changes: Vec<(std::ops::Range<u32>, std::ops::Range<u32>)> =
        diff.hunks().map(|h| (h.before, h.after)).collect();
    let mut out: Vec<Hunk> = Vec::new();
    let mut i = 0;
    while i < changes.len() {
        // Merge the changes whose context lines touch or overlap.
        let mut j = i;
        while j + 1 < changes.len()
            && changes[j + 1].0.start.saturating_sub(changes[j].0.end) <= 2 * context
        {
            j += 1;
        }
        let first = &changes[i];
        let last = &changes[j];
        let old_from = first.0.start.saturating_sub(context);
        let old_to = (last.0.end + context).min(line_u32(old_lines.len()));
        // The head side starts as far before the first change as the base
        // side does: those lines are context, equal on both sides.
        let new_from = first.1.start - (first.0.start - old_from);
        let mut lines = Vec::new();
        let (mut o, mut n) = (old_from, new_from);
        for change in &changes[i..=j] {
            while o < change.0.start {
                lines.push(HunkLine {
                    kind: LineKind::Context,
                    text: strip_eol(old_lines[o as usize]).to_vec(),
                    old_line: o + 1,
                    new_line: n + 1,
                });
                o += 1;
                n += 1;
            }
            for k in change.0.clone() {
                lines.push(HunkLine {
                    kind: LineKind::Removed,
                    text: strip_eol(old_lines[k as usize]).to_vec(),
                    old_line: k + 1,
                    new_line: 0,
                });
            }
            for k in change.1.clone() {
                lines.push(HunkLine {
                    kind: LineKind::Added,
                    text: strip_eol(new_lines[k as usize]).to_vec(),
                    old_line: 0,
                    new_line: k + 1,
                });
            }
            o = change.0.end;
            n = change.1.end;
        }
        while o < old_to {
            lines.push(HunkLine {
                kind: LineKind::Context,
                text: strip_eol(old_lines[o as usize]).to_vec(),
                old_line: o + 1,
                new_line: n + 1,
            });
            o += 1;
            n += 1;
        }
        let old_count = old_to - old_from;
        let new_count = n - new_from;
        out.push(Hunk {
            old_start: if old_count == 0 {
                old_from
            } else {
                old_from + 1
            },
            old_lines: old_count,
            new_start: if new_count == 0 {
                new_from
            } else {
                new_from + 1
            },
            new_lines: new_count,
            lines,
        });
        i = j + 1;
    }
    out
}

/// The text bytes of a regular file's blob, or why there are no hunks.
enum Side {
    Text(Vec<u8>),
    Binary,
    TooLarge,
}

fn side(store: &Backend, state: Option<&FileState>) -> Result<Option<Side>> {
    let Some(FileState::Regular { content, .. }) = state else {
        return Ok(None);
    };
    if store.blobs().size(*content)? > MAX_DIFF_FILE_BYTES {
        return Ok(Some(Side::TooLarge));
    }
    let bytes = store.blobs().get(*content)?;
    if bytes.contains(&0) {
        return Ok(Some(Side::Binary));
    }
    Ok(Some(Side::Text(bytes)))
}

/// A page of the diff of one of a proposal's heads (its current one when
/// `head` is `None`) against that head's first parent, with line hunks.
///
/// # Errors
///
/// [`Error::NotFound`] for a missing repo or proposal, or a head the
/// proposal never had. As [`read::diff`] otherwise.
#[allow(clippy::too_many_arguments)] // one per request field
pub fn diff(
    store: &Backend,
    repo: &str,
    number: i64,
    head: Option<ChangesetId>,
    path: Option<&RepoPath>,
    after: Option<&RepoPath>,
    context: u32,
    deadline: &Deadline,
) -> Result<ProposalDiff> {
    let (head, base) = store.read(|c| {
        let repo_id = c.repo_id(repo)?;
        let row = load(c, repo_id, number)?;
        let head = match head {
            None => row.head,
            Some(head) => {
                if !c
                    .proposal_heads(repo_id, number)?
                    .iter()
                    .any(|h| h.changeset == head)
                {
                    return Err(Error::NotFound(format!(
                        "head {} of proposal #{number}",
                        hex(head.as_bytes())
                    )));
                }
                head
            }
        };
        let cs = c.get_changeset(repo_id, head)?;
        let base = *cs
            .parents()
            .first()
            .ok_or_else(|| Error::Corrupt(format!("proposal #{number}'s head has no parent")))?;
        Ok((head, base))
    })?;
    let DiffPage {
        entries,
        mut next_after,
        renames_incomplete,
    } = read::diff(store, repo, base, head, path, after, deadline)?;
    let mut files = Vec::with_capacity(entries.len());
    let mut bytes = 0_usize;
    let count = entries.len();
    for (i, entry) in entries.into_iter().enumerate() {
        deadline.check()?;
        // The half of a rename that deletes shows no lines: its added half
        // carries the change.
        let old_state = match (&entry.old, &entry.rename) {
            (None, Some(rename)) => {
                let blob = read::file_blob(store, repo, base, &rename.path, deadline)?;
                Some(FileState::Regular {
                    content: blob,
                    executable: false,
                })
            }
            _ => entry.old,
        };
        let skip = entry.new.is_none() && entry.rename.is_some();
        let (mut hunks_out, mut binary, mut too_large) = (Vec::new(), false, false);
        if !skip {
            let old = side(store, old_state.as_ref())?;
            let new = side(store, entry.new.as_ref())?;
            match (old, new) {
                (Some(Side::TooLarge), _) | (_, Some(Side::TooLarge)) => too_large = true,
                (Some(Side::Binary), _) | (_, Some(Side::Binary)) => binary = true,
                (None, None) => {}
                (old, new) => {
                    let text = |s: Option<Side>| match s {
                        Some(Side::Text(t)) => t,
                        _ => Vec::new(),
                    };
                    let (old, new) = (text(old), text(new));
                    if old != new {
                        hunks_out = hunks(&old, &new, context);
                    }
                }
            }
        }
        bytes += hunks_out
            .iter()
            .flat_map(|h| h.lines.iter())
            .map(|l| l.text.len() + 16)
            .sum::<usize>();
        let path = entry.path.clone();
        files.push(FileHunks {
            entry,
            hunks: hunks_out,
            binary,
            too_large,
        });
        if bytes > MAX_DIFF_REPLY_BYTES && i + 1 < count {
            next_after = Some(path);
            break;
        }
    }
    Ok(ProposalDiff {
        base,
        head,
        files,
        next_after,
        renames_incomplete,
    })
}

/// A proposal row as a backend reads it: number, ref, title, description,
/// author, head, state, merged, created, updated.
pub(crate) type RawProposal = (
    i64,
    String,
    String,
    String,
    String,
    Vec<u8>,
    String,
    Option<Vec<u8>>,
    i64,
    i64,
);
/// A head row as a backend reads it: seq, changeset, pushed by, at.
pub(crate) type RawHead = (i64, Vec<u8>, String, i64);
/// A review row as a backend reads it: seq, reviewer, verdict, changeset,
/// body, at.
pub(crate) type RawReview = (i64, String, String, Vec<u8>, String, i64);
/// A thread row as a backend reads it: thread, path, line, old side,
/// anchor, resolved, created.
pub(crate) type RawThread = (i64, Vec<u8>, i64, i64, Vec<u8>, i64, i64);
/// A comment row as a backend reads it: comment, thread, author, body, at.
pub(crate) type RawComment = (i64, i64, String, String, i64);

fn stored_id(what: &str, bytes: &[u8]) -> Result<ChangesetId> {
    ChangesetId::try_from(bytes).map_err(|e| Error::Corrupt(format!("stored {what}: {e}")))
}

pub(crate) fn decode_proposal(r: RawProposal) -> Result<ProposalRow> {
    Ok(ProposalRow {
        number: r.0,
        target_ref: r.1,
        title: r.2,
        description: r.3,
        author: r.4,
        head: stored_id("proposal head", &r.5)?,
        state: ProposalStatus::parse(&r.6)?,
        merged: r.7.map(|m| stored_id("merged changeset", &m)).transpose()?,
        created_at: r.8,
        updated_at: r.9,
    })
}

pub(crate) fn decode_head(r: RawHead) -> Result<HeadRow> {
    Ok(HeadRow {
        seq: r.0,
        changeset: stored_id("proposal head", &r.1)?,
        pushed_by: r.2,
        at: r.3,
    })
}

pub(crate) fn decode_review(r: RawReview) -> Result<ReviewRow> {
    Ok(ReviewRow {
        seq: r.0,
        reviewer: r.1,
        verdict: VerdictKind::parse(&r.2)?,
        changeset: stored_id("reviewed head", &r.3)?,
        body: r.4,
        at: r.5,
    })
}

pub(crate) fn decode_thread(r: RawThread) -> Result<ThreadRow> {
    Ok(ThreadRow {
        thread: r.0,
        path: r.1,
        line: u32::try_from(r.2).map_err(|_| Error::Corrupt(format!("thread line {}", r.2)))?,
        old_side: r.3 != 0,
        anchor: stored_id("thread anchor", &r.4)?,
        resolved: r.5 != 0,
        created_at: r.6,
    })
}

#[allow(clippy::unnecessary_wraps)] // the shape every decoder has, which the queries map
pub(crate) fn decode_comment(r: RawComment) -> Result<CommentRow> {
    Ok(CommentRow {
        comment: r.0,
        thread: r.1,
        author: r.2,
        body: r.3,
        at: r.4,
    })
}

/// The hex of an ID, for messages.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> ChangesetId {
        ChangesetId::try_from([n; 32].as_slice()).unwrap()
    }

    fn proposal(head: u8) -> ProposalRow {
        ProposalRow {
            number: 1,
            target_ref: "main".into(),
            title: "t".into(),
            description: String::new(),
            author: "alice".into(),
            head: id(head),
            state: ProposalStatus::Open,
            merged: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn review(seq: i64, who: &str, verdict: VerdictKind, head: u8) -> ReviewRow {
        ReviewRow {
            seq,
            reviewer: who.into(),
            verdict,
            changeset: id(head),
            body: String::new(),
            at: seq,
        }
    }

    fn rules(min: u32) -> RepoRules {
        RepoRules {
            reviewers: ["alice", "bob", "carol"].map(String::from).into(),
            protected: [("main".to_owned(), min)].into(),
        }
    }

    #[test]
    fn an_approval_of_the_head_by_a_reviewer_passes() {
        let g = gate(
            &proposal(1),
            &[review(1, "bob", VerdictKind::Approve, 1)],
            &rules(1),
        );
        assert!(g.approved, "{g:?}");
        assert_eq!(g.approved_by, ["bob"]);
    }

    #[test]
    fn no_review_fails_with_a_reason() {
        let g = gate(&proposal(1), &[], &rules(1));
        assert!(!g.approved);
        assert!(g.reason.contains("needs 1 approval"), "{}", g.reason);
    }

    #[test]
    fn the_authors_own_approval_never_counts() {
        let g = gate(
            &proposal(1),
            &[review(1, "alice", VerdictKind::Approve, 1)],
            &rules(1),
        );
        assert!(!g.approved);
        assert_eq!(g.approved_by, Vec::<String>::new());
    }

    #[test]
    fn an_approval_of_an_older_head_does_not_count() {
        let g = gate(
            &proposal(2),
            &[review(1, "bob", VerdictKind::Approve, 1)],
            &rules(1),
        );
        assert!(!g.approved);
    }

    #[test]
    fn a_request_for_changes_blocks_until_the_same_reviewer_approves() {
        let reviews = [
            review(1, "bob", VerdictKind::Approve, 1),
            review(2, "carol", VerdictKind::RequestChanges, 1),
        ];
        let g = gate(&proposal(1), &reviews, &rules(1));
        assert!(!g.approved);
        assert_eq!(g.changes_requested_by, ["carol"]);
        let mut reviews = reviews.to_vec();
        reviews.push(review(3, "carol", VerdictKind::Comment, 1));
        assert!(
            !gate(&proposal(1), &reviews, &rules(1)).approved,
            "a comment unblocks nothing"
        );
        reviews.push(review(4, "carol", VerdictKind::Approve, 1));
        let g = gate(&proposal(1), &reviews, &rules(1));
        assert!(g.approved, "{g:?}");
        assert_eq!(g.approved_by, ["bob", "carol"]);
    }

    #[test]
    fn a_principal_without_a_review_rule_counts_for_nothing() {
        let g = gate(
            &proposal(1),
            &[review(1, "dave", VerdictKind::Approve, 1)],
            &rules(1),
        );
        assert!(!g.approved);
        let g = gate(
            &proposal(1),
            &[
                review(1, "bob", VerdictKind::Approve, 1),
                review(2, "dave", VerdictKind::RequestChanges, 1),
            ],
            &rules(1),
        );
        assert!(g.approved, "{g:?}");
    }

    #[test]
    fn two_required_approvals_need_two_reviewers() {
        let one = [review(1, "bob", VerdictKind::Approve, 1)];
        assert!(!gate(&proposal(1), &one, &rules(2)).approved);
        let two = [one[0].clone(), review(2, "carol", VerdictKind::Approve, 1)];
        assert!(gate(&proposal(1), &two, &rules(2)).approved);
    }

    #[test]
    fn a_closed_proposal_never_passes() {
        let mut p = proposal(1);
        p.state = ProposalStatus::Closed;
        let g = gate(&p, &[review(1, "bob", VerdictKind::Approve, 1)], &rules(1));
        assert!(!g.approved);
        assert!(g.reason.contains("closed"), "{}", g.reason);
    }

    fn texts(h: &Hunk) -> Vec<String> {
        h.lines
            .iter()
            .map(|l| {
                let sign = match l.kind {
                    LineKind::Context => ' ',
                    LineKind::Added => '+',
                    LineKind::Removed => '-',
                };
                format!("{sign}{}", String::from_utf8_lossy(&l.text))
            })
            .collect()
    }

    #[test]
    fn a_changed_line_gives_one_hunk_with_context() {
        let old = b"a\nb\nc\nd\ne\nf\ng\n";
        let new = b"a\nb\nc\nD\ne\nf\ng\n";
        let h = hunks(old, new, 1);
        assert_eq!(h.len(), 1);
        assert_eq!(
            (
                h[0].old_start,
                h[0].old_lines,
                h[0].new_start,
                h[0].new_lines
            ),
            (3, 3, 3, 3)
        );
        assert_eq!(texts(&h[0]), [" c", "-d", "+D", " e"]);
        assert_eq!(h[0].lines[2].new_line, 4);
        assert_eq!(h[0].lines[1].old_line, 4);
    }

    #[test]
    fn far_apart_changes_give_two_hunks_and_near_ones_merge() {
        let old: Vec<u8> = (0..20)
            .flat_map(|i| format!("{i}\n").into_bytes())
            .collect();
        let mut new = old.clone();
        new.splice(0..2, b"X\n".iter().copied()); // line 1: "0" -> "X"
        let text = String::from_utf8(new).unwrap().replace("\n18\n", "\nY\n");
        let h = hunks(&old, text.as_bytes(), 3);
        assert_eq!(h.len(), 2, "{h:?}");
        let h = hunks(&old, text.as_bytes(), 10);
        assert_eq!(h.len(), 1, "{h:?}");
    }

    #[test]
    fn an_added_file_is_one_hunk_from_zero() {
        let h = hunks(b"", b"x\ny\n", 3);
        assert_eq!(h.len(), 1);
        assert_eq!(
            (
                h[0].old_start,
                h[0].old_lines,
                h[0].new_start,
                h[0].new_lines
            ),
            (0, 0, 1, 2)
        );
        assert_eq!(texts(&h[0]), ["+x", "+y"]);
    }

    #[test]
    fn a_deleted_file_is_one_hunk_to_zero() {
        let h = hunks(b"x\ny\n", b"", 3);
        assert_eq!(
            (
                h[0].old_start,
                h[0].old_lines,
                h[0].new_start,
                h[0].new_lines
            ),
            (1, 2, 0, 0)
        );
    }

    #[test]
    fn line_endings_are_stripped_and_crlf_too() {
        let h = hunks(b"a\r\n", b"b\r\n", 0);
        assert_eq!(texts(&h[0]), ["-a", "+b"]);
    }
}
