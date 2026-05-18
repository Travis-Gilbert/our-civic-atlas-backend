fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", protoc);

    tonic_build::configure().compile_protos(
        &[
            "../../proto/civic_atlas/v1/civic_atlas.proto",
            "../../proto/civic_atlas/v1/spacetime_atlas.proto",
            "../../proto/theseus_bridge/v1/bridge.proto",
        ],
        &["../../proto"],
    )?;

    println!("cargo:rerun-if-changed=../../proto/civic_atlas/v1/civic_atlas.proto");
    println!("cargo:rerun-if-changed=../../proto/civic_atlas/v1/spacetime_atlas.proto");
    println!("cargo:rerun-if-changed=../../proto/theseus_bridge/v1/bridge.proto");
    Ok(())
}
