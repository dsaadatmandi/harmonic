fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::compile_protos("proto/harmonic.proto")?;
    tonic_prost_build::compile_protos("proto/bootstrap.proto")?;
    Ok(())
}