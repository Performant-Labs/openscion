//! The gRPC front end. Parses requests, runs the work on a blocking thread
//! (`SQLite` and the object folder are synchronous), and maps errors to status
//! codes.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use scion_core::RepoPath;
use scion_proto::v1::scion_server::{Scion, ScionServer};
use scion_proto::v1::{
    CreateChangesetRequest, CreateChangesetResponse, GetChangesetRequest, GetChangesetResponse,
    GetLandRequest, LandResponse, ListDirRequest, ListDirResponse, LogEntry, LogRequest,
    LogResponse, PutBlobRequest, PutBlobResponse, ReadFileRequest, ReadFileResponse,
    ResolveRefRequest, ResolveRefResponse,
};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_stream::{Stream, StreamExt};
use tonic::service::interceptor::InterceptedService;
use tonic::{Request, Response, Status, Streaming};

use crate::auth::{Auth, Caller};
use crate::error::Result;
use crate::land::{self, LandOutcome, LandState, Lands};
use crate::store::{MAX_OBJECT_BYTES, Store};
use crate::{read, write};

/// Largest single gRPC message, in either direction.
pub const MAX_MESSAGE_BYTES: usize = 64 << 20;
/// Chunk size for streamed file content.
const CHUNK_BYTES: usize = 1 << 20;
/// Most changesets one `Log` call returns.
const MAX_LOG: usize = 1000;

/// Serves the API on `listener`, requiring `token`, until `shutdown` resolves.
///
/// # Errors
///
/// Transport errors.
pub async fn serve(
    store: Store,
    listener: TcpListener,
    token: &str,
    shutdown: impl Future<Output = ()>,
) -> Result<(), tonic::transport::Error> {
    let service = InterceptedService::new(ScionService::new(store).into_server(), Auth::new(token));
    tonic::transport::Server::builder()
        .add_service(service)
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
        .await
}

/// Implements the Scion RPCs.
pub struct ScionService {
    store: Store,
    lands: Arc<Lands>,
}

impl ScionService {
    /// Serves `store`.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self {
            store,
            lands: Arc::default(),
        }
    }

    /// Wraps the service in a gRPC server with message size limits.
    #[must_use]
    pub fn into_server(self) -> ScionServer<Self> {
        ScionServer::new(self)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES)
    }

    /// Runs synchronous store work off the async runtime.
    async fn blocking<T, F>(&self, f: F) -> Result<T, Status>
    where
        T: Send + 'static,
        F: FnOnce(Store) -> Result<T> + Send + 'static,
    {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(store))
            .await
            .map_err(|e| Status::internal(format!("worker failed: {e}")))?
            .map_err(Status::from)
    }
}

fn parse_id<T: for<'a> TryFrom<&'a [u8]>>(bytes: &[u8], what: &str) -> Result<T, Status> {
    T::try_from(bytes).map_err(|_| Status::invalid_argument(format!("{what} must be 32 bytes")))
}

fn parse_path(bytes: Vec<u8>) -> Result<RepoPath, Status> {
    RepoPath::new(bytes).map_err(|e| Status::invalid_argument(format!("bad path: {e}")))
}

fn caller<T>(request: &Request<T>) -> Result<Caller, Status> {
    request
        .extensions()
        .get::<Caller>()
        .cloned()
        .ok_or_else(|| Status::unauthenticated("no caller"))
}

fn land_response(outcome: LandOutcome) -> LandResponse {
    use scion_proto::v1::LandState as Wire;
    let state = match outcome.state {
        LandState::Pending => Wire::Pending,
        LandState::Landed => Wire::Landed,
        LandState::Conflict => Wire::Conflict,
        LandState::Rejected => Wire::Rejected,
    };
    LandResponse {
        land_id: outcome.land_id.to_string(),
        state: state.into(),
        result_changeset_id: outcome
            .result
            .map(|id| id.as_bytes().to_vec())
            .unwrap_or_default(),
        detail: outcome.detail,
        conflict_paths: outcome
            .conflict_paths
            .into_iter()
            .map(|p| p.as_bytes().to_vec())
            .collect(),
    }
}

type ReadFileStream = Pin<Box<dyn Stream<Item = Result<ReadFileResponse, Status>> + Send>>;

#[tonic::async_trait]
impl Scion for ScionService {
    async fn put_blob(
        &self,
        request: Request<Streaming<PutBlobRequest>>,
    ) -> Result<Response<PutBlobResponse>, Status> {
        let mut stream = request.into_inner();
        let mut repo = None;
        let mut data = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            repo.get_or_insert(chunk.repo);
            if data.len() + chunk.data.len() > MAX_OBJECT_BYTES {
                return Err(Status::invalid_argument(format!(
                    "blob is larger than {MAX_OBJECT_BYTES} bytes"
                )));
            }
            data.extend_from_slice(&chunk.data);
        }
        let repo = repo.ok_or_else(|| Status::invalid_argument("empty upload"))?;
        let id = self
            .blocking(move |store| write::put_blob(&store, &repo, &data))
            .await?;
        Ok(Response::new(PutBlobResponse {
            blob_id: id.as_bytes().to_vec(),
        }))
    }

    async fn create_changeset(
        &self,
        request: Request<CreateChangesetRequest>,
    ) -> Result<Response<CreateChangesetResponse>, Status> {
        let CreateChangesetRequest { repo, changeset } = request.into_inner();
        let id = self
            .blocking(move |store| write::create_changeset(&store, &repo, &changeset))
            .await?;
        Ok(Response::new(CreateChangesetResponse {
            changeset_id: id.as_bytes().to_vec(),
        }))
    }

    async fn land(
        &self,
        request: Request<scion_proto::v1::LandRequest>,
    ) -> Result<Response<LandResponse>, Status> {
        let caller = caller(&request)?;
        let req = request.into_inner();
        let req = land::LandRequest {
            changeset: parse_id(&req.changeset_id, "changeset_id")?,
            repo: req.repo,
            target_ref: req.target_ref,
            request_key: req.request_key,
        };
        let lands = Arc::clone(&self.lands);
        let outcome = self
            .blocking(move |store| lands.land(&store, &caller, &req))
            .await?;
        Ok(Response::new(land_response(outcome)))
    }

    async fn get_land(
        &self,
        request: Request<GetLandRequest>,
    ) -> Result<Response<LandResponse>, Status> {
        let GetLandRequest { repo, land_id } = request.into_inner();
        let land_id = land_id
            .parse()
            .map_err(|_| Status::invalid_argument("land_id must be a number"))?;
        let outcome = self
            .blocking(move |store| land::get_land(&store, &repo, land_id))
            .await?;
        Ok(Response::new(land_response(outcome)))
    }

    async fn resolve_ref(
        &self,
        request: Request<ResolveRefRequest>,
    ) -> Result<Response<ResolveRefResponse>, Status> {
        let ResolveRefRequest { repo, name } = request.into_inner();
        let id = self
            .blocking(move |store| read::resolve_ref(&store, &repo, &name))
            .await?;
        Ok(Response::new(ResolveRefResponse {
            changeset_id: id.as_bytes().to_vec(),
        }))
    }

    async fn get_changeset(
        &self,
        request: Request<GetChangesetRequest>,
    ) -> Result<Response<GetChangesetResponse>, Status> {
        let req = request.into_inner();
        let id = parse_id(&req.changeset_id, "changeset_id")?;
        let changeset = self
            .blocking(move |store| read::get_changeset(&store, &req.repo, id))
            .await?;
        Ok(Response::new(GetChangesetResponse {
            changeset: changeset.encode(),
        }))
    }

    type ReadFileStream = ReadFileStream;

    async fn read_file(
        &self,
        request: Request<ReadFileRequest>,
    ) -> Result<Response<ReadFileStream>, Status> {
        let req = request.into_inner();
        let id = parse_id(&req.changeset_id, "changeset_id")?;
        let path = parse_path(req.path)?;
        let data = self
            .blocking(move |store| read::read_file(&store, &req.repo, id, &path))
            .await?;
        // An empty file still sends one (empty) chunk.
        let chunks: Vec<_> = if data.is_empty() {
            vec![Ok(ReadFileResponse { data })]
        } else {
            data.chunks(CHUNK_BYTES)
                .map(|chunk| {
                    Ok(ReadFileResponse {
                        data: chunk.to_vec(),
                    })
                })
                .collect()
        };
        Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
    }

    async fn list_dir(
        &self,
        request: Request<ListDirRequest>,
    ) -> Result<Response<ListDirResponse>, Status> {
        let req = request.into_inner();
        let id = parse_id(&req.changeset_id, "changeset_id")?;
        let path = if req.path.is_empty() {
            None
        } else {
            Some(parse_path(req.path)?)
        };
        let tree = self
            .blocking(move |store| read::list_dir(&store, &req.repo, id, path.as_ref()))
            .await?;
        Ok(Response::new(ListDirResponse {
            tree: tree.encode(),
        }))
    }

    async fn log(&self, request: Request<LogRequest>) -> Result<Response<LogResponse>, Status> {
        let req = request.into_inner();
        let id = parse_id(&req.changeset_id, "changeset_id")?;
        let limit = (req.limit as usize).clamp(1, MAX_LOG);
        let entries = self
            .blocking(move |store| read::log(&store, &req.repo, id, limit))
            .await?
            .into_iter()
            .map(|(id, changeset)| LogEntry {
                changeset_id: id.as_bytes().to_vec(),
                changeset: changeset.encode(),
            })
            .collect();
        Ok(Response::new(LogResponse { entries }))
    }
}
