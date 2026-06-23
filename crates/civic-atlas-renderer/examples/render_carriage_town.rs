//! D1 acceptance runner: one full reconstruction through
//! `run_full_pipeline` with the real Scene Foundry renderer.
//!
//! Drives the genuine engine pipeline (evidence -> extraction -> block
//! subgraph -> priors -> merge -> generate_assets) over a Whaley-House-like
//! Carriage Town fixture (Sanborn sheet: 3 stories, brick, hipped roof;
//! 14 x 18 m footprint from the parcel geometry) and writes the produced
//! massing GLB + provenance record through the local asset store.
//!
//! Run:
//!   cargo run -p civic-atlas-renderer --example render_carriage_town -- \
//!     [asset_dir] [public_base_url]
//!
//! Defaults: asset_dir=/tmp/scene-foundry-assets, base=/assets/scene-foundry

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use civic_atlas_reconstruction_engine::{
    run_full_pipeline, Artifact, BlockCoherentPriorModel, DecodedArtifact, InMemoryRepository,
    ReconstructionRequest, ZeroEmbeddingProvider,
};
use civic_atlas_renderer::{LocalDirAssetStore, SceneFoundryRenderer};
use civic_atlas_types::civic_atlas::v1::{CivicObject, TenantContext, TimeSlice};

fn whaley_like_fixture() -> InMemoryRepository {
    InMemoryRepository {
        parcel_history: vec![CivicObject {
            id: "00000000-0000-0000-0000-000000000001".to_string(),
            tenant_id: "flint".to_string(),
            name: "building:carriage-town:1".to_string(),
            object_type: "BuildingPresence".to_string(),
            // ~14 m x 18 m rectangle at Flint's latitude.
            geometry_json: r#"{"type":"Polygon","coordinates":[[
                [-83.7082, 43.0118],
                [-83.70802806, 43.0118],
                [-83.70802806, 43.01196170],
                [-83.7082, 43.01196170],
                [-83.7082, 43.0118]
            ]]}"#
                .to_string(),
            time_start_ms: Some(-2_682_288_000_000),
            time_end_ms: None,
            confidence: 1.0,
            source_ids: vec!["habs:mi-318".to_string()],
            dossier_path: String::new(),
            attributes: HashMap::from([(
                "block_id".to_string(),
                "block:carriage-town".to_string(),
            )]),
        }],
        direct_artifacts: vec![Artifact {
            artifact_id: "artifact:sanborn:flint-1899-s18".to_string(),
            artifact_key: "sanborn-flint-1899-s18".to_string(),
            source_type: "sanborn_sheet".to_string(),
            title: "Sanborn Flint 1899 sheet 18".to_string(),
            uri: "https://www.loc.gov/resource/g4114fm.g4114fm_g061121899/?sp=18".to_string(),
            citation: "Sanborn Map Company, Flint 1899".to_string(),
            captured_at_ms: Some(-2_240_524_800_000),
            fetched_at_ms: None,
            content_hash: "sha256-fixture-sanborn".to_string(),
            decoded: DecodedArtifact::SanbornSheet {
                footprint_wkt: None,
                story_count: Some(3),
                material_code: Some("red".to_string()),
                notation: None,
                roof_form: Some("hipped".to_string()),
            },
            metadata: BTreeMap::new(),
        }],
        adjacent_artifacts: Vec::new(),
        graph: None,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let asset_dir = args
        .next()
        .unwrap_or_else(|| "/tmp/scene-foundry-assets".to_string());
    let base_url = args
        .next()
        .unwrap_or_else(|| "/assets/scene-foundry".to_string());

    let request = ReconstructionRequest {
        tenant_context: TenantContext {
            tenant_id: "flint".to_string(),
            atlas_node_id: "atlas:flint".to_string(),
            metadata: Default::default(),
        },
        parcel_id: "parcel:carriage-town:whaley".to_string(),
        time_slice: TimeSlice {
            at_ms: Some(-2_208_988_800_000), // 1900-01-01
            start_ms: None,
            end_ms: None,
        },
        requested_by: "example:render_carriage_town".to_string(),
        auto_approve: false,
    };

    let repository = whaley_like_fixture();
    let embeddings = ZeroEmbeddingProvider::default();
    let prior_model = BlockCoherentPriorModel::default();
    let store = Arc::new(LocalDirAssetStore::new(&asset_dir, &base_url));
    let renderer = SceneFoundryRenderer::new(store);

    let output =
        run_full_pipeline(request, &repository, &embeddings, &prior_model, &renderer).await?;

    let manifest = &output.asset_manifest;
    println!("{}", serde_json::to_string_pretty(manifest)?);

    // The gate, asserted hard so the example doubles as a check.
    anyhow::ensure!(manifest.status != "queued", "manifest must not be queued");
    anyhow::ensure!(manifest.status == "completed", "manifest must be completed");
    for asset in &manifest.assets {
        anyhow::ensure!(
            asset.content_hash.starts_with("sha256-") && asset.content_hash.len() > 15,
            "asset {} lacks a real content hash",
            asset.asset_id
        );
        let key = asset
            .uri
            .trim_start_matches(&format!("{}/", base_url.trim_end_matches('/')));
        let path = std::path::Path::new(&asset_dir).join(key);
        anyhow::ensure!(path.exists(), "asset bytes missing at {}", path.display());
        eprintln!(
            "asset on disk: {} ({} bytes)",
            path.display(),
            std::fs::metadata(&path)?.len()
        );
    }

    let spec = &output.merged.spec;
    eprintln!(
        "spec {} v{}: stories={} width={:?} depth={:?} roof={}",
        spec.spec_id,
        spec.spec_version,
        spec.mass.as_ref().map(|mass| mass.stories).unwrap_or(0),
        spec.mass
            .as_ref()
            .and_then(|mass| mass.width.as_ref())
            .and_then(|range| range.max),
        spec.mass
            .as_ref()
            .and_then(|mass| mass.depth.as_ref())
            .and_then(|range| range.max),
        spec.roof
            .as_ref()
            .map(|roof| roof.roof_type.as_str())
            .unwrap_or("-"),
    );
    eprintln!("render gate: PASS (status=completed, real content hashes, bytes on disk)");
    Ok(())
}
