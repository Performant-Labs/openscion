//! Generates the gRPC client and server from `proto/scion.proto`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .compile_protos(&["../../proto/scion.proto"], &["../../proto"])?;
    Ok(())
}
