//! The `scion.v1.Review` RPCs: the wire front of [`crate::review`].
//!
//! Every call authenticates like the `Scion` service (the interceptor
//! puts the caller in the request), needs a caller that reads the whole
//! repo, and queues behind the same store worker. Each call that changes
//! review state records the audit's `review` intent before it runs and its
//! result line after, so an approval leaves evidence as a land does.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use scion_core::ChangesetId;
use scion_proto::MAX_MESSAGE_BYTES;
use scion_proto::v1::review_server::{Review, ReviewServer};
use scion_proto::v1::{
    AddCommentRequest, CloseProposalRequest, CreateProposalRequest, DiffHunk, DiffLine,
    DiffLineKind, FileDiff, GetProposalDiffRequest, GetProposalDiffResponse, GetProposalRequest,
    ListProposalsRequest, ListProposalsResponse, ListThreadsRequest, ListThreadsResponse, Proposal,
    ProposalGate, ProposalHead, ProposalReview, ProposalState, ReviewComment, ReviewThread,
    SetThreadResolvedRequest, SubmitReviewRequest, UpdateProposalRequest, Verdict,
};
use tokio::sync::Semaphore;
use tonic::{Request, Response, Status};

use crate::access::Access;
use crate::auth::Caller;
use crate::backend::Backend;
use crate::deadline::Deadline;
use crate::error::Result;
use crate::metrics::code_label;
use crate::quotas::Quotas;
use crate::review::{
    self, Anchor, Gate, HeadRow, LineKind, NewProposal, ProposalRow, ProposalStatus, ReviewRow,
    ThreadView, VerdictKind,
};
use crate::service::{
    REQUEST_TIMEOUT, Worker, caller, check_name, check_whole_reader, count_committed_reply,
    count_reply, diff_entry, json_fields, parse_id, request_id_of,
};

/// Proposal diffs running at once: each reads blobs and diffs lines.
const MAX_PROPOSAL_DIFFS: usize = 4;

/// Implements the Review RPCs.
pub(crate) struct ReviewService {
    worker: Worker,
    access: Arc<Access>,
    quotas: Quotas,
    audit: crate::audit::Audit,
    diffs: Arc<Semaphore>,
}

impl ReviewService {
    pub(crate) fn new(
        worker: Worker,
        access: Arc<Access>,
        quotas: Quotas,
        audit: crate::audit::Audit,
    ) -> Self {
        Self {
            worker,
            access,
            quotas,
            audit,
            diffs: Arc::new(Semaphore::new(MAX_PROPOSAL_DIFFS)),
        }
    }

    /// The gRPC server, with the message limits of the protocol.
    pub(crate) fn into_server(self) -> ReviewServer<Self> {
        ReviewServer::new(self)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES)
    }

    /// Runs a change of review state: the audit's intent first, durable,
    /// then the job, which writes the result line itself, so the evidence
    /// does not depend on this call living on. A call that goes before
    /// its job starts is answered `cancelled` by its guard's drop: every
    /// intent gets one result line, as for a land. `number` is the
    /// proposal the call names (0 for one it opens); `answered` reads the
    /// number the job's answer names, for the result line.
    #[allow(clippy::too_many_arguments)] // the line's fields, one each
    async fn change<T, F>(
        &self,
        request_id: &str,
        caller: &Caller,
        rpc: &'static str,
        repo: &str,
        number: i64,
        detail: String,
        job: F,
        answered: fn(&T) -> i64,
    ) -> Result<T, Status>
    where
        T: Send + 'static,
        F: FnOnce(Backend) -> Result<T> + Send + 'static,
    {
        let line = Line {
            request_id: request_id.to_owned(),
            principal: caller.0.clone(),
            rpc,
            repo: repo.to_owned(),
            detail,
        };
        let state = Arc::new(AtomicU8::new(INTENT));
        // Armed before the intent goes in, so a call cut while the append
        // is in flight is still answered; voided if the intent fails.
        let _guard = Answer {
            state: Arc::clone(&state),
            audit: self.audit.clone(),
            line: Some(line.failed(number, "cancelled")),
        };
        self.audit
            .record(line.intent(number))
            .await
            .map_err(|why| {
                state.store(VOID, Ordering::Release);
                tracing::error!(reason = %why, "an audit intent was not made durable");
                Status::unavailable("the audit line was not made durable")
            })?;
        let audit = self.audit.clone();
        self.worker
            .run(move |store| {
                if state.swap(STARTED, Ordering::AcqRel) == ABANDONED {
                    return Err(crate::error::Error::Cancelled(
                        "the call went before the change started".to_owned(),
                    ));
                }
                let out = job(store).map_err(Status::from);
                let result = match &out {
                    Ok(value) => line.ok(answered(value)),
                    Err(status) => line.failed(number, code_label(status.code())),
                };
                if let Err(why) = audit.record_blocking(&result) {
                    tracing::error!(reason = %why, "an audit result was not made durable");
                }
                Ok(out)
            })
            .await?
    }
}

/// The states of a change's answer, as for a land: the intent is in, the
/// job claimed the answer, the call's drop claimed it, or no intent went
/// in and none is owed.
const INTENT: u8 = 0;
const STARTED: u8 = 1;
const ABANDONED: u8 = 2;
const VOID: u8 = 3;

/// The fields every `review` audit line of one call carries.
struct Line {
    request_id: String,
    principal: String,
    rpc: &'static str,
    repo: String,
    detail: String,
}

impl Line {
    fn fields(&self, number: i64, why: Option<&str>) -> serde_json::Value {
        let mut fields = serde_json::json!({
            "rpc": self.rpc,
            "repo": self.repo,
            "number": number,
            "detail": self.detail,
        });
        if let Some(why) = why {
            fields["why"] = serde_json::json!(why);
        }
        json_fields(&self.request_id, &self.principal, &fields)
    }

    fn intent(&self, number: i64) -> crate::audit::Event {
        crate::audit::Event::new("review", "intent", self.fields(number, None))
    }

    fn ok(&self, number: i64) -> crate::audit::Event {
        crate::audit::Event::new("review", "ok", self.fields(number, None))
    }

    fn failed(&self, number: i64, why: &str) -> crate::audit::Event {
        crate::audit::Event::new("review", "failed", self.fields(number, Some(why)))
    }
}

/// Answers the intent with `cancelled` when the call goes before its job
/// claims the answer. The append runs off this thread: a `Drop` must not
/// fsync on a runtime worker.
struct Answer {
    state: Arc<AtomicU8>,
    audit: crate::audit::Audit,
    line: Option<crate::audit::Event>,
}

impl Drop for Answer {
    fn drop(&mut self) {
        if self.state.swap(ABANDONED, Ordering::AcqRel) != INTENT {
            return;
        }
        let Some(line) = self.line.take() else {
            return;
        };
        let audit = self.audit.clone();
        let write = move || {
            if let Err(why) = audit.record_blocking(&line) {
                tracing::error!(reason = %why, "a cancelled review's audit line was not made durable");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(write);
            }
            Err(_) => {
                std::thread::spawn(write);
            }
        }
    }
}

/// A proposal number on the wire: 1 or more.
fn parse_number(number: u64) -> Result<i64, Status> {
    i64::try_from(number)
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| Status::invalid_argument("number must be a proposal number, from 1"))
}

fn optional_id(bytes: &[u8], what: &str) -> Result<Option<ChangesetId>, Status> {
    if bytes.is_empty() {
        Ok(None)
    } else {
        parse_id(bytes, what).map(Some)
    }
}

fn number_u64(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

fn state_msg(state: ProposalStatus) -> ProposalState {
    match state {
        ProposalStatus::Open => ProposalState::Open,
        ProposalStatus::Merged => ProposalState::Merged,
        ProposalStatus::Closed => ProposalState::Closed,
    }
}

fn verdict_msg(verdict: VerdictKind) -> Verdict {
    match verdict {
        VerdictKind::Approve => Verdict::Approve,
        VerdictKind::RequestChanges => Verdict::RequestChanges,
        VerdictKind::Comment => Verdict::Comment,
    }
}

fn gate_msg(gate: &Gate) -> ProposalGate {
    ProposalGate {
        protected: gate.protected,
        required_approvals: gate.required,
        approved_by: gate.approved_by.clone(),
        changes_requested_by: gate.changes_requested_by.clone(),
        approved: gate.approved,
        reason: gate.reason.clone(),
    }
}

fn proposal_msg(
    repo: &str,
    row: &ProposalRow,
    heads: &[HeadRow],
    reviews: &[ReviewRow],
    gate: &Gate,
) -> Proposal {
    Proposal {
        repo: repo.to_owned(),
        number: number_u64(row.number),
        target_ref: row.target_ref.clone(),
        title: row.title.clone(),
        description: row.description.clone(),
        author: row.author.clone(),
        head_changeset_id: row.head.as_bytes().to_vec(),
        state: state_msg(row.state).into(),
        created_unix_secs: row.created_at,
        updated_unix_secs: row.updated_at,
        merged_changeset_id: row
            .merged
            .map(|m| m.as_bytes().to_vec())
            .unwrap_or_default(),
        heads: heads
            .iter()
            .map(|h| ProposalHead {
                changeset_id: h.changeset.as_bytes().to_vec(),
                pushed_by: h.pushed_by.clone(),
                unix_secs: h.at,
            })
            .collect(),
        reviews: reviews
            .iter()
            .map(|r| ProposalReview {
                reviewer: r.reviewer.clone(),
                verdict: verdict_msg(r.verdict).into(),
                changeset_id: r.changeset.as_bytes().to_vec(),
                body: r.body.clone(),
                unix_secs: r.at,
            })
            .collect(),
        gate: Some(gate_msg(gate)),
    }
}

fn view_msg(repo: &str, view: &review::ProposalView) -> Proposal {
    proposal_msg(repo, &view.row, &view.heads, &view.reviews, &view.gate)
}

fn thread_msg(thread: ThreadView) -> ReviewThread {
    ReviewThread {
        id: number_u64(thread.row.thread),
        path: thread.row.path,
        line: thread.row.line,
        old_side: thread.row.old_side,
        changeset_id: thread.row.anchor.as_bytes().to_vec(),
        resolved: thread.row.resolved,
        outdated: thread.outdated,
        comments: thread
            .comments
            .into_iter()
            .map(|c| ReviewComment {
                id: number_u64(c.comment),
                author: c.author,
                body: c.body,
                unix_secs: c.at,
            })
            .collect(),
    }
}

fn proposal_number(p: &Proposal) -> i64 {
    i64::try_from(p.number).unwrap_or(0)
}

#[tonic::async_trait]
impl Review for ReviewService {
    async fn create_proposal(
        &self,
        request: Request<CreateProposalRequest>,
    ) -> Result<Response<Proposal>, Status> {
        let caller = caller(&request)?;
        let request_id = request_id_of(&request);
        let _call = self.quotas.enter(&caller).map_err(Status::from)?;
        let req = request.into_inner();
        check_whole_reader(&self.access, &caller, &req.repo)?;
        check_name("target_ref", &req.target_ref)?;
        let head: ChangesetId = parse_id(&req.changeset_id, "changeset_id")?;
        let rules = self.access.review_rules(&req.repo);
        let (repo, who) = (req.repo.clone(), caller.clone());
        let detail = review::hex(head.as_bytes());
        let proposal = self
            .change(
                &request_id,
                &caller,
                "CreateProposal",
                &req.repo,
                0,
                detail,
                move |store| {
                    let new = NewProposal {
                        target_ref: req.target_ref,
                        head,
                        title: req.title,
                        description: req.description,
                    };
                    let number = review::create(&store, &who, &repo, new)?;
                    let view = review::view(&store, &repo, &rules, number)?;
                    Ok(view_msg(&repo, &view))
                },
                proposal_number,
            )
            .await?;
        Ok(count_committed_reply(Response::new(proposal)))
    }

    async fn update_proposal(
        &self,
        request: Request<UpdateProposalRequest>,
    ) -> Result<Response<Proposal>, Status> {
        let caller = caller(&request)?;
        let request_id = request_id_of(&request);
        let _call = self.quotas.enter(&caller).map_err(Status::from)?;
        let req = request.into_inner();
        check_whole_reader(&self.access, &caller, &req.repo)?;
        let number = parse_number(req.number)?;
        let head = optional_id(&req.changeset_id, "changeset_id")?;
        let rules = self.access.review_rules(&req.repo);
        let (repo, who) = (req.repo.clone(), caller.clone());
        let detail = head.map(|h| review::hex(h.as_bytes())).unwrap_or_default();
        let proposal = self
            .change(
                &request_id,
                &caller,
                "UpdateProposal",
                &req.repo,
                number,
                detail,
                move |store| {
                    review::update(
                        &store,
                        &who,
                        &repo,
                        &rules,
                        number,
                        head,
                        req.title,
                        req.description,
                    )?;
                    Ok(view_msg(
                        &repo,
                        &review::view(&store, &repo, &rules, number)?,
                    ))
                },
                proposal_number,
            )
            .await?;
        Ok(count_committed_reply(Response::new(proposal)))
    }

    async fn get_proposal(
        &self,
        request: Request<GetProposalRequest>,
    ) -> Result<Response<Proposal>, Status> {
        let caller = caller(&request)?;
        let _call = self.quotas.enter(&caller).map_err(Status::from)?;
        let req = request.into_inner();
        check_whole_reader(&self.access, &caller, &req.repo)?;
        let number = parse_number(req.number)?;
        let rules = self.access.review_rules(&req.repo);
        let proposal = self
            .worker
            .run(move |store| {
                Ok(view_msg(
                    &req.repo,
                    &review::view(&store, &req.repo, &rules, number)?,
                ))
            })
            .await?;
        Ok(count_reply(Response::new(proposal)))
    }

    async fn list_proposals(
        &self,
        request: Request<ListProposalsRequest>,
    ) -> Result<Response<ListProposalsResponse>, Status> {
        let caller = caller(&request)?;
        let _call = self.quotas.enter(&caller).map_err(Status::from)?;
        let req = request.into_inner();
        check_whole_reader(&self.access, &caller, &req.repo)?;
        let state = match ProposalState::try_from(req.state) {
            Ok(ProposalState::Unspecified) => None,
            Ok(ProposalState::Open) => Some(ProposalStatus::Open),
            Ok(ProposalState::Merged) => Some(ProposalStatus::Merged),
            Ok(ProposalState::Closed) => Some(ProposalStatus::Closed),
            Err(_) => return Err(Status::invalid_argument("unknown proposal state")),
        };
        if !req.target_ref.is_empty() {
            check_name("target_ref", &req.target_ref)?;
        }
        let after = i64::try_from(req.after_number)
            .map_err(|_| Status::invalid_argument("after_number is too large"))?;
        let limit = usize::try_from(req.limit).unwrap_or(usize::MAX);
        let rules = self.access.review_rules(&req.repo);
        let reply = self
            .worker
            .run(move |store| {
                let target = (!req.target_ref.is_empty()).then_some(req.target_ref.as_str());
                let (page, more) =
                    review::list(&store, &req.repo, &rules, state, target, after, limit)?;
                let next = if more {
                    page.last().map_or(0, |(row, _)| number_u64(row.number))
                } else {
                    0
                };
                Ok(ListProposalsResponse {
                    proposals: page
                        .iter()
                        .map(|(row, gate)| proposal_msg(&req.repo, row, &[], &[], gate))
                        .collect(),
                    next_after_number: next,
                })
            })
            .await?;
        Ok(count_reply(Response::new(reply)))
    }

    async fn close_proposal(
        &self,
        request: Request<CloseProposalRequest>,
    ) -> Result<Response<Proposal>, Status> {
        let caller = caller(&request)?;
        let request_id = request_id_of(&request);
        let _call = self.quotas.enter(&caller).map_err(Status::from)?;
        let req = request.into_inner();
        check_whole_reader(&self.access, &caller, &req.repo)?;
        let number = parse_number(req.number)?;
        let rules = self.access.review_rules(&req.repo);
        let (repo, who) = (req.repo.clone(), caller.clone());
        let proposal = self
            .change(
                &request_id,
                &caller,
                "CloseProposal",
                &req.repo,
                number,
                String::new(),
                move |store| {
                    review::close(&store, &who, &repo, &rules, number)?;
                    Ok(view_msg(
                        &repo,
                        &review::view(&store, &repo, &rules, number)?,
                    ))
                },
                proposal_number,
            )
            .await?;
        Ok(count_committed_reply(Response::new(proposal)))
    }

    async fn get_proposal_diff(
        &self,
        request: Request<GetProposalDiffRequest>,
    ) -> Result<Response<GetProposalDiffResponse>, Status> {
        let caller = caller(&request)?;
        let _call = self.quotas.enter(&caller).map_err(Status::from)?;
        let deadline = Deadline::of_call(&request, REQUEST_TIMEOUT);
        let _deadline = deadline.guard();
        let req = request.into_inner();
        check_whole_reader(&self.access, &caller, &req.repo)?;
        let number = parse_number(req.number)?;
        let head = optional_id(&req.changeset_id, "changeset_id")?;
        let path = optional_path(req.path)?;
        let after = optional_path(req.after)?;
        let context = req.context_lines.unwrap_or(review::DEFAULT_CONTEXT);
        if context > review::MAX_CONTEXT {
            return Err(Status::invalid_argument(format!(
                "context_lines is at most {}",
                review::MAX_CONTEXT
            )));
        }
        let slot = Arc::clone(&self.diffs)
            .acquire_owned()
            .await
            .map_err(|_| Status::unavailable("server is shutting down"))?;
        let diff = self
            .worker
            .run(move |store| {
                let _slot = slot;
                review::diff(
                    &store,
                    &req.repo,
                    number,
                    head,
                    path.as_ref(),
                    after.as_ref(),
                    context,
                    &deadline,
                )
            })
            .await?;
        Ok(count_reply(Response::new(GetProposalDiffResponse {
            base_changeset_id: diff.base.as_bytes().to_vec(),
            head_changeset_id: diff.head.as_bytes().to_vec(),
            files: diff
                .files
                .into_iter()
                .map(|file| FileDiff {
                    entry: Some(diff_entry(file.entry)),
                    hunks: file
                        .hunks
                        .into_iter()
                        .map(|h| DiffHunk {
                            old_start: h.old_start,
                            old_lines: h.old_lines,
                            new_start: h.new_start,
                            new_lines: h.new_lines,
                            lines: h
                                .lines
                                .into_iter()
                                .map(|l| DiffLine {
                                    kind: match l.kind {
                                        LineKind::Context => DiffLineKind::Context,
                                        LineKind::Added => DiffLineKind::Added,
                                        LineKind::Removed => DiffLineKind::Removed,
                                    }
                                    .into(),
                                    text: l.text,
                                    old_line: l.old_line,
                                    new_line: l.new_line,
                                })
                                .collect(),
                        })
                        .collect(),
                    binary: file.binary,
                    too_large: file.too_large,
                })
                .collect(),
            next_after: diff
                .next_after
                .map(|p| p.as_bytes().to_vec())
                .unwrap_or_default(),
            renames_incomplete: diff.renames_incomplete,
        })))
    }

    async fn add_comment(
        &self,
        request: Request<AddCommentRequest>,
    ) -> Result<Response<ReviewThread>, Status> {
        let caller = caller(&request)?;
        let request_id = request_id_of(&request);
        let _call = self.quotas.enter(&caller).map_err(Status::from)?;
        let req = request.into_inner();
        check_whole_reader(&self.access, &caller, &req.repo)?;
        let number = parse_number(req.number)?;
        let thread = i64::try_from(req.thread_id)
            .map_err(|_| Status::invalid_argument("thread_id is too large"))?;
        let anchor = if thread == 0 {
            Anchor {
                path: optional_path(req.path)?,
                line: req.line,
                old_side: req.old_side,
            }
        } else if !req.path.is_empty() || req.line != 0 || req.old_side {
            return Err(Status::invalid_argument(
                "a reply takes its thread's place; leave path, line and old_side unset",
            ));
        } else {
            Anchor::default()
        };
        let (repo, who) = (req.repo.clone(), caller.clone());
        let thread = self
            .change(
                &request_id,
                &caller,
                "AddComment",
                &req.repo,
                number,
                String::new(),
                move |store| {
                    let id =
                        review::comment(&store, &who, &repo, number, thread, &anchor, req.body)?;
                    Ok(thread_msg(review::thread(&store, &repo, number, id)?))
                },
                |_| 0,
            )
            .await?;
        Ok(count_committed_reply(Response::new(thread)))
    }

    async fn set_thread_resolved(
        &self,
        request: Request<SetThreadResolvedRequest>,
    ) -> Result<Response<ReviewThread>, Status> {
        let caller = caller(&request)?;
        let request_id = request_id_of(&request);
        let _call = self.quotas.enter(&caller).map_err(Status::from)?;
        let req = request.into_inner();
        check_whole_reader(&self.access, &caller, &req.repo)?;
        let number = parse_number(req.number)?;
        let thread = i64::try_from(req.thread_id)
            .ok()
            .filter(|t| *t > 0)
            .ok_or_else(|| Status::invalid_argument("thread_id must be a thread, from 1"))?;
        let repo = req.repo.clone();
        let detail = if req.resolved { "resolved" } else { "active" }.to_owned();
        let thread = self
            .change(
                &request_id,
                &caller,
                "SetThreadResolved",
                &req.repo,
                number,
                detail,
                move |store| {
                    review::set_resolved(&store, &repo, number, thread, req.resolved)?;
                    Ok(thread_msg(review::thread(&store, &repo, number, thread)?))
                },
                |_| 0,
            )
            .await?;
        Ok(count_committed_reply(Response::new(thread)))
    }

    async fn list_threads(
        &self,
        request: Request<ListThreadsRequest>,
    ) -> Result<Response<ListThreadsResponse>, Status> {
        let caller = caller(&request)?;
        let _call = self.quotas.enter(&caller).map_err(Status::from)?;
        let req = request.into_inner();
        check_whole_reader(&self.access, &caller, &req.repo)?;
        let number = parse_number(req.number)?;
        let threads = self
            .worker
            .run(move |store| review::threads(&store, &req.repo, number))
            .await?;
        Ok(count_reply(Response::new(ListThreadsResponse {
            threads: threads.into_iter().map(thread_msg).collect(),
        })))
    }

    async fn submit_review(
        &self,
        request: Request<SubmitReviewRequest>,
    ) -> Result<Response<Proposal>, Status> {
        let caller = caller(&request)?;
        let request_id = request_id_of(&request);
        let _call = self.quotas.enter(&caller).map_err(Status::from)?;
        let req = request.into_inner();
        check_whole_reader(&self.access, &caller, &req.repo)?;
        let number = parse_number(req.number)?;
        let verdict = match Verdict::try_from(req.verdict) {
            Ok(Verdict::Approve) => VerdictKind::Approve,
            Ok(Verdict::RequestChanges) => VerdictKind::RequestChanges,
            Ok(Verdict::Comment) => VerdictKind::Comment,
            Ok(Verdict::Unspecified) | Err(_) => {
                return Err(Status::invalid_argument(
                    "verdict must be APPROVE, REQUEST_CHANGES or COMMENT",
                ));
            }
        };
        let seen: ChangesetId = parse_id(&req.changeset_id, "changeset_id")?;
        let rules = self.access.review_rules(&req.repo);
        let (repo, who) = (req.repo.clone(), caller.clone());
        let proposal = self
            .change(
                &request_id,
                &caller,
                "SubmitReview",
                &req.repo,
                number,
                verdict.as_str().to_owned(),
                move |store| {
                    review::submit_review(
                        &store, &who, &repo, &rules, number, verdict, seen, req.body,
                    )?;
                    Ok(view_msg(
                        &repo,
                        &review::view(&store, &repo, &rules, number)?,
                    ))
                },
                proposal_number,
            )
            .await?;
        Ok(count_committed_reply(Response::new(proposal)))
    }
}

fn optional_path(bytes: Vec<u8>) -> Result<Option<scion_core::RepoPath>, Status> {
    if bytes.is_empty() {
        Ok(None)
    } else {
        scion_core::RepoPath::new(bytes)
            .map(Some)
            .map_err(|e| Status::invalid_argument(format!("bad path: {e}")))
    }
}
