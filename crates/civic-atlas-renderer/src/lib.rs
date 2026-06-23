//! Scene Foundry reconstruction renderer.
//!
//! Replaces the `SceneFoundryManifestGenerator` stub with a real
//! `AssetGenerator`: every `generate` call writes the spec's massing
//! geometry to storage as a GLB with a real content hash and returns a
//! manifest with a completed status. The evidence-graded tier selector
//! grades the photographic evidence on the spec and dispatches the
//! model-inference refinement stages (facade parse, rectify/inpaint/
//! project, PyTorch3D fitting, splatting, MASt3R/VGGT, Open3D meshing,
//! Blender archetypes) to the GPU lane in `civic-atlas-ingest` through the
//! `scene_foundry_render_jobs` table.
//!
//! Invariants carried from the handoff:
//! - The footprint and massing are authoritative; nothing here regenerates
//!   them.
//! - Provenance honesty: undocumented parts render as inference and are
//!   visibly flagged as inference, in the GLB materials, the node extras,
//!   and the provenance record asset.
//! - Done means real assets: manifests carry non-empty content hashes and a
//!   completed status, never the stub's queued placeholder.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;

use civic_atlas_reconstruction_engine::{
    fidelity_tier, reconstruction_node_id, AssetGenerator, AssetManifest, GeneratedAsset,
    MergedReconstructionSpec,
};
use civic_atlas_types::civic_atlas::v1::ReconstructionSpec;

pub mod glb;
pub mod jobs;
pub mod massing;
pub mod provenance;
pub mod store;
pub mod tier;

pub use jobs::{EnqueueOutcome, PgRenderJobQueue, RenderJobQueue};
pub use provenance::{
    classify_part, stamp_massing_texture_provenance, MaterialTreatment, ProvenanceRecord,
};
pub use store::{AssetStore, LocalDirAssetStore, StoredAsset};
pub use tier::{select_tier, RenderTier, TierDecision, TierThresholds};

pub const GENERATOR_NAME: &str = "scene_foundry";
pub const MANIFEST_STATUS_COMPLETED: &str = "completed";

pub const ASSET_TYPE_GEOMETRY: &str = "scene_foundry_geometry";
pub const ASSET_TYPE_PROVENANCE: &str = "scene_foundry_provenance_record";

#[derive(Clone)]
pub struct RendererConfig {
    pub thresholds: TierThresholds,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            thresholds: TierThresholds::default(),
        }
    }
}

/// The real Scene Foundry renderer.
pub struct SceneFoundryRenderer {
    store: Arc<dyn AssetStore>,
    jobs: Option<Arc<dyn RenderJobQueue>>,
    config: RendererConfig,
}

impl SceneFoundryRenderer {
    pub fn new(store: Arc<dyn AssetStore>) -> Self {
        Self {
            store,
            jobs: None,
            config: RendererConfig::default(),
        }
    }

    pub fn with_job_queue(mut self, jobs: Arc<dyn RenderJobQueue>) -> Self {
        self.jobs = Some(jobs);
        self
    }

    pub fn with_config(mut self, config: RendererConfig) -> Self {
        self.config = config;
        self
    }

    /// Build the provenance record document for the (stamped) spec.
    fn provenance_record(
        spec: &ReconstructionSpec,
        decision: &TierDecision,
        model: &massing::MassingModel,
    ) -> ProvenanceRecord {
        let parts = model
            .parts
            .iter()
            .map(|part| provenance::PartProvenanceRecord {
                node_id: reconstruction_node_id(spec, &part.field_path),
                part_type: format!("{:?}", part.kind).to_ascii_lowercase(),
                field_path: part.field_path.clone(),
                flag: part.flag.clone(),
            })
            .collect();
        ProvenanceRecord {
            spec_id: spec.spec_id.clone(),
            spec_version: spec.spec_version,
            render_tier: decision.tier.as_str().to_string(),
            render_tier_rationale: decision.rationale.clone(),
            generator: glb::GLB_GENERATOR.to_string(),
            parts,
            texture: provenance::texture_records(spec),
            photo_sources: provenance::photo_source_records(decision),
        }
    }
}

#[async_trait]
impl AssetGenerator for SceneFoundryRenderer {
    async fn generate(&self, merged: &MergedReconstructionSpec) -> Result<AssetManifest> {
        // Work on a stamped copy: the renderer records texture provenance
        // for what it actually produces (procedural PBR massing) without
        // mutating the caller's spec through the shared port. Call sites
        // that persist the spec apply `stamp_massing_texture_provenance`
        // before persisting so the stored spec carries the same record.
        let mut spec = merged.spec.clone();
        let decision = select_tier(&spec, &self.config.thresholds);
        stamp_massing_texture_provenance(&mut spec, decision.tier);

        let model = massing::build_massing(&spec).context("massing realization failed")?;
        let glb_bytes = glb::write_glb(&model, &spec, &decision).context("GLB authoring failed")?;
        let geometry = self
            .store
            .put(
                &spec.spec_id,
                spec.spec_version,
                "massing",
                "glb",
                &glb_bytes,
            )
            .await
            .context("storing massing geometry")?;

        let record = Self::provenance_record(&spec, &decision, &model);
        let record_bytes =
            serde_json::to_vec_pretty(&record).context("serializing provenance record")?;
        let provenance_asset = self
            .store
            .put(
                &spec.spec_id,
                spec.spec_version,
                "provenance",
                "json",
                &record_bytes,
            )
            .await
            .context("storing provenance record")?;

        // Refinement dispatch is best-effort: a failed enqueue degrades the
        // manifest honestly instead of failing the synchronous render.
        let refinement = match self.jobs.as_ref() {
            Some(queue) => match queue.enqueue(&spec, &decision).await {
                Ok(outcome) => outcome.as_str().to_string(),
                Err(error) => {
                    tracing::warn!(error = %error, spec_id = %spec.spec_id, "render refinement enqueue failed");
                    "enqueue_failed".to_string()
                }
            },
            None => "not_configured".to_string(),
        };

        let manifest_id = format!("scene-foundry:{}:v{}", spec.spec_id, spec.spec_version);
        let documented = model
            .parts
            .iter()
            .filter(|part| part.flag.documented)
            .count();
        let inferred = model.parts.len() - documented;

        let mut metadata: BTreeMap<String, String> = decision.metadata();
        metadata.insert("generator".to_string(), GENERATOR_NAME.to_string());
        metadata.insert("rendererCrate".to_string(), glb::GLB_GENERATOR.to_string());
        metadata.insert(
            "mergeConflictCount".to_string(),
            merged.conflicts.len().to_string(),
        );
        metadata.insert("refinement".to_string(), refinement.clone());
        metadata.insert(
            "refinementKind".to_string(),
            decision.tier.refinement_kind().to_string(),
        );
        metadata.insert("documentedPartCount".to_string(), documented.to_string());
        metadata.insert("inferredPartCount".to_string(), inferred.to_string());
        metadata.insert("roofForm".to_string(), model.roof_form.as_str().to_string());

        let mut geometry_metadata = metadata.clone();
        geometry_metadata.insert(
            "massingSource".to_string(),
            "procedural-massing-v1".to_string(),
        );
        let mut provenance_metadata = metadata.clone();
        provenance_metadata.insert("recordKind".to_string(), "part-provenance".to_string());

        let assets = vec![
            GeneratedAsset {
                asset_id: format!("{manifest_id}:geometry"),
                asset_type: ASSET_TYPE_GEOMETRY.to_string(),
                uri: geometry.uri,
                content_hash: geometry.content_hash,
                metadata: geometry_metadata,
            },
            GeneratedAsset {
                asset_id: format!("{manifest_id}:provenance"),
                asset_type: ASSET_TYPE_PROVENANCE.to_string(),
                uri: provenance_asset.uri,
                content_hash: provenance_asset.content_hash,
                metadata: provenance_metadata,
            },
        ];

        Ok(AssetManifest {
            manifest_id,
            spec_id: spec.spec_id.clone(),
            spec_version: spec.spec_version,
            fidelity_tier: fidelity_tier(&spec).to_string(),
            generator: GENERATOR_NAME.to_string(),
            status: MANIFEST_STATUS_COMPLETED.to_string(),
            assets,
            metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use civic_atlas_types::civic_atlas::v1::{
        DimensionRange, Facade, Mass, OpeningGrid, PartProvenance, ReconstructionSource,
        ReconstructionSourceType, Roof,
    };

    fn evidence(source_type: ReconstructionSourceType, id: &str) -> PartProvenance {
        PartProvenance {
            sources: vec![ReconstructionSource {
                source_id: id.to_string(),
                source_type: source_type as i32,
                uri: format!("https://example.org/{id}"),
                ..Default::default()
            }],
            part_confidence: 0.88,
            ..Default::default()
        }
    }

    fn merged_spec(with_photo: bool) -> MergedReconstructionSpec {
        let facade_provenance = if with_photo {
            evidence(ReconstructionSourceType::ArchivalPhoto, "photo-1")
        } else {
            evidence(ReconstructionSourceType::Map, "sanborn-1")
        };
        MergedReconstructionSpec {
            spec: ReconstructionSpec {
                spec_id: "recon-whaley-1900".to_string(),
                spec_version: 1,
                mass: Some(Mass {
                    provenance: Some(evidence(ReconstructionSourceType::Map, "sanborn-1")),
                    stories: 2,
                    height: Some(DimensionRange {
                        min: Some(7.0),
                        max: Some(7.0),
                        unit: "m".into(),
                    }),
                    width: Some(DimensionRange {
                        min: Some(10.0),
                        max: Some(10.0),
                        unit: "m".into(),
                    }),
                    depth: Some(DimensionRange {
                        min: Some(14.0),
                        max: Some(14.0),
                        unit: "m".into(),
                    }),
                    ..Default::default()
                }),
                facades: vec![Facade {
                    provenance: Some(facade_provenance),
                    facade_side: "front".to_string(),
                    primary_material: "brick".to_string(),
                    opening_grids: vec![OpeningGrid {
                        provenance: Some(evidence(ReconstructionSourceType::Map, "sanborn-1")),
                        bay_count: 3,
                        floor_count: 2,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                roof: Some(Roof {
                    provenance: Some(evidence(ReconstructionSourceType::Map, "sanborn-1")),
                    roof_type: "gable".to_string(),
                    roof_material: "slate".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            conflicts: Vec::new(),
        }
    }

    #[tokio::test]
    async fn generate_produces_real_completed_assets() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LocalDirAssetStore::new(dir.path(), "/assets/scene-foundry"));
        let renderer = SceneFoundryRenderer::new(store);

        let manifest = renderer.generate(&merged_spec(false)).await.unwrap();

        // The gate: not queued, real hashes, real bytes on disk.
        assert_eq!(manifest.status, MANIFEST_STATUS_COMPLETED);
        assert_eq!(manifest.assets.len(), 2);
        for asset in &manifest.assets {
            assert!(
                asset.content_hash.starts_with("sha256-") && asset.content_hash.len() > 10,
                "asset {} must carry a real content hash",
                asset.asset_id
            );
        }
        let geometry = &manifest.assets[0];
        assert_eq!(geometry.asset_type, ASSET_TYPE_GEOMETRY);
        assert!(geometry.uri.ends_with(".glb"));
        let stored_glb = dir
            .path()
            .join(geometry.uri.trim_start_matches("/assets/scene-foundry/"));
        let bytes = std::fs::read(&stored_glb).expect("GLB written to storage");
        assert_eq!(&bytes[0..4], b"glTF");
        assert_eq!(store::content_hash(&bytes), geometry.content_hash);

        let provenance_asset = &manifest.assets[1];
        assert_eq!(provenance_asset.asset_type, ASSET_TYPE_PROVENANCE);
        assert!(provenance_asset.uri.ends_with(".json"));
        let record_path = dir.path().join(
            provenance_asset
                .uri
                .trim_start_matches("/assets/scene-foundry/"),
        );
        let record: ProvenanceRecord =
            serde_json::from_slice(&std::fs::read(record_path).unwrap()).unwrap();
        assert_eq!(record.spec_id, "recon-whaley-1900");
        assert!(!record.parts.is_empty());
        assert!(record
            .parts
            .iter()
            .all(|part| part.node_id.starts_with("reconstruction-node:")));
        // Texture provenance recorded as procedural for the massing render.
        assert!(record
            .texture
            .iter()
            .all(|texture| texture.texture_source == provenance::TEXTURE_SOURCE_PROCEDURAL));

        // No-photo spec selects the description-only tier.
        assert_eq!(manifest.metadata["renderTier"], "tier_c_description_only");
        assert_eq!(manifest.metadata["refinement"], "not_configured");
    }

    #[tokio::test]
    async fn photo_spec_selects_tier_a_and_records_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LocalDirAssetStore::new(dir.path(), "/assets/scene-foundry"));
        let renderer = SceneFoundryRenderer::new(store);

        let manifest = renderer.generate(&merged_spec(true)).await.unwrap();
        assert_eq!(
            manifest.metadata["renderTier"],
            "tier_a_single_facade_photo"
        );
        assert_eq!(manifest.metadata["photoSourceCount"], "1");
        assert_eq!(manifest.metadata["refinementKind"], "single_facade_fit");
        assert_eq!(manifest.status, MANIFEST_STATUS_COMPLETED);
    }

    #[tokio::test]
    async fn identical_specs_are_content_addressed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LocalDirAssetStore::new(dir.path(), "/assets/scene-foundry"));
        let renderer = SceneFoundryRenderer::new(store);

        let first = renderer.generate(&merged_spec(false)).await.unwrap();
        let second = renderer.generate(&merged_spec(false)).await.unwrap();
        assert_eq!(first.assets[0].content_hash, second.assets[0].content_hash);
        assert_eq!(first.assets[0].uri, second.assets[0].uri);
    }
}
