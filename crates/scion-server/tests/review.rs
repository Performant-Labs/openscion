//! Proposals and review before landing, over gRPC (the fork's issue #2):
//! a change to a test repo is proposed, commented on, updated, approved
//! and landed, and an unapproved one is refused. Runs on SQLite, and on
//! Postgres with `SCION_TEST_BACKEND=postgres`.

mod common;

use scion_core::{BlobId, Changeset, ChangesetId, FileState, IdentityTime, RepoPath};
use scion_proto::runtime::auth::token_digest;
use scion_proto::runtime::listen::Limits;
use scion_proto::v1::review_client::ReviewClient;
use scion_proto::v1::scion_client::ScionClient;
use scion_proto::v1::{
    AddCommentRequest, CloseProposalRequest, CreateChangesetRequest, CreateProposalRequest,
    DiffLineKind, GetProposalDiffRequest, GetProposalRequest, LandRequest, LandState,
    ListProposalsRequest, ListThreadsRequest, Proposal, ProposalState, ResolveRefRequest,
    SetThreadResolvedRequest, SubmitReviewRequest, UpdateProposalRequest, Verdict,
};
use std::sync::Arc;

use scion_server::test_support::meta::{Meta as _, MetaTx as _};
use scion_server::{Access, Backend, Metrics, test_support::service};
use tokio::net::TcpListener;
use tonic::metadata::AsciiMetadataValue;
use tonic::service::interceptor::{InterceptedService, Interceptor};
use tonic::transport::Channel;
use tonic::{Code, Request, Status};

const REPO: &str = "main";

#[derive(Clone)]
struct Bearer(AsciiMetadataValue);

impl Interceptor for Bearer {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        request
            .metadata_mut()
            .insert("authorization", self.0.clone());
        Ok(request)
    }
}

type Scion = ScionClient<InterceptedService<Channel, Bearer>>;
type Review = ReviewClient<InterceptedService<Channel, Bearer>>;

fn hex(token: &str) -> String {
    blake3::Hash::from(token_digest(token)).to_hex().to_string()
}

/// alice and bob write; bob and carol review; dave only reads. `main` of
/// repo `main` needs one approval; `dev` is not protected. Every token is
/// its principal's name.
fn access() -> Access {
    use std::fmt::Write as _;
    let mut text = String::new();
    for who in ["alice", "bob", "carol", "dave"] {
        writeln!(text, "token\t{who}\t{}\nread\t{who}\t{REPO}", hex(who)).unwrap();
    }
    writeln!(text, "write\talice\t{REPO}\nwrite\tbob\t{REPO}").unwrap();
    writeln!(text, "review\tbob\t{REPO}\nreview\tcarol\t{REPO}").unwrap();
    writeln!(text, "protect\t{REPO}\tmain\t1").unwrap();
    Access::parse(&text).unwrap()
}

fn author() -> IdentityTime {
    IdentityTime::new(
        b"Ada".to_vec(),
        b"ada@example.com".to_vec(),
        1_700_000_000,
        0,
    )
    .unwrap()
}

fn regular(content: BlobId) -> FileState {
    FileState::Regular {
        content,
        executable: false,
    }
}

/// A store whose repo has a root changeset with `README`, on `main` and
/// on `dev`.
fn store() -> (tempfile::TempDir, Backend, ChangesetId) {
    let dir = scion_server::test_support::private_tempdir();
    let store = Backend::open_test(dir.path()).unwrap();
    let blob = store.blobs().put(b"# hello\n").unwrap();
    let root = store
        .write(|c| {
            let repo = c.create_repo(REPO)?;
            let cs = Changeset::new(
                vec![],
                author(),
                None,
                b"root\n".to_vec(),
                [(RepoPath::new("README").unwrap(), Some(regular(blob)))].into(),
            )
            .unwrap();
            let root = c.insert_changeset(repo, &cs)?;
            c.cas_ref(repo, "main", None, root)?;
            c.cas_ref(repo, "dev", None, root)?;
            c.add_repo_blob(repo, blob)?;
            Ok(root)
        })
        .unwrap();
    (dir, store, root)
}

/// A changeset on `base` that puts `content` at `path`, stored through
/// `CreateChangeset` as `client`'s principal.
async fn change(
    store: &Backend,
    client: &mut Scion,
    base: ChangesetId,
    path: &str,
    content: &[u8],
) -> ChangesetId {
    let blob = store.blobs().put(content).unwrap();
    store
        .write(|c| {
            let repo = c.repo_id(REPO)?;
            c.add_repo_blob(repo, blob)?;
            Ok(())
        })
        .unwrap();
    let cs = Changeset::new(
        vec![base],
        author(),
        None,
        b"change\n".to_vec(),
        [(RepoPath::new(path).unwrap(), Some(regular(blob)))].into(),
    )
    .unwrap();
    let (bytes, id) = cs.encode_with_id();
    client
        .create_changeset(CreateChangesetRequest {
            repo: REPO.into(),
            changeset: bytes,
        })
        .await
        .unwrap();
    id
}

type Stop = tokio::sync::oneshot::Sender<()>;

async fn serve(store: &Backend, dir: &std::path::Path) -> (Stop, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(service::serve(
        store.clone(),
        listener,
        access(),
        service::ServeOptions {
            data: dir.to_path_buf(),
            audit: scion_server::test_support::audit::Audit::off(),
            limits: Limits::default(),
            quotas: scion_server::Quotas::off(),
            pending_lands: None,
            land_stale_secs: 1 << 40,
            tls: None,
            metrics: Arc::new(Metrics::default()),
            reply_memory_budget: scion_server::REPLY_MEMORY_BUDGET,
        },
        Some(Arc::new(scion_server::Keys::generate())),
        async {
            stopped.await.ok();
        },
    ));
    (stop, format!("http://{addr}"))
}

async fn clients(url: &str, who: &str) -> (Scion, Review) {
    let channel = common::channel(url).await.unwrap();
    let bearer = Bearer(format!("Bearer {who}").parse().unwrap());
    (
        ScionClient::with_interceptor(channel.clone(), bearer.clone()),
        ReviewClient::with_interceptor(channel, bearer),
    )
}

async fn head_of(scion: &mut Scion, name: &str) -> Vec<u8> {
    scion
        .resolve_ref(ResolveRefRequest {
            repo: REPO.into(),
            name: name.into(),
        })
        .await
        .unwrap()
        .into_inner()
        .changeset_id
}

async fn land(
    scion: &mut Scion,
    cs: ChangesetId,
    onto: &str,
    key: &str,
) -> Result<scion_proto::v1::LandResponse, Status> {
    scion
        .land(LandRequest {
            repo: REPO.into(),
            changeset_id: cs.as_bytes().to_vec(),
            target_ref: onto.into(),
            request_key: key.into(),
        })
        .await
        .map(tonic::Response::into_inner)
}

async fn review(
    client: &mut Review,
    number: u64,
    verdict: Verdict,
    seen: ChangesetId,
) -> Result<Proposal, Status> {
    client
        .submit_review(SubmitReviewRequest {
            repo: REPO.into(),
            number,
            verdict: verdict.into(),
            changeset_id: seen.as_bytes().to_vec(),
            body: String::new(),
        })
        .await
        .map(tonic::Response::into_inner)
}

async fn get(client: &mut Review, number: u64) -> Proposal {
    client
        .get_proposal(GetProposalRequest {
            repo: REPO.into(),
            number,
        })
        .await
        .unwrap()
        .into_inner()
}

/// The issue's "Done when": proposed, commented on, updated, approved and
/// landed; refused while unapproved, without recording the refusal, so
/// the same request key lands once the proposal passes.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)] // one story, start to end
async fn a_proposal_is_commented_updated_approved_and_landed() {
    let (dir, store, root) = store();
    let (_stop, url) = serve(&store, dir.path()).await;
    let (mut alice, mut alice_review) = clients(&url, "alice").await;
    let (_, mut bob) = clients(&url, "bob").await;
    let (_, mut carol) = clients(&url, "carol").await;
    let (_, mut dave) = clients(&url, "dave").await;

    // Proposed.
    let first = change(&store, &mut alice, root, "src/a", b"one\n").await;
    let proposal = alice_review
        .create_proposal(CreateProposalRequest {
            repo: REPO.into(),
            target_ref: "main".into(),
            changeset_id: first.as_bytes().to_vec(),
            title: "Add src/a".into(),
            description: "First cut.".into(),
        })
        .await
        .unwrap()
        .into_inner();
    let n = proposal.number;
    assert_eq!(n, 1);
    assert_eq!(proposal.author, "alice");
    assert_eq!(proposal.state(), ProposalState::Open);
    let gate = proposal.gate.clone().unwrap();
    assert!(gate.protected && !gate.approved, "{gate:?}");
    assert_eq!(gate.required_approvals, 1);
    assert!(gate.reason.contains("needs 1 approval"), "{}", gate.reason);
    assert_eq!(
        head_of(&mut alice, "main").await,
        root.as_bytes(),
        "a proposal moves no ref"
    );

    // Refused while unapproved: no direct land on a protected ref.
    let err = land(&mut alice, first, "main", "k1").await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    assert!(err.message().contains("protected"), "{}", err.message());
    assert_eq!(head_of(&mut alice, "main").await, root.as_bytes());

    // The diff a reviewer reads: one file, one added line.
    let diff = bob
        .get_proposal_diff(GetProposalDiffRequest {
            repo: REPO.into(),
            number: n,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(diff.base_changeset_id, root.as_bytes());
    assert_eq!(diff.files.len(), 1);
    let file = &diff.files[0];
    assert_eq!(file.entry.as_ref().unwrap().path, b"src/a");
    assert_eq!(file.hunks.len(), 1);
    let line = &file.hunks[0].lines[0];
    assert_eq!(
        (line.kind(), line.text.as_slice(), line.new_line),
        (DiffLineKind::Added, b"one".as_slice(), 1)
    );

    // Commented on: a line thread, and a reply.
    let thread = bob
        .add_comment(AddCommentRequest {
            repo: REPO.into(),
            number: n,
            path: b"src/a".to_vec(),
            line: 1,
            body: "Say two.".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    alice_review
        .add_comment(AddCommentRequest {
            repo: REPO.into(),
            number: n,
            thread_id: thread.id,
            body: "Will do.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let threads = bob
        .list_threads(ListThreadsRequest {
            repo: REPO.into(),
            number: n,
        })
        .await
        .unwrap()
        .into_inner()
        .threads;
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].comments.len(), 2);
    assert_eq!(threads[0].comments[1].author, "alice");
    assert!(!threads[0].outdated && !threads[0].resolved);
    let blocked = review(&mut bob, n, Verdict::RequestChanges, first)
        .await
        .unwrap();
    assert_eq!(blocked.gate.unwrap().changes_requested_by, ["bob"]);

    // Updated: a new head starts the approvals over, and the thread on
    // the old head shows as outdated.
    let second = change(&store, &mut alice, root, "src/a", b"two\n").await;
    let updated = alice_review
        .update_proposal(UpdateProposalRequest {
            repo: REPO.into(),
            number: n,
            changeset_id: second.as_bytes().to_vec(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(updated.head_changeset_id, second.as_bytes());
    assert_eq!(updated.heads.len(), 2);
    let threads = bob
        .list_threads(ListThreadsRequest {
            repo: REPO.into(),
            number: n,
        })
        .await
        .unwrap()
        .into_inner()
        .threads;
    assert!(threads[0].outdated);

    // Still refused: bob's request for changes stands. This key is used
    // again below, after the approval.
    let err = land(&mut alice, second, "main", "k2").await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
    assert!(
        err.message().contains("changes requested by bob"),
        "{}",
        err.message()
    );

    // Reviews that do not count, or are refused.
    let stale = review(&mut carol, n, Verdict::Approve, first)
        .await
        .unwrap_err();
    assert_eq!(
        stale.code(),
        Code::FailedPrecondition,
        "a review names the head it saw"
    );
    let own = review(&mut alice_review, n, Verdict::Approve, second)
        .await
        .unwrap_err();
    assert_eq!(
        own.code(),
        Code::FailedPrecondition,
        "an author does not approve its own"
    );
    let reader = review(&mut dave, n, Verdict::Approve, second)
        .await
        .unwrap_err();
    assert_eq!(
        reader.code(),
        Code::PermissionDenied,
        "dave has no review rule"
    );
    review(&mut dave, n, Verdict::Comment, second)
        .await
        .unwrap();

    // Approved.
    let approved = review(&mut bob, n, Verdict::Approve, second).await.unwrap();
    let gate = approved.gate.unwrap();
    assert!(gate.approved, "{gate:?}");
    assert_eq!(gate.approved_by, ["bob"]);
    bob.set_thread_resolved(SetThreadResolvedRequest {
        repo: REPO.into(),
        number: n,
        thread_id: thread.id,
        resolved: true,
    })
    .await
    .unwrap();

    // The old head is not the proposal's any more.
    let err = land(&mut alice, first, "main", "k1").await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);

    // Landed, with the key the refusal did not record.
    let landed = land(&mut alice, second, "main", "k2").await.unwrap();
    assert_eq!(landed.state(), LandState::Landed, "{landed:?}");
    assert_eq!(
        head_of(&mut alice, "main").await,
        landed.result_changeset_id
    );
    let merged = get(&mut bob, n).await;
    assert_eq!(merged.state(), ProposalState::Merged);
    assert_eq!(merged.merged_changeset_id, landed.result_changeset_id);
    assert!(
        !merged.gate.unwrap().approved,
        "a merged proposal lands nothing more"
    );

    // A retry of the same land returns its stored outcome.
    let again = land(&mut alice, second, "main", "k2").await.unwrap();
    assert_eq!(again, landed);
}

/// A ref without a protect rule lands as before, with or without a
/// proposal; a proposal whose head lands there is marked merged.
#[tokio::test(flavor = "multi_thread")]
async fn an_unprotected_ref_lands_as_before() {
    let (dir, store, root) = store();
    let (_stop, url) = serve(&store, dir.path()).await;
    let (mut alice, mut alice_review) = clients(&url, "alice").await;

    let direct = change(&store, &mut alice, root, "a", b"a\n").await;
    let out = land(&mut alice, direct, "dev", "d1").await.unwrap();
    assert_eq!(out.state(), LandState::Landed);

    let base = ChangesetId::try_from(out.result_changeset_id.as_slice()).unwrap();
    let proposed = change(&store, &mut alice, base, "b", b"b\n").await;
    let p = alice_review
        .create_proposal(CreateProposalRequest {
            repo: REPO.into(),
            target_ref: "dev".into(),
            changeset_id: proposed.as_bytes().to_vec(),
            title: "b".into(),
            description: String::new(),
        })
        .await
        .unwrap()
        .into_inner();
    let gate = p.gate.unwrap();
    assert!(!gate.protected);
    assert!(
        gate.approved,
        "an unprotected ref asks for no approval: {gate:?}"
    );
    let out = land(&mut alice, proposed, "dev", "d2").await.unwrap();
    assert_eq!(out.state(), LandState::Landed);
    let p = get(&mut alice_review, p.number).await;
    assert_eq!(p.state(), ProposalState::Merged);
    assert_eq!(p.merged_changeset_id, out.result_changeset_id);
}

/// Who may change a proposal, and what a closed one may do.
#[tokio::test(flavor = "multi_thread")]
async fn only_its_author_or_a_reviewer_closes_a_proposal() {
    let (dir, store, root) = store();
    let (_stop, url) = serve(&store, dir.path()).await;
    let (mut alice, mut alice_review) = clients(&url, "alice").await;
    let (_, mut bob) = clients(&url, "bob").await;
    let (_, mut dave) = clients(&url, "dave").await;

    let cs = change(&store, &mut alice, root, "x", b"x\n").await;
    let create = CreateProposalRequest {
        repo: REPO.into(),
        target_ref: "main".into(),
        changeset_id: cs.as_bytes().to_vec(),
        title: "x".into(),
        description: String::new(),
    };
    let p = alice_review
        .create_proposal(create.clone())
        .await
        .unwrap()
        .into_inner();
    let twice = alice_review.create_proposal(create).await.unwrap_err();
    assert_eq!(
        twice.code(),
        Code::FailedPrecondition,
        "one open proposal per head and ref"
    );

    let close = CloseProposalRequest {
        repo: REPO.into(),
        number: p.number,
    };
    let err = dave.close_proposal(close.clone()).await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
    review(&mut bob, p.number, Verdict::Approve, cs)
        .await
        .unwrap();
    let closed = alice_review
        .close_proposal(close)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(closed.state(), ProposalState::Closed);

    // A closed proposal gates nothing open: the land is refused.
    let err = land(&mut alice, cs, "main", "x").await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
    let err = review(&mut bob, p.number, Verdict::Approve, cs)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
}

/// Bad requests are refused before anything is stored, and a repo the
/// caller may not read answers as a missing one.
#[tokio::test(flavor = "multi_thread")]
async fn bad_requests_are_refused() {
    let (dir, store, root) = store();
    let (_stop, url) = serve(&store, dir.path()).await;
    let (mut alice, mut review_client) = clients(&url, "alice").await;
    let cs = change(&store, &mut alice, root, "y", b"y\n").await;
    let ok = CreateProposalRequest {
        repo: REPO.into(),
        target_ref: "main".into(),
        changeset_id: cs.as_bytes().to_vec(),
        title: "y".into(),
        description: String::new(),
    };
    let cases = [
        (
            CreateProposalRequest {
                title: String::new(),
                ..ok.clone()
            },
            Code::InvalidArgument,
        ),
        (
            CreateProposalRequest {
                title: "t".repeat(257),
                ..ok.clone()
            },
            Code::InvalidArgument,
        ),
        (
            CreateProposalRequest {
                target_ref: "nope".into(),
                ..ok.clone()
            },
            Code::NotFound,
        ),
        (
            CreateProposalRequest {
                changeset_id: vec![7; 32],
                ..ok.clone()
            },
            Code::NotFound,
        ),
        (
            CreateProposalRequest {
                changeset_id: vec![7; 3],
                ..ok.clone()
            },
            Code::InvalidArgument,
        ),
        (
            CreateProposalRequest {
                repo: "other".into(),
                ..ok.clone()
            },
            Code::NotFound,
        ),
        // The root has no parent: a head has exactly one.
        (
            CreateProposalRequest {
                changeset_id: root.as_bytes().to_vec(),
                ..ok.clone()
            },
            Code::InvalidArgument,
        ),
    ];
    for (request, code) in cases {
        let err = review_client.create_proposal(request).await.unwrap_err();
        assert_eq!(err.code(), code, "{err:?}");
    }
    let err = review_client
        .get_proposal(GetProposalRequest {
            repo: REPO.into(),
            number: 0,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    let err = review_client
        .get_proposal(GetProposalRequest {
            repo: REPO.into(),
            number: 9,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    let listed = review_client
        .list_proposals(ListProposalsRequest {
            repo: REPO.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert!(
        listed.proposals.is_empty(),
        "no refused request stored a proposal"
    );
}

/// Proposals list by number, a page at a time, filtered by state and ref.
#[tokio::test(flavor = "multi_thread")]
async fn proposals_list_in_pages() {
    let (dir, store, root) = store();
    let (_stop, url) = serve(&store, dir.path()).await;
    let (mut alice, mut client) = clients(&url, "alice").await;
    for i in 0..5 {
        let cs = change(&store, &mut alice, root, &format!("f{i}"), b"z\n").await;
        client
            .create_proposal(CreateProposalRequest {
                repo: REPO.into(),
                target_ref: if i % 2 == 0 { "main" } else { "dev" }.into(),
                changeset_id: cs.as_bytes().to_vec(),
                title: format!("p{i}"),
                description: String::new(),
            })
            .await
            .unwrap();
    }
    client
        .close_proposal(CloseProposalRequest {
            repo: REPO.into(),
            number: 1,
        })
        .await
        .unwrap();
    let page = |after, limit, state: ProposalState, target: &str| ListProposalsRequest {
        repo: REPO.into(),
        state: state.into(),
        target_ref: target.into(),
        after_number: after,
        limit,
    };
    let first = client
        .list_proposals(page(0, 2, ProposalState::Unspecified, ""))
        .await
        .unwrap()
        .into_inner();
    let numbers: Vec<u64> = first.proposals.iter().map(|p| p.number).collect();
    assert_eq!(numbers, [1, 2]);
    assert_eq!(first.next_after_number, 2);
    let rest = client
        .list_proposals(page(2, 10, ProposalState::Unspecified, ""))
        .await
        .unwrap()
        .into_inner();
    let numbers: Vec<u64> = rest.proposals.iter().map(|p| p.number).collect();
    assert_eq!(numbers, [3, 4, 5]);
    assert_eq!(rest.next_after_number, 0);
    let open_main = client
        .list_proposals(page(0, 0, ProposalState::Open, "main"))
        .await
        .unwrap()
        .into_inner();
    let numbers: Vec<u64> = open_main.proposals.iter().map(|p| p.number).collect();
    assert_eq!(numbers, [3, 5], "proposal 1 is closed; 2 and 4 are on dev");
}
