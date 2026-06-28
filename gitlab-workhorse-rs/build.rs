fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(false)
        .compile_protos(
            &[
                "proto/gitaly/smarthttp.proto",
                "proto/gitaly/repository.proto",
                "proto/gitaly/blob.proto",
                "proto/gitaly/diff.proto",
            ],
            &["proto"],
        )?;
    Ok(())
}
