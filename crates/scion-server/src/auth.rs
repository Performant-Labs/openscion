//! Bearer-token check on every RPC.

use std::sync::Arc;

use tonic::service::Interceptor;
use tonic::{Request, Status};

/// Who made a request. Added to request extensions by [`Auth`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller(pub String);

/// Accepts requests that carry `authorization: Bearer <token>`.
// SHORTCUT: one static token, one principal ("default"). Upgrade: a token to
// principal map, then per-path write rules.
#[derive(Clone)]
pub struct Auth {
    token: Arc<[u8]>,
}

impl Auth {
    /// Accepts this token.
    #[must_use]
    pub fn new(token: &str) -> Self {
        Self {
            token: token.as_bytes().into(),
        }
    }
}

impl Interceptor for Auth {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        let presented = request
            .metadata()
            .get("authorization")
            .and_then(|value| value.as_bytes().strip_prefix(b"Bearer "));
        if !presented.is_some_and(|token| constant_time_eq(token, &self.token)) {
            return Err(Status::unauthenticated("missing or wrong token"));
        }
        request.extensions_mut().insert(Caller("default".into()));
        Ok(request)
    }
}

/// Compares without an early exit, so timing does not reveal a prefix match.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}
