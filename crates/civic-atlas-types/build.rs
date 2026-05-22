fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", protoc);

    tonic_build::configure().compile_protos(
        &[
            "../../proto/civic_atlas/v1/civic_atlas.proto",
            "../../proto/civic_atlas/v1/reconstruction.proto",
            "../../proto/civic_atlas/v1/reconstruction_service.proto",
            "../../proto/civic_atlas/v1/spacetime_atlas.proto",
            "../../proto/civic_atlas/v1/corrections.proto",
            "../../proto/theseus_bridge/v1/bridge.proto",
            "../../proto/theseus_search/v1/search.proto",
        ],
        &["../../proto"],
    )?;

    println!("cargo:rerun-if-changed=../../proto/civic_atlas/v1/civic_atlas.proto");
    println!("cargo:rerun-if-changed=../../proto/civic_atlas/v1/reconstruction.proto");
    println!("cargo:rerun-if-changed=../../proto/civic_atlas/v1/reconstruction_service.proto");
    println!("cargo:rerun-if-changed=../../proto/civic_atlas/v1/spacetime_atlas.proto");
    println!("cargo:rerun-if-changed=../../proto/civic_atlas/v1/corrections.proto");
    println!("cargo:rerun-if-changed=../../proto/theseus_bridge/v1/bridge.proto");
    println!("cargo:rerun-if-changed=../../proto/theseus_search/v1/search.proto");
    Ok(())
}
