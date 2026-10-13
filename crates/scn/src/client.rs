//! Connection to the server, plus helpers every command shares.

use anyhow::{Context, Result, bail};
use scion_core::{ChangesetId, RepoPath};
use scion_proto::v1::ResolveRefRequest;
use scion_proto::v1::scion_client::ScionClient;
use tonic::metadata::MetadataValue;
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;

/// Largest single gRPC message the client accepts.
const MAX_MESSAGE_BYTES: usize = 64 << 20;

/// A connected client.
pub type Client = ScionClient<InterceptedService<Channel, Bearer>>;

/// Adds the bearer token to every request.
#[derive(Clone)]
pub struct Bearer(Option<MetadataValue<tonic::metadata::Ascii>>);

impl Interceptor for Bearer {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        if let Some(value) = &self.0 {
            request
                .metadata_mut()
                .insert("authorization", value.clone());
        }
        Ok(request)
    }
}

/// What every command needs.
#[derive(Clone)]
pub struct Ctx {
    /// The gRPC client.
    pub client: Client,
    /// Repository name.
    pub repo: String,
    /// Print JSON instead of text.
    pub json: bool,
}

impl Ctx {
    /// Connects to `server`.
    pub async fn connect(server: &str, token: &str, repo: String, json: bool) -> Result<Self> {
        let channel = Channel::from_shared(server.to_owned())
            .context("bad server address")?
            .connect()
            .await
            .with_context(|| format!("cannot connect to {server}"))?;
        let bearer = if token.is_empty() {
            None
        } else {
            Some(
                format!("Bearer {token}")
                    .parse()
                    .context("token is not valid ASCII")?,
            )
        };
        let client = ScionClient::with_interceptor(channel, Bearer(bearer))
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        Ok(Self { client, repo, json })
    }

    /// Turns a ref name or a 64-digit changeset ID into a changeset ID.
    pub async fn resolve(&mut self, rev: &str) -> Result<ChangesetId> {
        if let Ok(id) = rev.parse() {
            return Ok(id);
        }
        let reply = self
            .client
            .resolve_ref(ResolveRefRequest {
                repo: self.repo.clone(),
                name: rev.to_owned(),
            })
            .await
            .with_context(|| format!("cannot resolve {rev:?}"))?
            .into_inner();
        id_from(&reply.changeset_id)
    }
}

/// Parses 32 raw ID bytes from a reply.
pub fn id_from<T: for<'a> TryFrom<&'a [u8]>>(bytes: &[u8]) -> Result<T> {
    T::try_from(bytes).map_err(|_| anyhow::anyhow!("server sent a bad ID"))
}

/// Splits `REV:PATH` into its parts. The path is optional.
pub fn split_spec(spec: &str) -> Result<(&str, Option<RepoPath>)> {
    match spec.split_once(':') {
        None => Ok((spec, None)),
        Some((rev, "")) => Ok((rev, None)),
        Some((rev, path)) => match RepoPath::new(path) {
            Ok(path) => Ok((rev, Some(path))),
            Err(e) => bail!("bad path {path:?}: {e}"),
        },
    }
}
