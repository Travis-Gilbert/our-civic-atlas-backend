//! Civic Atlas procedural reconstruction engine.
//!
//! The engine implements the eight named reconstruction stages as
//! independently callable functions. External systems are represented
//! as ports so the trained building head, RustyRed, Theseus embeddings,
//! and Scene Foundry renderer can be swapped in without changing the
//! stage contracts.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{anyhow, ensure, Context, Result};
use async_trait::async_trait;
use civic_atlas_types::civic_atlas::v1::{
    CivicObject, DimensionRange, Facade, GroundFloor, Mass, OpeningGrid, OpeningOverride, Ornament,
    PartProvenance, ProvenanceCorrection, ReconstructionAsset, ReconstructionSource,
    ReconstructionSourceType, ReconstructionSpec, ReconstructionSpecStatus, Roof, TenantContext,
    TextureProvenance, TimeSlice,
};
use civic_atlas_types::theseus_bridge::v1::BatchSpacetimeEmbeddingRequest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{types::Json, PgPool, Postgres, Row, Transaction};
use tonic::Request;
use uuid::Uuid;

pub const SPACETIME_EMBEDDING_DIMS: usize = 256;
pub const DEFAULT_ADJACENT_RADIUS_M: f64 = 100.0;

#[derive(Debug, Clone)]
pub struct ReconstructionRequest {
    pub tenant_context: TenantContext,
    pub parcel_id: String,
    pub time_slice: TimeSlice,
    pub requested_by: String,
    pub auto_approve: bool,
}

impl ReconstructionRequest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.tenant_context.tenant_id.trim().is_empty(),
            "tenant_id is required"
        );
        ensure!(!self.parcel_id.trim().is_empty(), "parcel_id is required");
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceBundle<S = DecodedArtifact> {
    pub focus_building: Option<CivicObject>,
    pub direct: Vec<Artifact<S>>,
    pub adjacent: Vec<Artifact<S>>,
    pub temporal_predecessor: Option<CivicObject>,
    pub temporal_successor: Option<CivicObject>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectExtraction<S = ReconstructionSpec> {
    pub spec: S,
    pub populated_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockSubgraph<S = ReconstructionSpec> {
    pub focus_node: String,
    pub nodes: Vec<GraphNode<S>>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedBlockSubgraph<S = ReconstructionSpec> {
    pub focus_node: String,
    pub nodes: Vec<GraphNode<S>>,
    pub edges: Vec<GraphEdge>,
    pub embedding_model: String,
    pub embedding_model_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PriorReconstructionSpec<S = ReconstructionSpec> {
    pub spec: S,
    pub model_version: String,
    pub edge_confidences: Vec<EdgeRelationshipConfidence>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergedReconstructionSpec<S = ReconstructionSpec> {
    pub spec: S,
    pub conflicts: Vec<MergeConflict>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetManifest {
    pub manifest_id: String,
    pub spec_id: String,
    pub spec_version: u32,
    pub fidelity_tier: String,
    pub generator: String,
    pub status: String,
    pub assets: Vec<GeneratedAsset>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PipelineOutput<Spec = ReconstructionSpec, Source = DecodedArtifact> {
    pub evidence: EvidenceBundle<Source>,
    pub direct: DirectExtraction<Spec>,
    pub block_subgraph: BlockSubgraph<Spec>,
    pub embedded_subgraph: EmbeddedBlockSubgraph<Spec>,
    pub prior: PriorReconstructionSpec<Spec>,
    pub merged: MergedReconstructionSpec<Spec>,
    pub asset_manifest: AssetManifest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact<S = DecodedArtifact> {
    pub artifact_id: String,
    pub artifact_key: String,
    pub source_type: String,
    pub title: String,
    pub uri: String,
    pub citation: String,
    pub captured_at_ms: Option<i64>,
    pub fetched_at_ms: Option<i64>,
    pub content_hash: String,
    pub decoded: S,
    pub metadata: BTreeMap<String, String>,
}

impl<S> Artifact<S> {
    fn source(&self) -> ReconstructionSource {
        ReconstructionSource {
            source_id: self.artifact_id.clone(),
            source_type: source_type_from_str(&self.source_type) as i32,
            title: self.title.clone(),
            uri: self.uri.clone(),
            captured_at_ms: self.captured_at_ms,
            citation: self.citation.clone(),
            metadata: self
                .metadata
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum DecodedArtifact {
    SanbornSheet {
        footprint_wkt: Option<String>,
        story_count: Option<u32>,
        material_code: Option<String>,
        notation: Option<String>,
        roof_form: Option<String>,
    },
    Photo {
        visible_facades: Vec<String>,
        story_count: Option<u32>,
        bay_count: Option<u32>,
        roof_form: Option<String>,
        caption_text: Option<String>,
        scale_height_m: Option<f64>,
    },
    DirectoryEntry {
        business_name: Option<String>,
        residents: Vec<String>,
        address: Option<String>,
        use_type: Option<String>,
    },
    GisFeature {
        footprint_wkt: Option<String>,
        attributes: BTreeMap<String, Value>,
        source_layer: Option<String>,
        capture_date_ms: Option<i64>,
    },
    RasterTile {
        footprint_wkt: Option<String>,
        raster_uri: Option<String>,
        band_count: Option<u32>,
        capture_date_ms: Option<i64>,
    },
    Text {
        text: String,
    },
    Unknown {
        raw: Value,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphNode<S = ReconstructionSpec> {
    pub node_id: String,
    pub node_type: String,
    pub object: Option<CivicObject>,
    pub direct_spec: Option<S>,
    pub embedding: Vec<f32>,
    pub missing_embedding: bool,
    pub attributes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairformerNodeFeature {
    pub node_id: String,
    pub embedding_dim: usize,
    pub direct_field_count: usize,
    pub missing_embedding: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub weight: f64,
    pub distance_m: Option<f64>,
    pub time_distance_years: Option<f64>,
    pub confidence: Option<f64>,
    pub attributes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairformerEdgeFeature {
    pub source: String,
    pub target: String,
    pub relation: String,
    pub distance_m: f64,
    pub time_distance_years: f64,
    pub shared_party_wall: bool,
    pub shared_setback_line: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeRelationshipConfidence {
    pub source: String,
    pub target: String,
    pub relation: String,
    pub confidence: f64,
    pub model_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconstructionNodeType {
    Site,
    Building,
    Level,
    Mass,
    Facade,
    OpeningGrid,
    GroundFloor,
    Roof,
    Ornament,
    TextureFace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconstructionNodeMetadata {
    pub spec_id: String,
    pub civic_object_id: String,
    pub building_id: String,
    pub field_path: String,
    pub label: String,
    pub source_ids: Vec<String>,
    pub confidence: Option<f64>,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconstructionSceneNode {
    pub id: String,
    pub node_type: ReconstructionNodeType,
    pub parent_id: Option<String>,
    pub children: Vec<String>,
    pub visible: bool,
    pub metadata: ReconstructionNodeMetadata,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconstructionNodeTree {
    pub version: u32,
    pub source: String,
    pub root_node_ids: Vec<String>,
    pub nodes: BTreeMap<String, ReconstructionSceneNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureProvenanceProjection {
    pub texture_source: String,
    pub lora_archetype: Option<String>,
    pub lora_weight: Option<f64>,
    pub conditioning_source_id: Option<String>,
    pub texture_confidence: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeConflict {
    pub field_path: String,
    pub direct_value: String,
    pub prior_value: String,
    pub direct_confidence: f64,
    pub threshold: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedAsset {
    pub asset_id: String,
    pub asset_type: String,
    pub uri: String,
    pub content_hash: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PartRecord {
    pub key: String,
    pub part_type: String,
    pub payload: Value,
    pub confidence: f64,
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingBatch {
    pub embeddings: HashMap<String, NodeEmbedding>,
    pub model: String,
    pub model_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeEmbedding {
    pub embedding: Vec<f32>,
    pub missing: bool,
}

#[async_trait]
pub trait EvidenceRepository<S = DecodedArtifact>: Send + Sync {
    async fn parcel_history(&self, request: &ReconstructionRequest) -> Result<Vec<CivicObject>>;
    async fn direct_artifacts(
        &self,
        request: &ReconstructionRequest,
        focus_building: Option<&CivicObject>,
    ) -> Result<Vec<Artifact<S>>>;
    async fn adjacent_artifacts(
        &self,
        request: &ReconstructionRequest,
        focus_building: Option<&CivicObject>,
        radius_m: f64,
    ) -> Result<Vec<Artifact<S>>>;
}

#[async_trait]
pub trait BlockSubgraphRepository<S = ReconstructionSpec, Source = DecodedArtifact>:
    Send + Sync
{
    async fn block_subgraph(
        &self,
        request: &ReconstructionRequest,
        evidence: &EvidenceBundle<Source>,
    ) -> Result<BlockSubgraph<S>>;
}

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embeddings(
        &self,
        tenant_context: &TenantContext,
        node_ids: &[String],
    ) -> Result<EmbeddingBatch>;
}

pub trait PriorModel<S = ReconstructionSpec>: Send + Sync {
    fn infer(
        &self,
        graph: &EmbeddedBlockSubgraph<S>,
        direct: &DirectExtraction<S>,
    ) -> Result<PriorReconstructionSpec<S>>;
}

#[async_trait]
pub trait AssetGenerator<S = ReconstructionSpec>: Send + Sync {
    async fn generate(&self, spec: &MergedReconstructionSpec<S>) -> Result<AssetManifest>;
}

pub trait ReconstructionDomain {
    type Source;
    type Spec: Clone;

    fn extract_direct(
        request: &ReconstructionRequest,
        evidence: &EvidenceBundle<Self::Source>,
    ) -> Result<DirectExtraction<Self::Spec>>;

    fn merge(
        direct: &DirectExtraction<Self::Spec>,
        prior: &PriorReconstructionSpec<Self::Spec>,
        config: MergeConfig,
    ) -> Result<MergedReconstructionSpec<Self::Spec>>;

    fn spec_to_subgraph(_spec: &Self::Spec) -> BlockSubgraph<Self::Spec> {
        BlockSubgraph {
            focus_node: String::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BuildingDomain;

impl ReconstructionDomain for BuildingDomain {
    type Source = DecodedArtifact;
    type Spec = ReconstructionSpec;

    fn extract_direct(
        request: &ReconstructionRequest,
        evidence: &EvidenceBundle<Self::Source>,
    ) -> Result<DirectExtraction<Self::Spec>> {
        extract_direct(request, evidence)
    }

    fn merge(
        direct: &DirectExtraction<Self::Spec>,
        prior: &PriorReconstructionSpec<Self::Spec>,
        config: MergeConfig,
    ) -> Result<MergedReconstructionSpec<Self::Spec>> {
        merge_evidence_prior(direct, prior, config)
    }

    fn spec_to_subgraph(spec: &Self::Spec) -> BlockSubgraph<Self::Spec> {
        let focus_node = if spec.building_id.is_empty() {
            format!("parcel:{}", spec.parcel_id)
        } else {
            spec.building_id.clone()
        };
        BlockSubgraph {
            focus_node: focus_node.clone(),
            nodes: vec![GraphNode {
                node_id: focus_node,
                node_type: "BuildingPresence".to_string(),
                object: None,
                direct_spec: Some(spec.clone()),
                embedding: Vec::new(),
                missing_embedding: true,
                attributes: BTreeMap::new(),
            }],
            edges: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MergeConfig {
    pub low_confidence_threshold: f64,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            low_confidence_threshold: 0.7,
        }
    }
}

pub async fn run_full_pipeline<R, E, M, A>(
    request: ReconstructionRequest,
    repository: &R,
    embeddings: &E,
    prior_model: &M,
    asset_generator: &A,
) -> Result<PipelineOutput<ReconstructionSpec, DecodedArtifact>>
where
    R: EvidenceRepository<DecodedArtifact>
        + BlockSubgraphRepository<ReconstructionSpec, DecodedArtifact>,
    E: EmbeddingProvider,
    M: PriorModel<ReconstructionSpec>,
    A: AssetGenerator<ReconstructionSpec>,
{
    run_domain_pipeline::<BuildingDomain, _, _, _, _>(
        request,
        repository,
        embeddings,
        prior_model,
        asset_generator,
    )
    .await
}

pub async fn run_domain_pipeline<D, R, E, M, A>(
    request: ReconstructionRequest,
    repository: &R,
    embeddings: &E,
    prior_model: &M,
    asset_generator: &A,
) -> Result<PipelineOutput<D::Spec, D::Source>>
where
    D: ReconstructionDomain,
    R: EvidenceRepository<D::Source> + BlockSubgraphRepository<D::Spec, D::Source>,
    E: EmbeddingProvider,
    M: PriorModel<D::Spec>,
    A: AssetGenerator<D::Spec>,
{
    request.validate()?;
    let evidence = assemble_evidence(repository, &request).await?;
    let direct = D::extract_direct(&request, &evidence)?;
    let block_subgraph = build_block_subgraph(repository, &request, &evidence, &direct).await?;
    let embedded_subgraph = hydrate_embeddings(embeddings, &request, &block_subgraph).await?;
    let prior = infer_priors(prior_model, &embedded_subgraph, &direct)?;
    let merged = D::merge(&direct, &prior, MergeConfig::default())?;
    let asset_manifest = generate_assets(asset_generator, &merged).await?;
    Ok(PipelineOutput {
        evidence,
        direct,
        block_subgraph,
        embedded_subgraph,
        prior,
        merged,
        asset_manifest,
    })
}

pub async fn assemble_evidence<R, S>(
    repository: &R,
    request: &ReconstructionRequest,
) -> Result<EvidenceBundle<S>>
where
    R: EvidenceRepository<S>,
{
    let history = repository.parcel_history(request).await?;
    let focus_index = history
        .iter()
        .position(|object| object_matches_time(object, &request.time_slice))
        .or_else(|| (!history.is_empty()).then_some(0));
    let focus_building = focus_index.and_then(|index| history.get(index).cloned());
    let temporal_predecessor = focus_index
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| history.get(index).cloned());
    let temporal_successor = focus_index.and_then(|index| history.get(index + 1).cloned());
    let direct = repository
        .direct_artifacts(request, focus_building.as_ref())
        .await?;
    let adjacent = repository
        .adjacent_artifacts(request, focus_building.as_ref(), DEFAULT_ADJACENT_RADIUS_M)
        .await?;
    Ok(EvidenceBundle {
        focus_building,
        direct,
        adjacent,
        temporal_predecessor,
        temporal_successor,
    })
}

pub fn extract_direct(
    request: &ReconstructionRequest,
    evidence: &EvidenceBundle,
) -> Result<DirectExtraction> {
    request.validate()?;
    let mut spec = base_spec(request, evidence.focus_building.as_ref());
    let mut populated_fields = Vec::new();

    for artifact in &evidence.direct {
        match &artifact.decoded {
            DecodedArtifact::SanbornSheet {
                story_count,
                material_code,
                notation,
                roof_form,
                ..
            } => {
                if let Some(story_count) = story_count {
                    match spec.mass.as_mut() {
                        Some(mass) if mass.stories == 0 => {
                            mass.stories = *story_count;
                            populated_fields.push("mass.stories".to_string());
                        }
                        None => {
                            spec.mass = Some(Mass {
                                provenance: Some(provenance(
                                    artifact,
                                    0.92,
                                    false,
                                    "sanborn polygon/notation",
                                )),
                                form: "mapped footprint".to_string(),
                                stories: *story_count,
                                ..Default::default()
                            });
                            populated_fields.push("mass.stories".to_string());
                        }
                        Some(_) => {}
                    }
                }
                if spec.facades.is_empty() {
                    spec.facades.push(Facade {
                        provenance: Some(provenance(artifact, 0.9, false, "sanborn color code")),
                        facade_side: "primary".to_string(),
                        primary_material: material_code
                            .as_deref()
                            .map(sanborn_material)
                            .unwrap_or("unknown")
                            .to_string(),
                        color: material_code.clone().unwrap_or_default(),
                        ..Default::default()
                    });
                    populated_fields.push("facades[0].primary_material".to_string());
                }
                if let Some(roof_form) = roof_form.as_ref().filter(|value| !value.is_empty()) {
                    spec.roof = Some(Roof {
                        provenance: Some(provenance(
                            artifact,
                            0.78,
                            false,
                            "sanborn roof notation",
                        )),
                        roof_type: roof_form.clone(),
                        ..Default::default()
                    });
                    populated_fields.push("roof.roof_type".to_string());
                }
                if spec.ground_floor.is_none()
                    && notation
                        .as_deref()
                        .map(|text| text.to_ascii_lowercase().contains("store"))
                        .unwrap_or(false)
                {
                    spec.ground_floor = Some(GroundFloor {
                        provenance: Some(provenance(artifact, 0.82, false, "sanborn use notation")),
                        use_type: "commercial".to_string(),
                        storefront_type: "storefront".to_string(),
                        ..Default::default()
                    });
                    populated_fields.push("ground_floor.use_type".to_string());
                }
            }
            DecodedArtifact::Photo {
                visible_facades,
                story_count,
                bay_count,
                roof_form,
                scale_height_m,
                ..
            } => {
                let height = scale_height_m.map(|height| DimensionRange {
                    min: Some(height * 0.95),
                    max: Some(height * 1.05),
                    unit: "m".to_string(),
                });
                if spec.mass.is_none() && (story_count.is_some() || height.is_some()) {
                    spec.mass = Some(Mass {
                        provenance: Some(provenance(
                            artifact,
                            0.86,
                            false,
                            "photo scale/story count",
                        )),
                        form: "photo-observed mass".to_string(),
                        stories: story_count.unwrap_or_default(),
                        height,
                        ..Default::default()
                    });
                    if scale_height_m.is_some() {
                        populated_fields.push("mass.height".to_string());
                    }
                    if story_count.is_some() {
                        populated_fields.push("mass.stories".to_string());
                    }
                } else if let Some(mass) = spec.mass.as_mut() {
                    if mass.height.is_none() {
                        mass.height = height;
                        if mass.height.is_some() {
                            populated_fields.push("mass.height".to_string());
                        }
                    }
                    if mass.stories == 0 {
                        if let Some(story_count) = story_count {
                            mass.stories = *story_count;
                            populated_fields.push("mass.stories".to_string());
                        }
                    }
                }
                if spec.facades.is_empty() || spec.facades[0].opening_grids.is_empty() {
                    let orientation = visible_facades
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "primary".to_string());
                    let grid = OpeningGrid {
                        provenance: Some(provenance(
                            artifact,
                            0.83,
                            false,
                            "photo bay segmentation",
                        )),
                        bay_count: bay_count.unwrap_or_default(),
                        floor_count: story_count.unwrap_or_default(),
                        window_pattern: "photo_observed".to_string(),
                        ..Default::default()
                    };
                    if spec.facades.is_empty() {
                        spec.facades.push(Facade {
                            provenance: Some(provenance(artifact, 0.72, false, "photo facade")),
                            facade_side: orientation,
                            opening_grids: vec![grid],
                            ..Default::default()
                        });
                    } else {
                        spec.facades[0].opening_grids.push(grid);
                    }
                    populated_fields.push("facades[0].opening_grids".to_string());
                }
                if let Some(roof_form) = roof_form.as_ref().filter(|value| !value.is_empty()) {
                    spec.roof = Some(Roof {
                        provenance: Some(provenance(artifact, 0.82, false, "photo roofline")),
                        roof_type: roof_form.clone(),
                        ..Default::default()
                    });
                    populated_fields.push("roof.roof_type".to_string());
                }
            }
            DecodedArtifact::DirectoryEntry { use_type, .. } => {
                if spec.ground_floor.is_none() {
                    spec.ground_floor = Some(GroundFloor {
                        provenance: Some(provenance(artifact, 0.76, false, "directory entry")),
                        use_type: use_type.clone().unwrap_or_else(|| "occupied".to_string()),
                        ..Default::default()
                    });
                    populated_fields.push("ground_floor.use_type".to_string());
                }
            }
            DecodedArtifact::GisFeature { attributes, .. } => {
                let story_count = attribute_u32_any(
                    attributes,
                    &[
                        "stories",
                        "story_count",
                        "num_stories",
                        "Cib_Storie",
                        "Dwelling_U",
                    ],
                );
                let use_type = attribute_string_any(
                    attributes,
                    &[
                        "use_type",
                        "Use_Type",
                        "property_class",
                        "Prop_Class",
                        "LandUse",
                    ],
                );
                let primary_material = attribute_string_any(
                    attributes,
                    &["primary_material", "building_material", "material"],
                );

                if spec.mass.is_none() {
                    spec.mass = Some(Mass {
                        provenance: Some(provenance(artifact, 0.95, false, "GIS parcel feature")),
                        form: "parcel polygon".to_string(),
                        stories: story_count.unwrap_or_default(),
                        ..Default::default()
                    });
                    populated_fields.push("mass.form".to_string());
                    if story_count.is_some() {
                        populated_fields.push("mass.stories".to_string());
                    }
                } else if let Some(mass) = spec.mass.as_mut() {
                    if mass.stories == 0 {
                        if let Some(story_count) = story_count {
                            mass.stories = story_count;
                            populated_fields.push("mass.stories".to_string());
                        }
                    }
                }

                if spec.facades.is_empty() {
                    if let Some(primary_material) = primary_material {
                        spec.facades.push(Facade {
                            provenance: Some(provenance(
                                artifact,
                                0.91,
                                false,
                                "GIS assessor attribute",
                            )),
                            facade_side: "primary".to_string(),
                            primary_material,
                            ..Default::default()
                        });
                        populated_fields.push("facades[0].primary_material".to_string());
                    }
                }

                if spec.ground_floor.is_none() {
                    if let Some(use_type) = use_type {
                        spec.ground_floor = Some(GroundFloor {
                            provenance: Some(provenance(
                                artifact,
                                0.9,
                                false,
                                "GIS assessor attribute",
                            )),
                            use_type,
                            ..Default::default()
                        });
                        populated_fields.push("ground_floor.use_type".to_string());
                    }
                }
            }
            DecodedArtifact::RasterTile { .. } => {}
            DecodedArtifact::Text { text } => {
                if text.to_ascii_lowercase().contains("cornice") {
                    spec.ornaments.push(Ornament {
                        provenance: Some(provenance(artifact, 0.7, false, "source text mention")),
                        ornament_id: "cornice".to_string(),
                        ornament_kind: "cornice".to_string(),
                        location: "roofline".to_string(),
                        ..Default::default()
                    });
                    populated_fields.push("ornaments.cornice".to_string());
                }
            }
            DecodedArtifact::Unknown { .. } => {}
        }
    }

    if spec.mass.is_none() {
        if let Some(focus) = evidence.focus_building.as_ref() {
            if !focus.geometry_json.trim().is_empty() {
                spec.mass = Some(Mass {
                    provenance: Some(system_provenance(
                        "parcel-footprint",
                        "Modern parcel/building footprint fallback",
                        0.58,
                        false,
                    )),
                    form: "footprint fallback".to_string(),
                    ..Default::default()
                });
                populated_fields.push("mass.form".to_string());
            }
        }
    }

    Ok(DirectExtraction {
        spec,
        populated_fields,
    })
}

pub async fn build_block_subgraph<R, Spec, Source>(
    repository: &R,
    request: &ReconstructionRequest,
    evidence: &EvidenceBundle<Source>,
    direct: &DirectExtraction<Spec>,
) -> Result<BlockSubgraph<Spec>>
where
    R: BlockSubgraphRepository<Spec, Source>,
    Spec: Clone,
{
    let mut graph = repository.block_subgraph(request, evidence).await?;
    if graph.focus_node.is_empty() {
        graph.focus_node = evidence
            .focus_building
            .as_ref()
            .map(|object| object.id.clone())
            .unwrap_or_else(|| format!("parcel:{}", request.parcel_id));
    }
    if graph
        .nodes
        .iter()
        .all(|node| node.node_id != graph.focus_node)
    {
        graph.nodes.push(GraphNode {
            node_id: graph.focus_node.clone(),
            node_type: "BuildingPresence".to_string(),
            object: evidence.focus_building.clone(),
            direct_spec: Some(direct.spec.clone()),
            embedding: Vec::new(),
            missing_embedding: true,
            attributes: BTreeMap::new(),
        });
    }
    for node in &mut graph.nodes {
        if node.node_id == graph.focus_node && node.direct_spec.is_none() {
            node.direct_spec = Some(direct.spec.clone());
        }
    }
    Ok(graph)
}

pub async fn hydrate_embeddings<E, S>(
    provider: &E,
    request: &ReconstructionRequest,
    graph: &BlockSubgraph<S>,
) -> Result<EmbeddedBlockSubgraph<S>>
where
    E: EmbeddingProvider,
    S: Clone,
{
    let node_ids: Vec<String> = graph
        .nodes
        .iter()
        .map(|node| node.node_id.clone())
        .collect();
    let mut batch = provider
        .embeddings(&request.tenant_context, &node_ids)
        .await
        .context("hydrating spacetime embeddings")?;
    let nodes = graph
        .nodes
        .iter()
        .cloned()
        .map(|mut node| {
            match batch.embeddings.remove(&node.node_id) {
                Some(embedding) => {
                    node.embedding = normalized_embedding(embedding.embedding);
                    node.missing_embedding = embedding.missing;
                }
                None => {
                    node.embedding = vec![0.0; SPACETIME_EMBEDDING_DIMS];
                    node.missing_embedding = true;
                }
            }
            node
        })
        .collect();
    Ok(EmbeddedBlockSubgraph {
        focus_node: graph.focus_node.clone(),
        nodes,
        edges: graph.edges.clone(),
        embedding_model: batch.model,
        embedding_model_version: batch.model_version,
    })
}

pub fn infer_priors<M, S>(
    model: &M,
    graph: &EmbeddedBlockSubgraph<S>,
    direct: &DirectExtraction<S>,
) -> Result<PriorReconstructionSpec<S>>
where
    M: PriorModel<S>,
{
    model.infer(graph, direct)
}

pub fn merge_evidence_prior(
    direct: &DirectExtraction,
    prior: &PriorReconstructionSpec,
    config: MergeConfig,
) -> Result<MergedReconstructionSpec> {
    let mut merged = prior.spec.clone();
    merged.tenant_context = direct.spec.tenant_context.clone();
    merged.spec_id = direct.spec.spec_id.clone();
    merged.civic_object_id = direct.spec.civic_object_id.clone();
    merged.building_id = direct.spec.building_id.clone();
    merged.parcel_id = direct.spec.parcel_id.clone();
    merged.block_id = direct.spec.block_id.clone();
    merged.title = direct.spec.title.clone();
    merged.spec_version = direct.spec.spec_version.max(1);
    merged.created_by = direct.spec.created_by.clone();
    merged.status = ReconstructionSpecStatus::Draft as i32;

    let mut conflicts = Vec::new();

    if let Some(direct_mass) = direct.spec.mass.as_ref() {
        if let Some(prior_mass) = prior.spec.mass.as_ref() {
            detect_u32_conflict(
                &mut conflicts,
                "mass.stories",
                direct_mass.stories,
                prior_mass.stories,
                confidence(direct_mass.provenance.as_ref()),
                config.low_confidence_threshold,
            );
        }
        merged.mass = Some(merge_mass(
            direct_mass,
            prior.spec.mass.as_ref(),
            prior.model_version.as_str(),
        ));
    }

    if !direct.spec.facades.is_empty() {
        merged.facades = merge_facades(&direct.spec.facades, &prior.spec.facades);
    }

    if let Some(direct_roof) = direct.spec.roof.as_ref() {
        if let Some(prior_roof) = prior.spec.roof.as_ref() {
            detect_string_conflict(
                &mut conflicts,
                "roof.roof_type",
                &direct_roof.roof_type,
                &prior_roof.roof_type,
                confidence(direct_roof.provenance.as_ref()),
                config.low_confidence_threshold,
            );
        }
        merged.roof = Some(merge_roof(direct_roof, prior.spec.roof.as_ref()));
    }

    if !direct.spec.ornaments.is_empty() {
        merged.ornaments = direct.spec.ornaments.clone();
    }

    if let Some(direct_ground_floor) = direct.spec.ground_floor.as_ref() {
        merged.ground_floor = Some(merge_ground_floor(
            direct_ground_floor,
            prior.spec.ground_floor.as_ref(),
        ));
    }

    if !conflicts.is_empty() {
        merged.metadata.insert(
            "mergeConflictCount".to_string(),
            conflicts.len().to_string(),
        );
    }

    Ok(MergedReconstructionSpec {
        spec: merged,
        conflicts,
    })
}

pub async fn generate_assets<A, S>(
    generator: &A,
    merged: &MergedReconstructionSpec<S>,
) -> Result<AssetManifest>
where
    A: AssetGenerator<S>,
{
    generator.generate(merged).await
}

#[derive(Debug, Clone)]
pub struct PairformerCivicPriorModel {
    model_version: String,
}

impl PairformerCivicPriorModel {
    pub fn new(model_version: impl Into<String>) -> Self {
        Self {
            model_version: model_version.into(),
        }
    }
}

impl Default for PairformerCivicPriorModel {
    fn default() -> Self {
        Self::new("civic-pairformer-adapter/heuristic-v1")
    }
}

impl PriorModel for PairformerCivicPriorModel {
    fn infer(
        &self,
        graph: &EmbeddedBlockSubgraph,
        direct: &DirectExtraction,
    ) -> Result<PriorReconstructionSpec> {
        let node_features = pairformer_node_features(graph);
        let edge_features = pairformer_edge_features(graph);
        let mut spec = direct.spec.clone();
        spec.status = ReconstructionSpecStatus::Draft as i32;

        let neighbor_specs: Vec<&ReconstructionSpec> = graph
            .nodes
            .iter()
            .filter(|node| node.node_id != graph.focus_node)
            .filter_map(|node| node.direct_spec.as_ref())
            .collect();

        let story_count = mode_u32(
            neighbor_specs
                .iter()
                .filter_map(|spec| spec.mass.as_ref().map(|mass| mass.stories))
                .filter(|count| *count > 0),
        )
        .unwrap_or(2);
        let material = mode_string(neighbor_specs.iter().flat_map(|spec| {
            spec.facades
                .iter()
                .map(|facade| facade.primary_material.as_str())
                .filter(|value| !value.is_empty())
        }))
        .unwrap_or_else(|| "brick".to_string());
        let roof_form = mode_string(
            neighbor_specs
                .iter()
                .filter_map(|spec| spec.roof.as_ref().map(|roof| roof.roof_type.as_str()))
                .filter(|value| !value.is_empty()),
        )
        .unwrap_or_else(|| {
            if material.contains("brick") {
                "flat".to_string()
            } else {
                "gable".to_string()
            }
        });
        let bay_count = mode_u32(neighbor_specs.iter().flat_map(|spec| {
            spec.facades
                .iter()
                .flat_map(|facade| facade.opening_grids.iter())
                .map(|grid| grid.bay_count)
                .filter(|count| *count > 0)
        }))
        .unwrap_or(3);

        if spec.mass.is_none() {
            spec.mass = Some(Mass {
                provenance: Some(prior_provenance(&self.model_version, 0.54)),
                form: "block-coherent mass".to_string(),
                stories: story_count,
                height: Some(DimensionRange {
                    min: Some((story_count as f64) * 2.8),
                    max: Some((story_count as f64) * 3.7),
                    unit: "m".to_string(),
                }),
                ..Default::default()
            });
        }
        if spec.facades.is_empty() {
            spec.facades.push(Facade {
                provenance: Some(prior_provenance(&self.model_version, 0.51)),
                facade_side: "primary".to_string(),
                primary_material: material,
                opening_grids: vec![OpeningGrid {
                    provenance: Some(prior_provenance(&self.model_version, 0.48)),
                    bay_count,
                    floor_count: story_count,
                    window_pattern: "block_coherent".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            });
        }
        if spec.roof.is_none() {
            spec.roof = Some(Roof {
                provenance: Some(prior_provenance(&self.model_version, 0.47)),
                roof_type: roof_form,
                roof_material: "era-typology prior".to_string(),
                ..Default::default()
            });
        }
        if spec.ground_floor.is_none() {
            let use_type = if spec
                .facades
                .first()
                .map(|facade| facade.primary_material.contains("brick"))
                .unwrap_or(false)
            {
                "commercial"
            } else {
                "residential"
            };
            spec.ground_floor = Some(GroundFloor {
                provenance: Some(prior_provenance(&self.model_version, 0.44)),
                use_type: use_type.to_string(),
                storefront_type: if use_type == "commercial" {
                    "storefront".to_string()
                } else {
                    String::new()
                },
                entry_location: "primary facade".to_string(),
                ..Default::default()
            });
        }
        if spec.ornaments.is_empty() {
            spec.ornaments.push(Ornament {
                provenance: Some(prior_provenance(&self.model_version, 0.32)),
                ornament_id: "none-observed".to_string(),
                ornament_kind: "none observed".to_string(),
                location: String::new(),
                ornament_material: String::new(),
                ..Default::default()
            });
        }
        spec.metadata.insert(
            "priorModel".to_string(),
            "civic Pairformer adapter fallback".to_string(),
        );
        spec.metadata.insert(
            "pairformerNodeFeatureCount".to_string(),
            node_features.len().to_string(),
        );
        spec.metadata.insert(
            "pairformerEdgeFeatureCount".to_string(),
            edge_features.len().to_string(),
        );

        Ok(PriorReconstructionSpec {
            spec,
            model_version: self.model_version.clone(),
            edge_confidences: publishable_edge_confidences(graph, &self.model_version),
        })
    }
}

pub type BlockCoherentPriorModel = PairformerCivicPriorModel;

#[derive(Debug, Clone)]
pub struct ZeroEmbeddingProvider {
    model: String,
    model_version: String,
}

impl Default for ZeroEmbeddingProvider {
    fn default() -> Self {
        Self {
            model: "zero-spacetime-embedding".to_string(),
            model_version: "missing-upstream-v1".to_string(),
        }
    }
}

#[async_trait]
impl EmbeddingProvider for ZeroEmbeddingProvider {
    async fn embeddings(
        &self,
        _tenant_context: &TenantContext,
        node_ids: &[String],
    ) -> Result<EmbeddingBatch> {
        Ok(EmbeddingBatch {
            embeddings: node_ids
                .iter()
                .map(|node_id| {
                    (
                        node_id.clone(),
                        NodeEmbedding {
                            embedding: vec![0.0; SPACETIME_EMBEDDING_DIMS],
                            missing: true,
                        },
                    )
                })
                .collect(),
            model: self.model.clone(),
            model_version: self.model_version.clone(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct TheseusBatchEmbeddingProvider {
    url: String,
}

impl TheseusBatchEmbeddingProvider {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

#[async_trait]
impl EmbeddingProvider for TheseusBatchEmbeddingProvider {
    async fn embeddings(
        &self,
        tenant_context: &TenantContext,
        node_ids: &[String],
    ) -> Result<EmbeddingBatch> {
        let mut client = theseus_client::TheseusClient::connect(&self.url)
            .await
            .context("connecting to Theseus bridge")?;
        let response = client
            .bridge()
            .get_batch_spacetime_embeddings(Request::new(BatchSpacetimeEmbeddingRequest {
                tenant_context: Some(tenant_context.clone()),
                node_ids: node_ids.to_vec(),
            }))
            .await
            .context("calling GetBatchSpacetimeEmbeddings")?
            .into_inner();
        let embeddings = response
            .embeddings
            .into_iter()
            .map(|(node_id, embedding)| {
                (
                    node_id,
                    NodeEmbedding {
                        embedding: embedding.embedding,
                        missing: embedding.missing,
                    },
                )
            })
            .collect();
        Ok(EmbeddingBatch {
            embeddings,
            model: response.model,
            model_version: response.model_version,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SceneFoundryManifestGenerator {
    uri_prefix: String,
}

impl SceneFoundryManifestGenerator {
    pub fn new(uri_prefix: impl Into<String>) -> Self {
        Self {
            uri_prefix: uri_prefix.into(),
        }
    }
}

impl Default for SceneFoundryManifestGenerator {
    fn default() -> Self {
        Self::new("scene-foundry://queued")
    }
}

#[async_trait]
impl AssetGenerator for SceneFoundryManifestGenerator {
    async fn generate(&self, merged: &MergedReconstructionSpec) -> Result<AssetManifest> {
        let spec = &merged.spec;
        let manifest_id = format!("scene-foundry:{}:v{}", spec.spec_id, spec.spec_version);
        let uri = format!(
            "{}/{}/v{}/manifest.json",
            self.uri_prefix.trim_end_matches('/'),
            spec.spec_id,
            spec.spec_version
        );
        let mut metadata = BTreeMap::new();
        metadata.insert("generator".to_string(), "scene_foundry".to_string());
        metadata.insert(
            "mergeConflictCount".to_string(),
            merged.conflicts.len().to_string(),
        );
        let asset = GeneratedAsset {
            asset_id: manifest_id.clone(),
            asset_type: "scene_foundry_manifest".to_string(),
            uri,
            content_hash: String::new(),
            metadata: metadata.clone(),
        };
        Ok(AssetManifest {
            manifest_id,
            spec_id: spec.spec_id.clone(),
            spec_version: spec.spec_version,
            fidelity_tier: fidelity_tier(spec).to_string(),
            generator: "scene_foundry".to_string(),
            status: "queued".to_string(),
            assets: vec![asset],
            metadata,
        })
    }
}

#[derive(Clone)]
pub struct PostgisRepository {
    pool: PgPool,
}

impl PostgisRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl EvidenceRepository for PostgisRepository {
    async fn parcel_history(&self, request: &ReconstructionRequest) -> Result<Vec<CivicObject>> {
        let mut tx = self.pool.begin().await?;
        let tenant_id = resolve_tenant_id_in_tx(&mut tx, &request.tenant_context.tenant_id).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let rows = sqlx::query(
            r#"
            SELECT b.id, b.civic_object_id, b.t_start_ms, b.t_end_ms,
                   ST_AsGeoJSON(COALESCE(b.geom, p.geom)) AS geometry_json,
                   b.properties, p.parcel_key, p.properties AS parcel_properties
            FROM parcels p
            LEFT JOIN buildings b
              ON b.tenant_id = p.tenant_id AND b.parcel_id = p.id
            WHERE p.tenant_id = $1
              AND (p.id::text = $2 OR p.parcel_key = $2)
            ORDER BY b.t_start_ms NULLS FIRST, b.t_end_ms NULLS LAST
            "#,
        )
        .bind(tenant_id)
        .bind(&request.parcel_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                civic_object_from_building_row(row, &request.tenant_context.tenant_id)
            })
            .collect())
    }

    async fn direct_artifacts(
        &self,
        request: &ReconstructionRequest,
        focus_building: Option<&CivicObject>,
    ) -> Result<Vec<Artifact>> {
        let mut tx = self.pool.begin().await?;
        let tenant_id = resolve_tenant_id_in_tx(&mut tx, &request.tenant_context.tenant_id).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let focus_building_id = focus_building
            .and_then(|object| object.id.parse::<Uuid>().ok())
            .unwrap_or(Uuid::nil());
        let rows = sqlx::query(
            r#"
            SELECT a.id, a.artifact_key, a.source_type, a.title,
                   COALESCE(a.uri, '') AS uri,
                   COALESCE(a.citation, '') AS citation,
                   a.captured_at_ms, a.payload_jsonb,
                   aa.t_start_ms, aa.t_end_ms
            FROM artifacts a
            INNER JOIN artifact_anchors aa
              ON aa.tenant_id = a.tenant_id AND aa.artifact_id = a.id
            LEFT JOIN parcels p
              ON p.tenant_id = aa.tenant_id AND p.id = aa.parcel_id
            WHERE a.tenant_id = $1
              AND (
                   p.id::text = $2
                OR p.parcel_key = $2
                OR aa.building_id = NULLIF($3, '00000000-0000-0000-0000-000000000000')::uuid
              )
              AND ($4::bigint IS NULL OR COALESCE(aa.t_end_ms, a.captured_at_ms, $4) >= $4)
              AND ($5::bigint IS NULL OR COALESCE(aa.t_start_ms, a.captured_at_ms, $5) <= $5)
            ORDER BY a.captured_at_ms NULLS LAST, a.title
            "#,
        )
        .bind(tenant_id)
        .bind(&request.parcel_id)
        .bind(focus_building_id.to_string())
        .bind(request.time_slice.start_ms.or(request.time_slice.at_ms))
        .bind(request.time_slice.end_ms.or(request.time_slice.at_ms))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(artifact_from_row).collect()
    }

    async fn adjacent_artifacts(
        &self,
        request: &ReconstructionRequest,
        _focus_building: Option<&CivicObject>,
        radius_m: f64,
    ) -> Result<Vec<Artifact>> {
        let mut tx = self.pool.begin().await?;
        let tenant_id = resolve_tenant_id_in_tx(&mut tx, &request.tenant_context.tenant_id).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let rows = sqlx::query(
            r#"
            WITH focus AS (
              SELECT geom
              FROM parcels
              WHERE tenant_id = $1 AND (id::text = $2 OR parcel_key = $2)
              LIMIT 1
            )
            SELECT a.id, a.artifact_key, a.source_type, a.title,
                   COALESCE(a.uri, '') AS uri,
                   COALESCE(a.citation, '') AS citation,
                   a.captured_at_ms, a.payload_jsonb,
                   aa.t_start_ms, aa.t_end_ms
            FROM focus
            INNER JOIN artifact_anchors aa
              ON aa.tenant_id = $1
             AND aa.geom IS NOT NULL
             AND ST_DWithin(aa.geom::geography, focus.geom::geography, $3)
            INNER JOIN artifacts a
              ON a.tenant_id = aa.tenant_id AND a.id = aa.artifact_id
            WHERE ($4::bigint IS NULL OR COALESCE(aa.t_end_ms, a.captured_at_ms, $4) >= $4)
              AND ($5::bigint IS NULL OR COALESCE(aa.t_start_ms, a.captured_at_ms, $5) <= $5)
            ORDER BY a.captured_at_ms NULLS LAST, a.title
            LIMIT 256
            "#,
        )
        .bind(tenant_id)
        .bind(&request.parcel_id)
        .bind(radius_m)
        .bind(request.time_slice.start_ms.or(request.time_slice.at_ms))
        .bind(request.time_slice.end_ms.or(request.time_slice.at_ms))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(artifact_from_row).collect()
    }
}

#[async_trait]
impl BlockSubgraphRepository for PostgisRepository {
    async fn block_subgraph(
        &self,
        request: &ReconstructionRequest,
        evidence: &EvidenceBundle,
    ) -> Result<BlockSubgraph> {
        let mut tx = self.pool.begin().await?;
        let tenant_id = resolve_tenant_id_in_tx(&mut tx, &request.tenant_context.tenant_id).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let block_id = evidence
            .focus_building
            .as_ref()
            .and_then(|object| object.attributes.get("block_id").cloned())
            .unwrap_or_else(|| request.parcel_id.clone());
        let building_rows = sqlx::query(
            r#"
            SELECT DISTINCT b.id, b.civic_object_id, b.t_start_ms, b.t_end_ms,
                   ST_AsGeoJSON(b.geom) AS geometry_json, b.properties,
                   rs.spec_jsonb
            FROM reconstruction_specs rs
            INNER JOIN buildings b
              ON b.tenant_id = rs.tenant_id AND b.id = rs.building_id
            WHERE rs.tenant_id = $1
              AND rs.block_id = $2
              AND rs.status = 'approved'
            ORDER BY b.civic_object_id
            "#,
        )
        .bind(tenant_id)
        .bind(&block_id)
        .fetch_all(&mut *tx)
        .await?;

        let mut nodes = Vec::with_capacity(building_rows.len());
        for row in &building_rows {
            let object = civic_object_from_building_row(row, &request.tenant_context.tenant_id);
            let spec_json: Option<Json<Value>> = row.try_get("spec_jsonb").ok();
            nodes.push(GraphNode {
                node_id: object
                    .as_ref()
                    .map(|object| object.id.clone())
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
                node_type: "BuildingPresence".to_string(),
                object,
                direct_spec: spec_json.map(|value| spec_from_json(&value.0)),
                embedding: Vec::new(),
                missing_embedding: true,
                attributes: BTreeMap::new(),
            });
        }

        let focus_node = evidence
            .focus_building
            .as_ref()
            .map(|object| object.id.clone())
            .unwrap_or_else(|| format!("parcel:{}", request.parcel_id));
        let edges = pairwise_same_block_edges(&nodes);
        tx.commit().await?;
        Ok(BlockSubgraph {
            focus_node,
            nodes,
            edges,
        })
    }
}

fn base_spec(request: &ReconstructionRequest, focus: Option<&CivicObject>) -> ReconstructionSpec {
    let time_key = request
        .time_slice
        .at_ms
        .or(request.time_slice.start_ms)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "untimed".to_string());
    let focus_id = focus
        .map(|object| object.id.clone())
        .unwrap_or_else(|| request.parcel_id.clone());
    let mut metadata = HashMap::new();
    metadata.insert(
        "algorithm".to_string(),
        "procedural-reconstruction-v1".to_string(),
    );
    ReconstructionSpec {
        tenant_context: Some(request.tenant_context.clone()),
        spec_id: format!("recon-{}-{time_key}", stable_id_fragment(&focus_id)),
        civic_object_id: focus
            .map(|object| object.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("parcel:{}", request.parcel_id)),
        building_id: focus
            .and_then(|object| object.id.parse::<Uuid>().ok().map(|_| object.id.clone()))
            .unwrap_or_default(),
        parcel_id: request.parcel_id.clone(),
        block_id: focus
            .and_then(|object| object.attributes.get("block_id").cloned())
            .unwrap_or_default(),
        title: focus
            .map(|object| object.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("Reconstruction {}", request.parcel_id)),
        status: ReconstructionSpecStatus::Draft as i32,
        spec_version: 1,
        created_by: request.requested_by.clone(),
        metadata,
        ..Default::default()
    }
}

fn provenance(
    artifact: &Artifact,
    confidence: f64,
    from_gnn_prior: bool,
    note: &str,
) -> PartProvenance {
    PartProvenance {
        sources: vec![artifact.source()],
        part_confidence: confidence,
        from_gnn_prior,
        moderator_notes: note.to_string(),
        coverage_quality: confidence,
        ..Default::default()
    }
}

fn system_provenance(
    id: &str,
    title: &str,
    confidence: f64,
    from_gnn_prior: bool,
) -> PartProvenance {
    PartProvenance {
        sources: vec![ReconstructionSource {
            source_id: id.to_string(),
            source_type: ReconstructionSourceType::Other as i32,
            title: title.to_string(),
            ..Default::default()
        }],
        part_confidence: confidence,
        from_gnn_prior,
        coverage_quality: confidence,
        ..Default::default()
    }
}

fn prior_provenance(model_version: &str, confidence: f64) -> PartProvenance {
    PartProvenance {
        sources: vec![ReconstructionSource {
            source_id: format!("model:{model_version}"),
            source_type: ReconstructionSourceType::ModelPrior as i32,
            title: "Block-coherent prior".to_string(),
            ..Default::default()
        }],
        part_confidence: confidence,
        from_gnn_prior: true,
        coverage_quality: confidence,
        gnn_version: model_version.to_string(),
        ..Default::default()
    }
}

fn sanborn_material(code: &str) -> &'static str {
    match code.to_ascii_lowercase().as_str() {
        "red" | "brick" => "brick",
        "yellow" | "wood" | "wood-frame" => "wood-frame",
        "blue" | "concrete" => "concrete",
        "olive" | "fire-resistive" => "fire-resistive",
        "gray" | "grey" | "stone" => "stone",
        _ => "unknown",
    }
}

fn source_type_from_str(source_type: &str) -> ReconstructionSourceType {
    let normalized = source_type.to_ascii_lowercase();
    if normalized == "gis_feature" || normalized.starts_with("gis_feature:") {
        return ReconstructionSourceType::Survey;
    }
    match normalized.as_str() {
        "photo" | "archival_photo" | "archival-photo" => ReconstructionSourceType::ArchivalPhoto,
        "sanborn_sheet" | "sanborn" | "map" => ReconstructionSourceType::Map,
        "permit" => ReconstructionSourceType::Permit,
        "assessor" | "parcel_record" => ReconstructionSourceType::Survey,
        "survey" => ReconstructionSourceType::Survey,
        "oral_history" | "oral-history" => ReconstructionSourceType::OralHistory,
        _ => ReconstructionSourceType::Other,
    }
}

fn object_matches_time(object: &CivicObject, time: &TimeSlice) -> bool {
    if time.at_ms.is_none() && time.start_ms.is_none() && time.end_ms.is_none() {
        return true;
    }
    if let Some(at_ms) = time.at_ms {
        return object
            .time_start_ms
            .map(|start| at_ms >= start)
            .unwrap_or(true)
            && object.time_end_ms.map(|end| at_ms <= end).unwrap_or(true);
    }
    let left_start = object.time_start_ms.unwrap_or(i64::MIN);
    let left_end = object.time_end_ms.unwrap_or(i64::MAX);
    let right_start = time.start_ms.unwrap_or(i64::MIN);
    let right_end = time.end_ms.unwrap_or(i64::MAX);
    left_start <= right_end && right_start <= left_end
}

fn normalized_embedding(mut embedding: Vec<f32>) -> Vec<f32> {
    if embedding.len() < SPACETIME_EMBEDDING_DIMS {
        embedding.resize(SPACETIME_EMBEDDING_DIMS, 0.0);
    }
    embedding.truncate(SPACETIME_EMBEDDING_DIMS);
    embedding
}

fn merge_mass(direct: &Mass, prior: Option<&Mass>, model_version: &str) -> Mass {
    let mut merged = prior.cloned().unwrap_or_else(|| Mass {
        provenance: Some(prior_provenance(model_version, 0.25)),
        ..Default::default()
    });
    if !direct.form.is_empty() {
        merged.form = direct.form.clone();
    }
    if direct.stories > 0 {
        merged.stories = direct.stories;
    }
    if direct.height.is_some() {
        merged.height = direct.height.clone();
    }
    if direct.width.is_some() {
        merged.width = direct.width.clone();
    }
    if direct.depth.is_some() {
        merged.depth = direct.depth.clone();
    }
    if direct.provenance.is_some() {
        merged.provenance = direct.provenance.clone();
    }
    merged.attributes.extend(direct.attributes.clone());
    merged
}

fn merge_facades(direct: &[Facade], prior: &[Facade]) -> Vec<Facade> {
    direct
        .iter()
        .enumerate()
        .map(|(index, direct_facade)| {
            let mut merged = prior.get(index).cloned().unwrap_or_default();
            if !direct_facade.facade_side.is_empty() {
                merged.facade_side = direct_facade.facade_side.clone();
            }
            if !direct_facade.primary_material.is_empty() {
                merged.primary_material = direct_facade.primary_material.clone();
            }
            if !direct_facade.color.is_empty() {
                merged.color = direct_facade.color.clone();
            }
            if !direct_facade.opening_grids.is_empty() {
                merged.opening_grids = direct_facade.opening_grids.clone();
            }
            if direct_facade.provenance.is_some() {
                merged.provenance = direct_facade.provenance.clone();
            }
            merged.attributes.extend(direct_facade.attributes.clone());
            merged
        })
        .collect()
}

fn merge_roof(direct: &Roof, prior: Option<&Roof>) -> Roof {
    let mut merged = prior.cloned().unwrap_or_default();
    if !direct.roof_type.is_empty() {
        merged.roof_type = direct.roof_type.clone();
    }
    if !direct.roof_material.is_empty() {
        merged.roof_material = direct.roof_material.clone();
    }
    if direct.pitch_degrees.is_some() {
        merged.pitch_degrees = direct.pitch_degrees;
    }
    if direct.provenance.is_some() {
        merged.provenance = direct.provenance.clone();
    }
    merged.attributes.extend(direct.attributes.clone());
    merged
}

fn merge_ground_floor(direct: &GroundFloor, prior: Option<&GroundFloor>) -> GroundFloor {
    let mut merged = prior.cloned().unwrap_or_default();
    if !direct.use_type.is_empty() {
        merged.use_type = direct.use_type.clone();
    }
    if !direct.storefront_type.is_empty() {
        merged.storefront_type = direct.storefront_type.clone();
    }
    if !direct.entry_location.is_empty() {
        merged.entry_location = direct.entry_location.clone();
    }
    if direct.has_canopy {
        merged.has_canopy = true;
    }
    if direct.provenance.is_some() {
        merged.provenance = direct.provenance.clone();
    }
    merged.attributes.extend(direct.attributes.clone());
    merged
}

fn detect_string_conflict(
    conflicts: &mut Vec<MergeConflict>,
    field_path: &str,
    direct: &str,
    prior: &str,
    direct_confidence: f64,
    threshold: f64,
) {
    if direct.is_empty() || prior.is_empty() || direct == prior || direct_confidence >= threshold {
        return;
    }
    conflicts.push(MergeConflict {
        field_path: field_path.to_string(),
        direct_value: direct.to_string(),
        prior_value: prior.to_string(),
        direct_confidence,
        threshold,
    });
}

fn detect_u32_conflict(
    conflicts: &mut Vec<MergeConflict>,
    field_path: &str,
    direct: u32,
    prior: u32,
    direct_confidence: f64,
    threshold: f64,
) {
    if direct == 0 || prior == 0 || direct == prior || direct_confidence >= threshold {
        return;
    }
    conflicts.push(MergeConflict {
        field_path: field_path.to_string(),
        direct_value: direct.to_string(),
        prior_value: prior.to_string(),
        direct_confidence,
        threshold,
    });
}

fn confidence(provenance: Option<&PartProvenance>) -> f64 {
    provenance
        .map(|item| item.part_confidence)
        .unwrap_or_default()
}

fn mode_string<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for value in values {
        if !value.is_empty() {
            *counts.entry(value.to_string()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(value, _)| value)
}

fn mode_u32(values: impl IntoIterator<Item = u32>) -> Option<u32> {
    let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
    for value in values {
        if value > 0 {
            *counts.entry(value).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(value, _)| value)
}

fn fidelity_tier(spec: &ReconstructionSpec) -> &'static str {
    let source_count = spec
        .mass
        .as_ref()
        .and_then(|mass| mass.provenance.as_ref())
        .map(|provenance| provenance.sources.len())
        .unwrap_or_default()
        + spec
            .facades
            .iter()
            .filter_map(|facade| facade.provenance.as_ref())
            .map(|provenance| provenance.sources.len())
            .sum::<usize>();
    if source_count >= 8 {
        "tier_5"
    } else if source_count >= 2 {
        "tier_3"
    } else {
        "tier_2"
    }
}

fn pairwise_same_block_edges(nodes: &[GraphNode]) -> Vec<GraphEdge> {
    let mut edges = Vec::new();
    for (left_index, left) in nodes.iter().enumerate() {
        for right in nodes.iter().skip(left_index + 1) {
            edges.push(GraphEdge {
                source: left.node_id.clone(),
                target: right.node_id.clone(),
                edge_type: "SAME_BLOCK_AS".to_string(),
                weight: 1.0,
                distance_m: None,
                time_distance_years: None,
                confidence: None,
                attributes: BTreeMap::new(),
            });
        }
    }
    edges
}

pub fn pairformer_node_features(graph: &EmbeddedBlockSubgraph) -> Vec<PairformerNodeFeature> {
    graph
        .nodes
        .iter()
        .map(|node| PairformerNodeFeature {
            node_id: node.node_id.clone(),
            embedding_dim: node.embedding.len(),
            direct_field_count: node
                .direct_spec
                .as_ref()
                .map(count_spec_fields)
                .unwrap_or_default(),
            missing_embedding: node.missing_embedding,
        })
        .collect()
}

pub fn pairformer_edge_features(graph: &EmbeddedBlockSubgraph) -> Vec<PairformerEdgeFeature> {
    graph
        .edges
        .iter()
        .map(|edge| PairformerEdgeFeature {
            source: edge.source.clone(),
            target: edge.target.clone(),
            relation: edge.edge_type.clone(),
            distance_m: edge.distance_m.unwrap_or_default(),
            time_distance_years: edge.time_distance_years.unwrap_or_default(),
            shared_party_wall: edge
                .attributes
                .get("sharedPartyWall")
                .map(|value| value == "true")
                .unwrap_or(false),
            shared_setback_line: edge
                .attributes
                .get("sharedSetbackLine")
                .map(|value| value == "true")
                .unwrap_or(false),
        })
        .collect()
}

fn publishable_edge_confidences(
    graph: &EmbeddedBlockSubgraph,
    model_version: &str,
) -> Vec<EdgeRelationshipConfidence> {
    graph
        .edges
        .iter()
        .map(|edge| EdgeRelationshipConfidence {
            source: edge.source.clone(),
            target: edge.target.clone(),
            relation: edge.edge_type.clone(),
            confidence: edge
                .confidence
                .unwrap_or_else(|| (0.35 + edge.weight.min(1.0) * 0.4).min(0.95)),
            model_version: model_version.to_string(),
        })
        .collect()
}

fn count_spec_fields(spec: &ReconstructionSpec) -> usize {
    usize::from(spec.mass.is_some())
        + spec.facades.len()
        + spec
            .facades
            .iter()
            .map(|facade| facade.opening_grids.len())
            .sum::<usize>()
        + usize::from(spec.roof.is_some())
        + spec.ornaments.len()
        + usize::from(spec.ground_floor.is_some())
}

fn civic_object_from_building_row(
    row: &sqlx::postgres::PgRow,
    tenant_slug: &str,
) -> Option<CivicObject> {
    let id: Option<Uuid> = row.try_get("id").ok();
    let id = id?;
    let properties: Json<Value> = row.try_get("properties").unwrap_or(Json(json!({})));
    let parcel_properties: Option<Json<Value>> = row.try_get("parcel_properties").ok();
    let mut attributes = json_to_string_map(&properties.0);
    if let Some(parcel_properties) = parcel_properties {
        attributes.extend(json_to_string_map(&parcel_properties.0));
    }
    Some(CivicObject {
        id: id.to_string(),
        tenant_id: tenant_slug.to_string(),
        name: row
            .try_get("civic_object_id")
            .unwrap_or_else(|_| id.to_string()),
        object_type: "BuildingPresence".to_string(),
        geometry_json: row
            .try_get::<Option<String>, _>("geometry_json")
            .ok()
            .flatten()
            .unwrap_or_default(),
        time_start_ms: row.try_get("t_start_ms").unwrap_or(None),
        time_end_ms: row.try_get("t_end_ms").unwrap_or(None),
        confidence: 1.0,
        source_ids: Vec::new(),
        dossier_path: String::new(),
        attributes,
    })
}

fn artifact_from_row(row: &sqlx::postgres::PgRow) -> Result<Artifact> {
    let payload: Json<Value> = row.try_get("payload_jsonb")?;
    let source_type: String = row.try_get("source_type")?;
    Ok(Artifact {
        artifact_id: row.try_get::<Uuid, _>("id")?.to_string(),
        artifact_key: row.try_get("artifact_key")?,
        source_type: source_type.clone(),
        title: row.try_get("title")?,
        uri: row.try_get("uri").unwrap_or_default(),
        citation: row.try_get("citation").unwrap_or_default(),
        captured_at_ms: row.try_get("captured_at_ms").unwrap_or(None),
        fetched_at_ms: None,
        content_hash: string_any(&payload.0, &["contentHash", "content_hash"]),
        decoded: decode_artifact(&source_type, &payload.0),
        metadata: json_to_btree_string_map(payload.0.get("metadata").unwrap_or(&Value::Null)),
    })
}

fn decode_artifact(source_type: &str, payload: &Value) -> DecodedArtifact {
    let lower = source_type.to_ascii_lowercase();
    if lower.contains("sanborn") || lower == "map" {
        return DecodedArtifact::SanbornSheet {
            footprint_wkt: optional_string_any(payload, &["footprintWkt", "footprint_wkt"]),
            story_count: u32_any(payload, &["storyCount", "story_count", "stories"]),
            material_code: optional_string_any(
                payload,
                &[
                    "materialCode",
                    "material_code",
                    "sanbornColor",
                    "sanborn_color",
                ],
            ),
            notation: optional_string_any(payload, &["notation", "symbol"]),
            roof_form: optional_string_any(payload, &["roofForm", "roof_form"]),
        };
    }
    if lower.contains("photo") || lower.contains("image") {
        return DecodedArtifact::Photo {
            visible_facades: payload
                .get("visibleFacades")
                .or_else(|| payload.get("visible_facades"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            story_count: u32_any(payload, &["storyCount", "story_count", "stories"]),
            bay_count: u32_any(payload, &["bayCount", "bay_count"]),
            roof_form: optional_string_any(payload, &["roofForm", "roof_form"]),
            caption_text: optional_string_any(payload, &["captionText", "caption_text", "ocr"]),
            scale_height_m: f64_any(payload, &["scaleHeightM", "scale_height_m"]),
        };
    }
    if lower.contains("directory") {
        return DecodedArtifact::DirectoryEntry {
            business_name: optional_string_any(payload, &["businessName", "business_name"]),
            residents: payload
                .get("residents")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            address: optional_string_any(payload, &["address"]),
            use_type: optional_string_any(payload, &["useType", "use_type"]),
        };
    }
    if lower.contains("gis_feature")
        || lower.contains("assessor")
        || lower.contains("parcel_record")
    {
        let attributes = payload
            .get("attributes")
            .or_else(|| payload.get("properties"))
            .or_else(|| payload.as_object().map(|_| payload))
            .and_then(Value::as_object)
            .map(|items| {
                items
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default();
        return DecodedArtifact::GisFeature {
            footprint_wkt: optional_string_any(payload, &["footprintWkt", "footprint_wkt"]),
            attributes,
            source_layer: optional_string_any(payload, &["sourceLayer", "source_layer", "layer"]),
            capture_date_ms: i64_any(
                payload,
                &[
                    "captureDateMs",
                    "capture_date_ms",
                    "capturedAtMs",
                    "captured_at_ms",
                ],
            ),
        };
    }
    if lower.contains("raster_tile") || lower.contains("wmts") || lower.contains("wms") {
        return DecodedArtifact::RasterTile {
            footprint_wkt: optional_string_any(payload, &["footprintWkt", "footprint_wkt"]),
            raster_uri: optional_string_any(payload, &["rasterUri", "raster_uri", "uri"]),
            band_count: u32_any(payload, &["bandCount", "band_count"]),
            capture_date_ms: i64_any(
                payload,
                &[
                    "captureDateMs",
                    "capture_date_ms",
                    "capturedAtMs",
                    "captured_at_ms",
                ],
            ),
        };
    }
    if let Some(text) = optional_string_any(payload, &["text", "ocrText", "ocr_text"]) {
        return DecodedArtifact::Text { text };
    }
    DecodedArtifact::Unknown {
        raw: payload.clone(),
    }
}

fn spec_from_json(value: &Value) -> ReconstructionSpec {
    ReconstructionSpec {
        spec_id: string_any(value, &["specId", "spec_id"]),
        civic_object_id: string_any(value, &["civicObjectId", "civic_object_id"]),
        building_id: string_any(value, &["buildingId", "building_id"]),
        parcel_id: string_any(value, &["parcelId", "parcel_id"]),
        block_id: string_any(value, &["blockId", "block_id"]),
        title: string_any(value, &["title"]),
        spec_version: u32_any(value, &["specVersion", "spec_version", "version"]).unwrap_or(1),
        mass: value.get("mass").map(|mass| Mass {
            form: string_any(mass, &["form"]),
            stories: u32_any(mass, &["stories", "storyCount", "story_count"]).unwrap_or_default(),
            part_id: string_any(mass, &["partId", "part_id"]),
            footprint_geometry_id: string_any(
                mass,
                &["footprintGeometryId", "footprint_geometry_id"],
            ),
            provenance: mass.get("provenance").map(provenance_from_json),
            ..Default::default()
        }),
        facades: value
            .get("facades")
            .and_then(Value::as_array)
            .map(|facades| {
                facades
                    .iter()
                    .map(|facade| Facade {
                        facade_side: string_any(
                            facade,
                            &["facadeSide", "facade_side", "orientation"],
                        ),
                        primary_material: string_any(
                            facade,
                            &["primaryMaterial", "primary_material", "material"],
                        ),
                        color: string_any(facade, &["color"]),
                        opening_grids: facade
                            .get("openingGrids")
                            .or_else(|| facade.get("opening_grids"))
                            .and_then(Value::as_array)
                            .map(|grids| grids.iter().map(opening_grid_from_json).collect())
                            .unwrap_or_default(),
                        provenance: facade.get("provenance").map(provenance_from_json),
                        part_id: string_any(facade, &["partId", "part_id"]),
                        texture_provenance: facade
                            .get("textureProvenance")
                            .or_else(|| facade.get("texture_provenance"))
                            .map(texture_provenance_from_json),
                        ..Default::default()
                    })
                    .collect()
            })
            .unwrap_or_default(),
        roof: value.get("roof").map(|roof| Roof {
            roof_type: string_any(roof, &["roofType", "roof_type", "form"]),
            roof_material: string_any(roof, &["roofMaterial", "roof_material", "material"]),
            provenance: roof.get("provenance").map(provenance_from_json),
            texture_provenance: roof
                .get("textureProvenance")
                .or_else(|| roof.get("texture_provenance"))
                .map(texture_provenance_from_json),
            ..Default::default()
        }),
        ground_floor: value
            .get("groundFloor")
            .or_else(|| value.get("ground_floor"))
            .map(|ground_floor| GroundFloor {
                use_type: string_any(ground_floor, &["useType", "use_type"]),
                storefront_type: string_any(ground_floor, &["storefrontType", "storefront_type"]),
                entry_location: string_any(ground_floor, &["entryLocation", "entry_location"]),
                has_canopy: ground_floor
                    .get("hasCanopy")
                    .or_else(|| ground_floor.get("has_canopy"))
                    .or_else(|| ground_floor.get("hasAwning"))
                    .or_else(|| ground_floor.get("has_awning"))
                    .and_then(Value::as_bool)
                    .unwrap_or_default(),
                provenance: ground_floor.get("provenance").map(provenance_from_json),
                part_id: string_any(ground_floor, &["partId", "part_id"]),
                texture_provenance: ground_floor
                    .get("textureProvenance")
                    .or_else(|| ground_floor.get("texture_provenance"))
                    .map(texture_provenance_from_json),
                ..Default::default()
            }),
        t_start_ms: i64_any(value, &["tStartMs", "t_start_ms"]),
        t_end_ms: i64_any(value, &["tEndMs", "t_end_ms"]),
        archetype_classification: string_any(
            value,
            &["archetypeClassification", "archetype_classification"],
        ),
        gnn_version: string_any(value, &["gnnVersion", "gnn_version"]),
        published_at_ms: i64_any(value, &["publishedAtMs", "published_at_ms"]),
        license: string_any(value, &["license"]),
        ..Default::default()
    }
}

fn provenance_from_json(value: &Value) -> PartProvenance {
    PartProvenance {
        part_confidence: f64_any(value, &["partConfidence", "part_confidence", "confidence"])
            .unwrap_or_default(),
        from_gnn_prior: value
            .get("fromGnnPrior")
            .or_else(|| value.get("from_gnn_prior"))
            .and_then(Value::as_bool)
            .unwrap_or_default(),
        gnn_version: string_any(value, &["gnnVersion", "gnn_version"]),
        moderator_notes: string_any(
            value,
            &[
                "moderatorNotes",
                "moderator_notes",
                "reviewerNote",
                "reviewer_note",
            ],
        ),
        coverage_quality: f64_any(value, &["coverageQuality", "coverage_quality"])
            .unwrap_or_default(),
        per_source_confidences: value
            .get("perSourceConfidences")
            .or_else(|| value.get("per_source_confidences"))
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default(),
        moderator_overridden: value
            .get("moderatorOverridden")
            .or_else(|| value.get("moderator_overridden"))
            .and_then(Value::as_bool)
            .unwrap_or_default(),
        moderator_overridden_at_ms: i64_any(
            value,
            &["moderatorOverriddenAtMs", "moderator_overridden_at_ms"],
        ),
        has_source_conflict: value
            .get("hasSourceConflict")
            .or_else(|| value.get("has_source_conflict"))
            .and_then(Value::as_bool)
            .unwrap_or_default(),
        correction: value.get("correction").map(correction_from_json),
        ..Default::default()
    }
}

fn opening_grid_from_json(value: &Value) -> OpeningGrid {
    OpeningGrid {
        bay_count: u32_any(value, &["bayCount", "bay_count"]).unwrap_or_default(),
        floor_count: u32_any(value, &["floorCount", "floor_count"]).unwrap_or_default(),
        window_pattern: string_any(
            value,
            &[
                "windowPattern",
                "window_pattern",
                "rhythm",
                "openingType",
                "opening_type",
            ],
        ),
        provenance: value.get("provenance").map(provenance_from_json),
        opening_overrides: value
            .get("openingOverrides")
            .or_else(|| value.get("opening_overrides"))
            .and_then(Value::as_array)
            .map(|items| items.iter().map(opening_override_from_json).collect())
            .unwrap_or_default(),
        attributes: value
            .get("attributes")
            .map(json_to_string_map)
            .unwrap_or_default(),
        part_id: string_any(value, &["partId", "part_id"]),
        has_storefront_ground: value
            .get("hasStorefrontGround")
            .or_else(|| value.get("has_storefront_ground"))
            .and_then(Value::as_bool)
            .unwrap_or_default(),
    }
}

fn opening_override_from_json(value: &Value) -> OpeningOverride {
    OpeningOverride {
        bay_index: u32_any(value, &["bayIndex", "bay_index"]).unwrap_or_default(),
        override_kind: string_any(value, &["overrideKind", "override_kind"]),
        override_pattern: string_any(value, &["overridePattern", "override_pattern"]),
        override_provenance: value
            .get("overrideProvenance")
            .or_else(|| value.get("override_provenance"))
            .map(provenance_from_json),
    }
}

fn correction_from_json(value: &Value) -> ProvenanceCorrection {
    ProvenanceCorrection {
        correction_id: string_any(value, &["correctionId", "correction_id"]),
        correction_type: string_any(value, &["correctionType", "correction_type"]),
        correction_reasoning: string_any(value, &["correctionReasoning", "correction_reasoning"]),
        correction_approved_at_ms: i64_any(
            value,
            &["correctionApprovedAtMs", "correction_approved_at_ms"],
        ),
    }
}

fn texture_provenance_from_json(value: &Value) -> TextureProvenance {
    TextureProvenance {
        texture_source: string_any(value, &["textureSource", "texture_source"]),
        lora_archetype: string_any(value, &["loraArchetype", "lora_archetype"]),
        lora_weight: f64_any(value, &["loraWeight", "lora_weight"]),
        controlnet_conditioning_source: string_any(
            value,
            &[
                "controlnetConditioningSource",
                "controlnet_conditioning_source",
            ],
        ),
        texture_confidence: f64_any(value, &["textureConfidence", "texture_confidence"]),
    }
}

fn json_to_string_map(value: &Value) -> HashMap<String, String> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|text| (key.clone(), text.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn json_to_btree_string_map(value: &Value) -> BTreeMap<String, String> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|text| (key.clone(), text.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn optional_string_any(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn attribute_string_any(attributes: &BTreeMap<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| attributes.get(*key))
        .map(value_to_string)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn string_any(value: &Value, keys: &[&str]) -> String {
    optional_string_any(value, keys).unwrap_or_default()
}

fn u32_any(value: &Value, keys: &[&str]) -> Option<u32> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
}

fn attribute_u32_any(attributes: &BTreeMap<String, Value>, keys: &[&str]) -> Option<u32> {
    keys.iter()
        .find_map(|key| attributes.get(*key))
        .and_then(value_to_u32)
}

fn f64_any(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(Value::as_f64)
}

fn i64_any(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(Value::as_i64)
}

fn stable_id_fragment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => String::new(),
    }
}

fn value_to_u32(value: &Value) -> Option<u32> {
    if let Some(number) = value.as_u64() {
        return u32::try_from(number).ok();
    }
    value
        .as_str()
        .and_then(|text| text.trim().parse::<f64>().ok())
        .and_then(|number| u32::try_from(number as i64).ok())
}

fn reconstruction_node_id(spec: &ReconstructionSpec, field_path: &str) -> String {
    format!(
        "reconstruction-node:{}:{}",
        stable_id_fragment(&spec.spec_id),
        stable_id_fragment(field_path)
    )
}

#[allow(clippy::too_many_arguments)]
fn reconstruction_node(
    spec: &ReconstructionSpec,
    id: String,
    node_type: ReconstructionNodeType,
    parent_id: Option<String>,
    field_path: &str,
    label: &str,
    provenance: Option<&PartProvenance>,
    children: Vec<String>,
    data: Value,
    editable: bool,
) -> ReconstructionSceneNode {
    ReconstructionSceneNode {
        id,
        node_type,
        parent_id,
        children,
        visible: true,
        metadata: ReconstructionNodeMetadata {
            spec_id: spec.spec_id.clone(),
            civic_object_id: spec.civic_object_id.clone(),
            building_id: spec.building_id.clone(),
            field_path: field_path.to_string(),
            label: label.to_string(),
            source_ids: source_ids_from_provenance(provenance),
            confidence: provenance.map(|item| item.part_confidence),
            editable,
        },
        data,
    }
}

fn texture_face_node(
    spec: &ReconstructionSpec,
    id: String,
    parent_id: String,
    field_path: &str,
    label: &str,
    target_part: &str,
    texture_provenance: Option<&TextureProvenance>,
    attributes: &HashMap<String, String>,
) -> ReconstructionSceneNode {
    let texture = texture_provenance_projection(texture_provenance, attributes);
    reconstruction_node(
        spec,
        id,
        ReconstructionNodeType::TextureFace,
        Some(parent_id),
        field_path,
        label,
        None,
        Vec::new(),
        json!({
            "targetPart": target_part,
            "textureProvenance": texture,
        }),
        false,
    )
}

fn texture_provenance_projection(
    texture_provenance: Option<&TextureProvenance>,
    attributes: &HashMap<String, String>,
) -> TextureProvenanceProjection {
    let fallback = texture_provenance_from_attributes(attributes);
    let Some(texture) = texture_provenance else {
        return fallback;
    };

    TextureProvenanceProjection {
        texture_source: non_empty_string(&texture.texture_source)
            .unwrap_or(fallback.texture_source),
        lora_archetype: non_empty_string(&texture.lora_archetype).or(fallback.lora_archetype),
        lora_weight: texture.lora_weight.or(fallback.lora_weight),
        conditioning_source_id: non_empty_string(&texture.controlnet_conditioning_source)
            .or(fallback.conditioning_source_id),
        texture_confidence: texture.texture_confidence.or(fallback.texture_confidence),
    }
}

fn texture_provenance_from_attributes(
    attributes: &HashMap<String, String>,
) -> TextureProvenanceProjection {
    TextureProvenanceProjection {
        texture_source: attributes
            .get("texture_source")
            .cloned()
            .unwrap_or_else(|| "untextured".to_string()),
        lora_archetype: attributes.get("lora_archetype").cloned(),
        lora_weight: attributes
            .get("lora_weight")
            .and_then(|value| value.parse::<f64>().ok()),
        conditioning_source_id: attributes
            .get("controlnet_conditioning_source")
            .or_else(|| attributes.get("conditioning_source_id"))
            .cloned(),
        texture_confidence: attributes
            .get("texture_confidence")
            .and_then(|value| value.parse::<f64>().ok()),
    }
}

fn non_empty_string(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn source_ids_from_provenance(provenance: Option<&PartProvenance>) -> Vec<String> {
    provenance
        .map(|item| {
            item.sources
                .iter()
                .map(|source| source.source_id.clone())
                .filter(|source_id| !source_id.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn facade_label(facade: &Facade, index: usize) -> String {
    if facade.facade_side.is_empty() {
        format!("Facade {}", index + 1)
    } else {
        format!("{} facade", facade.facade_side)
    }
}

fn ornament_label(ornament: &Ornament, index: usize) -> String {
    if ornament.ornament_kind.is_empty() {
        format!("Ornament {}", index + 1)
    } else {
        ornament.ornament_kind.clone()
    }
}

async fn set_transaction_tenant(tx: &mut Transaction<'_, Postgres>, tenant_id: Uuid) -> Result<()> {
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn resolve_tenant_id_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    tenant_key: &str,
) -> Result<Uuid> {
    let tenant_id: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id
        FROM tenants
        WHERE slug = $1 OR id::text = $1
        "#,
    )
    .bind(tenant_key)
    .fetch_optional(&mut **tx)
    .await?;
    tenant_id.ok_or_else(|| anyhow!("tenant not found: {tenant_key}"))
}

#[derive(Default)]
pub struct InMemoryRepository {
    pub parcel_history: Vec<CivicObject>,
    pub direct_artifacts: Vec<Artifact>,
    pub adjacent_artifacts: Vec<Artifact>,
    pub graph: Option<BlockSubgraph>,
}

#[async_trait]
impl EvidenceRepository for InMemoryRepository {
    async fn parcel_history(&self, _request: &ReconstructionRequest) -> Result<Vec<CivicObject>> {
        Ok(self.parcel_history.clone())
    }

    async fn direct_artifacts(
        &self,
        _request: &ReconstructionRequest,
        _focus_building: Option<&CivicObject>,
    ) -> Result<Vec<Artifact>> {
        Ok(self.direct_artifacts.clone())
    }

    async fn adjacent_artifacts(
        &self,
        _request: &ReconstructionRequest,
        _focus_building: Option<&CivicObject>,
        _radius_m: f64,
    ) -> Result<Vec<Artifact>> {
        Ok(self.adjacent_artifacts.clone())
    }
}

#[async_trait]
impl BlockSubgraphRepository for InMemoryRepository {
    async fn block_subgraph(
        &self,
        _request: &ReconstructionRequest,
        _evidence: &EvidenceBundle,
    ) -> Result<BlockSubgraph> {
        Ok(self.graph.clone().unwrap_or(BlockSubgraph {
            focus_node: String::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
        }))
    }
}

pub fn attach_manifest_to_spec(spec: &mut ReconstructionSpec, manifest: &AssetManifest) {
    spec.metadata
        .insert("assetManifestId".to_string(), manifest.manifest_id.clone());
    spec.metadata
        .insert("assetManifestStatus".to_string(), manifest.status.clone());
    spec.assets = manifest
        .assets
        .iter()
        .map(|asset| ReconstructionAsset {
            asset_id: asset.asset_id.clone(),
            spec_id: manifest.spec_id.clone(),
            spec_version: manifest.spec_version,
            tenant_id: spec
                .tenant_context
                .as_ref()
                .map(|tenant| tenant.tenant_id.clone())
                .unwrap_or_default(),
            asset_type: asset.asset_type.clone(),
            uri: asset.uri.clone(),
            content_hash: asset.content_hash.clone(),
            metadata: asset
                .metadata
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        })
        .collect();
}

pub fn reconstruction_spec_to_node_tree(spec: &ReconstructionSpec) -> ReconstructionNodeTree {
    let root_id = reconstruction_node_id(spec, "site");
    let building_id = reconstruction_node_id(spec, "building");
    let level_id = reconstruction_node_id(spec, "level[0]");
    let mut nodes = BTreeMap::new();
    let mut level_children = Vec::new();

    nodes.insert(
        root_id.clone(),
        reconstruction_node(
            spec,
            root_id.clone(),
            ReconstructionNodeType::Site,
            None,
            "spec",
            "Reconstruction site",
            None,
            vec![building_id.clone()],
            json!({
                "tenantId": spec
                    .tenant_context
                    .as_ref()
                    .map(|tenant| tenant.tenant_id.as_str())
                    .unwrap_or_default(),
                "parcelId": &spec.parcel_id,
                "blockId": &spec.block_id,
            }),
            false,
        ),
    );

    nodes.insert(
        building_id.clone(),
        reconstruction_node(
            spec,
            building_id.clone(),
            ReconstructionNodeType::Building,
            Some(root_id.clone()),
            "building",
            if spec.title.is_empty() {
                "Building"
            } else {
                spec.title.as_str()
            },
            None,
            vec![level_id.clone()],
            json!({
                "title": &spec.title,
                "status": spec.status,
                "specVersion": spec.spec_version,
                "civicObjectId": &spec.civic_object_id,
                "buildingId": &spec.building_id,
                "parcelId": &spec.parcel_id,
                "blockId": &spec.block_id,
            }),
            true,
        ),
    );

    if let Some(mass) = spec.mass.as_ref() {
        let id = reconstruction_node_id(spec, "mass");
        level_children.push(id.clone());
        nodes.insert(
            id.clone(),
            reconstruction_node(
                spec,
                id,
                ReconstructionNodeType::Mass,
                Some(level_id.clone()),
                "mass",
                "Mass",
                mass.provenance.as_ref(),
                Vec::new(),
                mass_to_json(mass),
                true,
            ),
        );
    }

    for (facade_index, facade) in spec.facades.iter().enumerate() {
        let facade_path = format!("facades[{facade_index}]");
        let facade_id = reconstruction_node_id(spec, &facade_path);
        let texture_path = format!("{facade_path}.texture");
        let texture_id = reconstruction_node_id(spec, &texture_path);
        let mut facade_children = Vec::new();
        let label = facade_label(facade, facade_index);
        level_children.push(facade_id.clone());

        for (grid_index, grid) in facade.opening_grids.iter().enumerate() {
            let grid_path = format!("{facade_path}.openingGrids[{grid_index}]");
            let grid_id = reconstruction_node_id(spec, &grid_path);
            facade_children.push(grid_id.clone());
            nodes.insert(
                grid_id.clone(),
                reconstruction_node(
                    spec,
                    grid_id,
                    ReconstructionNodeType::OpeningGrid,
                    Some(facade_id.clone()),
                    &grid_path,
                    &format!("Opening grid {}", grid_index + 1),
                    grid.provenance.as_ref(),
                    Vec::new(),
                    opening_grid_to_json(grid),
                    true,
                ),
            );
        }

        facade_children.push(texture_id.clone());
        nodes.insert(
            texture_id.clone(),
            texture_face_node(
                spec,
                texture_id,
                facade_id.clone(),
                &texture_path,
                "Facade texture",
                "facade",
                facade.texture_provenance.as_ref(),
                &facade.attributes,
            ),
        );
        nodes.insert(
            facade_id.clone(),
            reconstruction_node(
                spec,
                facade_id,
                ReconstructionNodeType::Facade,
                Some(level_id.clone()),
                &facade_path,
                &label,
                facade.provenance.as_ref(),
                facade_children,
                facade_to_json(facade),
                true,
            ),
        );
    }

    if let Some(ground_floor) = spec.ground_floor.as_ref() {
        let id = reconstruction_node_id(spec, "groundFloor");
        let texture_id = reconstruction_node_id(spec, "groundFloor.texture");
        level_children.push(id.clone());
        nodes.insert(
            texture_id.clone(),
            texture_face_node(
                spec,
                texture_id.clone(),
                id.clone(),
                "groundFloor.texture",
                "Ground-floor texture",
                "groundFloor",
                ground_floor.texture_provenance.as_ref(),
                &ground_floor.attributes,
            ),
        );
        nodes.insert(
            id.clone(),
            reconstruction_node(
                spec,
                id,
                ReconstructionNodeType::GroundFloor,
                Some(level_id.clone()),
                "groundFloor",
                "Ground floor",
                ground_floor.provenance.as_ref(),
                vec![texture_id],
                ground_floor_to_json(ground_floor),
                true,
            ),
        );
    }

    if let Some(roof) = spec.roof.as_ref() {
        let id = reconstruction_node_id(spec, "roof");
        let texture_id = reconstruction_node_id(spec, "roof.texture");
        level_children.push(id.clone());
        nodes.insert(
            texture_id.clone(),
            texture_face_node(
                spec,
                texture_id.clone(),
                id.clone(),
                "roof.texture",
                "Roof texture",
                "roof",
                roof.texture_provenance.as_ref(),
                &roof.attributes,
            ),
        );
        nodes.insert(
            id.clone(),
            reconstruction_node(
                spec,
                id,
                ReconstructionNodeType::Roof,
                Some(level_id.clone()),
                "roof",
                "Roof",
                roof.provenance.as_ref(),
                vec![texture_id],
                roof_to_json(roof),
                true,
            ),
        );
    }

    for (ornament_index, ornament) in spec.ornaments.iter().enumerate() {
        let ornament_path = format!("ornaments[{ornament_index}]");
        let id = reconstruction_node_id(spec, &ornament_path);
        let texture_path = format!("{ornament_path}.texture");
        let texture_id = reconstruction_node_id(spec, &texture_path);
        let label = ornament_label(ornament, ornament_index);
        level_children.push(id.clone());
        nodes.insert(
            texture_id.clone(),
            texture_face_node(
                spec,
                texture_id.clone(),
                id.clone(),
                &texture_path,
                "Ornament texture",
                "ornament",
                ornament.texture_provenance.as_ref(),
                &ornament.attributes,
            ),
        );
        nodes.insert(
            id.clone(),
            reconstruction_node(
                spec,
                id,
                ReconstructionNodeType::Ornament,
                Some(level_id.clone()),
                &ornament_path,
                &label,
                ornament.provenance.as_ref(),
                vec![texture_id],
                ornament_to_json(ornament),
                true,
            ),
        );
    }

    nodes.insert(
        level_id.clone(),
        reconstruction_node(
            spec,
            level_id,
            ReconstructionNodeType::Level,
            Some(building_id),
            "level[0]",
            "Level 0",
            None,
            level_children,
            json!({ "levelIndex": 0, "elevationMeters": 0.0 }),
            false,
        ),
    );

    ReconstructionNodeTree {
        version: 1,
        source: "ReconstructionSpec".to_string(),
        root_node_ids: vec![root_id],
        nodes,
    }
}

pub fn reconstruction_node_path<'a>(
    tree: &'a ReconstructionNodeTree,
    node_id: &str,
) -> Vec<&'a ReconstructionSceneNode> {
    let mut path = Vec::new();
    let mut current = tree.nodes.get(node_id);

    while let Some(node) = current {
        path.push(node);
        current = node
            .parent_id
            .as_ref()
            .and_then(|parent_id| tree.nodes.get(parent_id));
    }

    path.reverse();
    path
}

pub fn reconstruction_spec_to_json(spec: &ReconstructionSpec) -> Value {
    json!({
        "tenantContext": spec.tenant_context.as_ref().map(|tenant| json!({
            "tenantId": &tenant.tenant_id,
            "atlasNodeId": &tenant.atlas_node_id,
            "metadata": &tenant.metadata,
        })),
        "specId": &spec.spec_id,
        "civicObjectId": &spec.civic_object_id,
        "buildingId": &spec.building_id,
        "parcelId": &spec.parcel_id,
        "blockId": &spec.block_id,
        "title": &spec.title,
        "status": spec_status_label(spec.status),
        "specVersion": spec.spec_version,
        "tStartMs": spec.t_start_ms,
        "tEndMs": spec.t_end_ms,
        "archetypeClassification": &spec.archetype_classification,
        "gnnVersion": &spec.gnn_version,
        "publishedAtMs": spec.published_at_ms,
        "license": &spec.license,
        "supersedesSpecId": &spec.supersedes_spec_id,
        "createdBy": &spec.created_by,
        "reviewedBy": &spec.reviewed_by,
        "mass": spec.mass.as_ref().map(mass_to_json),
        "facades": spec.facades.iter().map(facade_to_json).collect::<Vec<_>>(),
        "roof": spec.roof.as_ref().map(roof_to_json),
        "ornaments": spec.ornaments.iter().map(ornament_to_json).collect::<Vec<_>>(),
        "groundFloor": spec.ground_floor.as_ref().map(ground_floor_to_json),
        "assets": spec.assets.iter().map(asset_to_json).collect::<Vec<_>>(),
        "metadata": &spec.metadata,
    })
}

pub fn reconstruction_part_records(spec: &ReconstructionSpec) -> Vec<PartRecord> {
    let mut parts = Vec::new();
    if let Some(mass) = spec.mass.as_ref() {
        parts.push(part_record(
            "mass",
            "Mass",
            mass.provenance.as_ref(),
            mass_to_json(mass),
        ));
    }
    for (index, facade) in spec.facades.iter().enumerate() {
        parts.push(part_record(
            &format!("facade:{index}"),
            "Facade",
            facade.provenance.as_ref(),
            facade_to_json(facade),
        ));
        for (grid_index, grid) in facade.opening_grids.iter().enumerate() {
            parts.push(part_record(
                &format!("facade:{index}:opening_grid:{grid_index}"),
                "OpeningGrid",
                grid.provenance.as_ref(),
                opening_grid_to_json(grid),
            ));
        }
    }
    if let Some(roof) = spec.roof.as_ref() {
        parts.push(part_record(
            "roof",
            "Roof",
            roof.provenance.as_ref(),
            roof_to_json(roof),
        ));
    }
    for (index, ornament) in spec.ornaments.iter().enumerate() {
        let suffix = if ornament.ornament_id.is_empty() {
            index.to_string()
        } else {
            ornament.ornament_id.clone()
        };
        parts.push(part_record(
            &format!("ornament:{suffix}"),
            "Ornament",
            ornament.provenance.as_ref(),
            ornament_to_json(ornament),
        ));
    }
    if let Some(ground_floor) = spec.ground_floor.as_ref() {
        parts.push(part_record(
            "ground_floor",
            "GroundFloor",
            ground_floor.provenance.as_ref(),
            ground_floor_to_json(ground_floor),
        ));
    }
    parts
}

pub fn merge_artifact_sets(direct: &[Artifact], adjacent: &[Artifact]) -> Vec<Artifact> {
    let mut seen = HashSet::new();
    direct
        .iter()
        .chain(adjacent.iter())
        .filter(|artifact| seen.insert(artifact.artifact_id.clone()))
        .cloned()
        .collect()
}

fn part_record(
    key: &str,
    part_type: &str,
    provenance: Option<&PartProvenance>,
    payload: Value,
) -> PartRecord {
    PartRecord {
        key: key.to_string(),
        part_type: part_type.to_string(),
        payload,
        confidence: confidence(provenance),
        source_ids: provenance
            .map(|item| {
                item.sources
                    .iter()
                    .map(|source| source.source_id.clone())
                    .filter(|source_id| !source_id.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn mass_to_json(mass: &Mass) -> Value {
    json!({
        "provenance": mass.provenance.as_ref().map(provenance_to_json),
        "form": &mass.form,
        "stories": mass.stories,
        "height": mass.height.as_ref().map(dimension_to_json),
        "width": mass.width.as_ref().map(dimension_to_json),
        "depth": mass.depth.as_ref().map(dimension_to_json),
        "partId": &mass.part_id,
        "footprintGeometryId": &mass.footprint_geometry_id,
        "attributes": &mass.attributes,
    })
}

fn facade_to_json(facade: &Facade) -> Value {
    json!({
        "provenance": facade.provenance.as_ref().map(provenance_to_json),
        "facadeSide": &facade.facade_side,
        "primaryMaterial": &facade.primary_material,
        "color": &facade.color,
        "openingGrids": facade.opening_grids.iter().map(opening_grid_to_json).collect::<Vec<_>>(),
        "partId": &facade.part_id,
        "textureProvenance": facade.texture_provenance.as_ref().map(texture_provenance_to_json),
        "attributes": &facade.attributes,
    })
}

fn opening_grid_to_json(grid: &OpeningGrid) -> Value {
    json!({
        "provenance": grid.provenance.as_ref().map(provenance_to_json),
        "bayCount": grid.bay_count,
        "floorCount": grid.floor_count,
        "windowPattern": &grid.window_pattern,
        "openingOverrides": grid.opening_overrides.iter().map(opening_override_to_json).collect::<Vec<_>>(),
        "partId": &grid.part_id,
        "hasStorefrontGround": grid.has_storefront_ground,
        "attributes": &grid.attributes,
    })
}

fn opening_override_to_json(opening_override: &OpeningOverride) -> Value {
    json!({
        "bayIndex": opening_override.bay_index,
        "overrideKind": &opening_override.override_kind,
        "overridePattern": &opening_override.override_pattern,
        "overrideProvenance": opening_override.override_provenance.as_ref().map(provenance_to_json),
    })
}

fn roof_to_json(roof: &Roof) -> Value {
    json!({
        "provenance": roof.provenance.as_ref().map(provenance_to_json),
        "roofType": &roof.roof_type,
        "roofMaterial": &roof.roof_material,
        "pitchDegrees": roof.pitch_degrees,
        "textureProvenance": roof.texture_provenance.as_ref().map(texture_provenance_to_json),
        "attributes": &roof.attributes,
    })
}

fn ornament_to_json(ornament: &Ornament) -> Value {
    json!({
        "provenance": ornament.provenance.as_ref().map(provenance_to_json),
        "ornamentId": &ornament.ornament_id,
        "ornamentKind": &ornament.ornament_kind,
        "location": &ornament.location,
        "ornamentMaterial": &ornament.ornament_material,
        "ornamentStyle": &ornament.ornament_style,
        "textureProvenance": ornament.texture_provenance.as_ref().map(texture_provenance_to_json),
        "attributes": &ornament.attributes,
    })
}

fn ground_floor_to_json(ground_floor: &GroundFloor) -> Value {
    json!({
        "provenance": ground_floor.provenance.as_ref().map(provenance_to_json),
        "useType": &ground_floor.use_type,
        "storefrontType": &ground_floor.storefront_type,
        "entryLocation": &ground_floor.entry_location,
        "hasCanopy": ground_floor.has_canopy,
        "partId": &ground_floor.part_id,
        "textureProvenance": ground_floor.texture_provenance.as_ref().map(texture_provenance_to_json),
        "attributes": &ground_floor.attributes,
    })
}

fn asset_to_json(asset: &ReconstructionAsset) -> Value {
    json!({
        "assetId": &asset.asset_id,
        "specId": &asset.spec_id,
        "specVersion": asset.spec_version,
        "tenantId": &asset.tenant_id,
        "assetType": &asset.asset_type,
        "uri": &asset.uri,
        "contentHash": &asset.content_hash,
        "metadata": &asset.metadata,
    })
}

fn provenance_to_json(provenance: &PartProvenance) -> Value {
    json!({
        "sources": provenance.sources.iter().map(|source| json!({
            "sourceId": &source.source_id,
            "sourceType": source.source_type,
            "title": &source.title,
            "uri": &source.uri,
            "capturedAtMs": source.captured_at_ms,
            "citation": &source.citation,
            "metadata": &source.metadata,
        })).collect::<Vec<_>>(),
        "partConfidence": provenance.part_confidence,
        "fromGnnPrior": provenance.from_gnn_prior,
        "moderatorNotes": &provenance.moderator_notes,
        "coverageQuality": provenance.coverage_quality,
        "gnnVersion": &provenance.gnn_version,
        "perSourceConfidences": &provenance.per_source_confidences,
        "moderatorOverridden": provenance.moderator_overridden,
        "moderatorOverriddenAtMs": provenance.moderator_overridden_at_ms,
        "hasSourceConflict": provenance.has_source_conflict,
        "correction": provenance.correction.as_ref().map(correction_to_json),
    })
}

fn correction_to_json(correction: &ProvenanceCorrection) -> Value {
    json!({
        "correctionId": &correction.correction_id,
        "correctionType": &correction.correction_type,
        "correctionReasoning": &correction.correction_reasoning,
        "correctionApprovedAtMs": correction.correction_approved_at_ms,
    })
}

fn texture_provenance_to_json(texture: &TextureProvenance) -> Value {
    json!({
        "textureSource": &texture.texture_source,
        "loraArchetype": &texture.lora_archetype,
        "loraWeight": texture.lora_weight,
        "controlnetConditioningSource": &texture.controlnet_conditioning_source,
        "textureConfidence": texture.texture_confidence,
    })
}

fn dimension_to_json(dimension: &DimensionRange) -> Value {
    json!({
        "min": dimension.min,
        "max": dimension.max,
        "unit": &dimension.unit,
    })
}

fn spec_status_label(status: i32) -> &'static str {
    match ReconstructionSpecStatus::try_from(status).ok() {
        Some(ReconstructionSpecStatus::InReview) => "in_review",
        Some(ReconstructionSpecStatus::Approved) => "approved",
        Some(ReconstructionSpecStatus::Superseded) => "superseded",
        Some(ReconstructionSpecStatus::Rejected) => "rejected",
        _ => "draft",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ReconstructionRequest {
        ReconstructionRequest {
            tenant_context: TenantContext {
                tenant_id: "flint".to_string(),
                atlas_node_id: "atlas:flint".to_string(),
                metadata: Default::default(),
            },
            parcel_id: "parcel-1".to_string(),
            time_slice: TimeSlice {
                at_ms: Some(1_735_689_600_000),
                start_ms: None,
                end_ms: None,
            },
            requested_by: "test".to_string(),
            auto_approve: false,
        }
    }

    fn sanborn_artifact() -> Artifact {
        Artifact {
            artifact_id: "artifact:sanborn:1".to_string(),
            artifact_key: "sanborn-1".to_string(),
            source_type: "sanborn_sheet".to_string(),
            title: "Sanborn block sheet".to_string(),
            uri: "https://example.test/sanborn".to_string(),
            citation: "Test Sanborn".to_string(),
            captured_at_ms: Some(1),
            fetched_at_ms: None,
            content_hash: "hash".to_string(),
            decoded: DecodedArtifact::SanbornSheet {
                footprint_wkt: None,
                story_count: Some(2),
                material_code: Some("red".to_string()),
                notation: Some("store".to_string()),
                roof_form: Some("flat".to_string()),
            },
            metadata: BTreeMap::new(),
        }
    }

    fn photo_artifact_without_mass_signal() -> Artifact {
        Artifact {
            artifact_id: "artifact:photo:1".to_string(),
            artifact_key: "photo-1".to_string(),
            source_type: "archival_photo".to_string(),
            title: "Undated storefront photo".to_string(),
            uri: "https://example.test/photo".to_string(),
            citation: "Test Photo".to_string(),
            captured_at_ms: Some(1),
            fetched_at_ms: None,
            content_hash: "hash-photo".to_string(),
            decoded: DecodedArtifact::Photo {
                visible_facades: vec!["primary".to_string()],
                story_count: None,
                bay_count: Some(4),
                roof_form: None,
                caption_text: None,
                scale_height_m: None,
            },
            metadata: BTreeMap::new(),
        }
    }

    fn gis_feature_artifact() -> Artifact {
        Artifact {
            artifact_id: "artifact:gis:1".to_string(),
            artifact_key: "gis-1".to_string(),
            source_type: "gis_feature".to_string(),
            title: "City parcel record".to_string(),
            uri: "https://example.test/parcel".to_string(),
            citation: "Test GIS feature".to_string(),
            captured_at_ms: Some(1),
            fetched_at_ms: None,
            content_hash: "hash-gis".to_string(),
            decoded: DecodedArtifact::GisFeature {
                footprint_wkt: Some("POLYGON ((0 0, 0 1, 1 1, 0 0))".to_string()),
                attributes: BTreeMap::from([
                    ("Cib_Storie".to_string(), json!("3")),
                    ("Use_Type".to_string(), json!("commercial")),
                    ("building_material".to_string(), json!("brick")),
                ]),
                source_layer: Some("Main_COF_Parcel".to_string()),
                capture_date_ms: Some(1_735_689_600_000),
            },
            metadata: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn full_pipeline_prefers_direct_fields_and_generates_manifest() {
        let repo = InMemoryRepository {
            parcel_history: vec![CivicObject {
                id: Uuid::nil().to_string(),
                tenant_id: "flint".to_string(),
                name: "building:ct-1".to_string(),
                object_type: "BuildingPresence".to_string(),
                geometry_json: "{}".to_string(),
                time_start_ms: Some(0),
                time_end_ms: None,
                confidence: 1.0,
                source_ids: Vec::new(),
                dossier_path: String::new(),
                attributes: HashMap::from([(
                    "block_id".to_string(),
                    "block:carriage-town".to_string(),
                )]),
            }],
            direct_artifacts: vec![sanborn_artifact()],
            ..Default::default()
        };
        let output = run_full_pipeline(
            request(),
            &repo,
            &ZeroEmbeddingProvider::default(),
            &BlockCoherentPriorModel::default(),
            &SceneFoundryManifestGenerator::default(),
        )
        .await
        .expect("pipeline runs");

        let spec = output.merged.spec;
        assert_eq!(spec.mass.unwrap().stories, 2);
        assert_eq!(spec.facades[0].primary_material, "brick");
        assert_eq!(spec.roof.unwrap().roof_type, "flat");
        assert_eq!(output.asset_manifest.status, "queued");
        assert!(output
            .embedded_subgraph
            .nodes
            .iter()
            .any(|node| node.node_id == Uuid::nil().to_string()));
    }

    #[tokio::test]
    async fn building_domain_pipeline_matches_legacy_wrapper() {
        let repo = InMemoryRepository {
            parcel_history: vec![CivicObject {
                id: Uuid::nil().to_string(),
                tenant_id: "flint".to_string(),
                name: "building:ct-1".to_string(),
                object_type: "BuildingPresence".to_string(),
                geometry_json: "{}".to_string(),
                time_start_ms: Some(0),
                time_end_ms: None,
                confidence: 1.0,
                source_ids: Vec::new(),
                dossier_path: String::new(),
                attributes: HashMap::from([(
                    "block_id".to_string(),
                    "block:carriage-town".to_string(),
                )]),
            }],
            direct_artifacts: vec![sanborn_artifact()],
            ..Default::default()
        };

        let wrapper_output = run_full_pipeline(
            request(),
            &repo,
            &ZeroEmbeddingProvider::default(),
            &BlockCoherentPriorModel::default(),
            &SceneFoundryManifestGenerator::default(),
        )
        .await
        .expect("legacy wrapper runs");
        let domain_output = run_domain_pipeline::<BuildingDomain, _, _, _, _>(
            request(),
            &repo,
            &ZeroEmbeddingProvider::default(),
            &BlockCoherentPriorModel::default(),
            &SceneFoundryManifestGenerator::default(),
        )
        .await
        .expect("building domain pipeline runs");

        assert_eq!(domain_output, wrapper_output);
    }

    #[test]
    fn photo_without_mass_signal_does_not_block_later_sanborn_story_count() {
        let evidence = EvidenceBundle {
            focus_building: None,
            direct: vec![photo_artifact_without_mass_signal(), sanborn_artifact()],
            adjacent: Vec::new(),
            temporal_predecessor: None,
            temporal_successor: None,
        };

        let direct = extract_direct(&request(), &evidence).expect("direct extraction succeeds");

        assert_eq!(direct.spec.mass.unwrap().stories, 2);
        assert!(direct
            .populated_fields
            .iter()
            .any(|field| field == "mass.stories"));
    }

    #[test]
    fn gis_feature_replaces_footprint_fallback_with_high_confidence_fields() {
        let evidence = EvidenceBundle {
            focus_building: Some(CivicObject {
                id: Uuid::nil().to_string(),
                tenant_id: "flint".to_string(),
                name: "building:ct-1".to_string(),
                object_type: "BuildingPresence".to_string(),
                geometry_json: "{\"type\":\"Polygon\"}".to_string(),
                time_start_ms: Some(0),
                time_end_ms: None,
                confidence: 1.0,
                source_ids: Vec::new(),
                dossier_path: String::new(),
                attributes: HashMap::new(),
            }),
            direct: vec![gis_feature_artifact()],
            adjacent: Vec::new(),
            temporal_predecessor: None,
            temporal_successor: None,
        };

        let direct = extract_direct(&request(), &evidence).expect("direct extraction succeeds");
        let mass = direct.spec.mass.expect("GIS feature should populate mass");

        assert_eq!(mass.form, "parcel polygon");
        assert_eq!(mass.stories, 3);
        assert_eq!(mass.provenance.unwrap().part_confidence, 0.95);
        assert_eq!(direct.spec.facades[0].primary_material, "brick");
        assert_eq!(direct.spec.ground_floor.unwrap().use_type, "commercial");
        assert!(direct
            .populated_fields
            .iter()
            .any(|field| field == "mass.form"));
    }

    #[test]
    fn source_type_from_str_accepts_gis_feature_prefix() {
        assert_eq!(
            source_type_from_str("gis_feature:ogc_api_features"),
            ReconstructionSourceType::Survey
        );
    }

    #[test]
    fn stored_neighbor_specs_preserve_opening_grids() {
        let spec = spec_from_json(&json!({
            "facades": [{
                "orientation": "primary",
                "material": "brick",
                "openingGrids": [{
                    "bayCount": 5,
                    "floorCount": 2,
                    "rhythm": "regular",
                    "openingType": "window"
                }]
            }]
        }));

        assert_eq!(spec.facades.len(), 1);
        assert_eq!(spec.facades[0].opening_grids.len(), 1);
        assert_eq!(spec.facades[0].opening_grids[0].bay_count, 5);
    }

    #[test]
    fn merge_flags_low_confidence_direct_conflict() {
        let mut direct_spec = base_spec(&request(), None);
        direct_spec.mass = Some(Mass {
            stories: 1,
            provenance: Some(system_provenance("manual", "Manual value", 0.4, false)),
            ..Default::default()
        });
        let mut prior_spec = direct_spec.clone();
        prior_spec.mass = Some(Mass {
            stories: 3,
            provenance: Some(prior_provenance("test-model", 0.8)),
            ..Default::default()
        });

        let merged = merge_evidence_prior(
            &DirectExtraction {
                spec: direct_spec,
                populated_fields: vec!["mass.stories".to_string()],
            },
            &PriorReconstructionSpec {
                spec: prior_spec,
                model_version: "test-model".to_string(),
                edge_confidences: Vec::new(),
            },
            MergeConfig::default(),
        )
        .expect("merge succeeds");

        assert_eq!(merged.spec.mass.unwrap().stories, 1);
        assert_eq!(merged.conflicts.len(), 1);
        assert_eq!(merged.conflicts[0].field_path, "mass.stories");
    }

    #[test]
    fn manifest_attaches_assets_to_spec() {
        let mut spec = base_spec(&request(), None);
        let manifest = AssetManifest {
            manifest_id: "manifest:1".to_string(),
            spec_id: spec.spec_id.clone(),
            spec_version: 1,
            fidelity_tier: "tier_2".to_string(),
            generator: "scene_foundry".to_string(),
            status: "queued".to_string(),
            assets: vec![GeneratedAsset {
                asset_id: "asset:1".to_string(),
                asset_type: "scene_foundry_manifest".to_string(),
                uri: "scene-foundry://queued/spec/v1/manifest.json".to_string(),
                content_hash: String::new(),
                metadata: BTreeMap::new(),
            }],
            metadata: BTreeMap::new(),
        };
        attach_manifest_to_spec(&mut spec, &manifest);

        assert_eq!(spec.assets.len(), 1);
        assert_eq!(spec.metadata["assetManifestStatus"], "queued");
    }

    #[test]
    fn reconstruction_spec_projects_to_pascal_style_node_tree() {
        let mut spec = base_spec(&request(), None);
        spec.spec_id = "spec:carriage-town:worker-cottage".to_string();
        spec.title = "Worker's Cottage".to_string();
        spec.mass = Some(Mass {
            stories: 1,
            provenance: Some(system_provenance("sanborn", "Sanborn", 0.86, false)),
            ..Default::default()
        });
        spec.facades.push(Facade {
            facade_side: "primary".to_string(),
            primary_material: "wood".to_string(),
            provenance: Some(system_provenance("photo", "Photo", 0.72, false)),
            opening_grids: vec![OpeningGrid {
                bay_count: 3,
                floor_count: 1,
                window_pattern: "regular".to_string(),
                provenance: Some(system_provenance("photo", "Photo", 0.61, false)),
                ..Default::default()
            }],
            attributes: HashMap::from([
                ("texture_source".to_string(), "lora_only".to_string()),
                (
                    "lora_archetype".to_string(),
                    "wood_frame_house_with_porch".to_string(),
                ),
                ("lora_weight".to_string(), "0.85".to_string()),
                ("texture_confidence".to_string(), "0.58".to_string()),
            ]),
            texture_provenance: Some(TextureProvenance {
                texture_source: "archival_photo".to_string(),
                lora_archetype: "brick_storefront".to_string(),
                lora_weight: Some(0.72),
                controlnet_conditioning_source: "photo".to_string(),
                texture_confidence: Some(0.81),
            }),
            ..Default::default()
        });
        spec.roof = Some(Roof {
            roof_type: "gable".to_string(),
            provenance: Some(prior_provenance("civic-pairformer/test", 0.49)),
            ..Default::default()
        });

        let tree = reconstruction_spec_to_node_tree(&spec);
        let facade_id = reconstruction_node_id(&spec, "facades[0]");
        let opening_id = reconstruction_node_id(&spec, "facades[0].openingGrids[0]");
        let texture_id = reconstruction_node_id(&spec, "facades[0].texture");

        assert_eq!(tree.root_node_ids.len(), 1);
        assert_eq!(
            tree.nodes[&facade_id].node_type,
            ReconstructionNodeType::Facade
        );
        assert_eq!(
            tree.nodes[&opening_id].node_type,
            ReconstructionNodeType::OpeningGrid
        );
        assert_eq!(
            tree.nodes[&facade_id].children,
            vec![opening_id.clone(), texture_id.clone()]
        );
        assert_eq!(tree.nodes[&facade_id].metadata.source_ids, vec!["photo"]);
        assert_eq!(tree.nodes[&facade_id].metadata.confidence, Some(0.72));
        assert_eq!(
            tree.nodes[&texture_id].data["textureProvenance"]["textureSource"],
            "archival_photo"
        );
        assert_eq!(
            tree.nodes[&texture_id].data["textureProvenance"]["loraArchetype"],
            "brick_storefront"
        );

        let path = reconstruction_node_path(&tree, &opening_id)
            .iter()
            .map(|node| node.node_type.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            path,
            vec![
                ReconstructionNodeType::Site,
                ReconstructionNodeType::Building,
                ReconstructionNodeType::Level,
                ReconstructionNodeType::Facade,
                ReconstructionNodeType::OpeningGrid,
            ]
        );
    }
}
