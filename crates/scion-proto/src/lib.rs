//! gRPC API for Scion, generated from `proto/scion.proto`.

/// Largest single gRPC message either side sends or accepts.
pub const MAX_MESSAGE_BYTES: usize = 64 << 20;

/// Chunk size for streamed file content (`PutBlob`, `ReadFile`).
pub const CHUNK_BYTES: usize = 1 << 20;

/// Version 1 of the API.
#[allow(missing_docs, clippy::pedantic)]
pub mod v1 {
    tonic::include_proto!("scion.v1");
}
