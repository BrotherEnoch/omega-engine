// ops/control-plane/build.rs
//
// Compile `proto/omega_control.proto` into Rust code using tonic-build.
// The generated code is placed in OUT_DIR and included in src/grpc.rs
// via the tonic::include_proto! macro.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(false) // control-plane is server-only
        .compile(&["proto/omega_control.proto"], &["proto"])?;
    Ok(())
}