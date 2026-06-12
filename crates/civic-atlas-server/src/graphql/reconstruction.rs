//! GraphQL types and resolvers for the reconstruction surface.
//!
//! Mirrors the sidecar's `reconstructionDossier(reconstructionId)` query
//! (apps/graphql-server/src/schema.ts:1343) and the shape of the
//! returned ReconstructionDossier (line 1067). Runs in-process: the
//! GraphQL resolver calls the existing `ReconstructionGrpcService`
//! directly, reusing the SQL, validation, and tenant logic Codex
//! shipped in the gRPC handler.
//!
//! Field coverage matches the sidecar contract: HistoricalReconstruction
//! with full per-part confidences + footprint + geometry; ReconstructionDossier
//! with evidence + conflicts + blockSubgraph + nodeTree.
//!
//! What still lives in the sidecar (and will port in follow-ups):
//!   - `placeForCivicObject` lookup that pulls a building's actual
//!     PostGIS geometry into position/footprint. This resolver uses
//!     the spec's mass dimension fallbacks until that helper ports.

use std::env;

use async_graphql::{Context, InputObject, Object, SimpleObject};
use chrono::NaiveDate;
use civic_atlas_reconstruction_engine::{
    run_full_pipeline, Artifact as EngineArtifact, BlockCoherentPriorModel,
    EvidenceBundle as EngineEvidenceBundle, GeneratedAsset as EngineGeneratedAsset,
    MergeConflict as EngineMergeConflict, PostgisRepository,
    ReconstructionRequest as EngineReconstructionRequest, ZeroEmbeddingProvider,
};
use civic_atlas_renderer::{
    select_tier, stamp_massing_texture_provenance, LocalDirAssetStore, PgRenderJobQueue,
    SceneFoundryRenderer, TierThresholds,
};
use civic_atlas_types::civic_atlas::v1::{
    reconstruction_service_server::ReconstructionService, GetReconstructionSpecRequest,
    ListReconstructionSpecsRequest, PartProvenance, ReconstructionAsset,
    ReconstructionSource as ProtoSource, ReconstructionSourceType, ReconstructionSpec,
    TenantContext, TimeSlice,
};
use serde_json::json;
use tonic::Request;

use crate::graphql::search::Source;
use crate::reconstruction::ReconstructionGrpcService;
use crate::AtlasState;

// ---------------------------------------------------------------------------
// GraphQL types
// ---------------------------------------------------------------------------

#[derive(SimpleObject, Default, Clone)]
pub struct ReconstructionFootprint {
    pub width_meters: f64,
    pub depth_meters: f64,
}

#[derive(SimpleObject, Default, Clone)]
pub struct HistoricalReconstruction {
    pub id: String,
    pub civic_object_id: String,
    pub name: String,
    pub description: String,
    pub position: Option<async_graphql::Json<[f64; 2]>>,
    pub footprint: ReconstructionFootprint,
    pub height_meters: f64,
    pub bearing_degrees: f64,
    pub confidence: f64,
    pub facade_confidence: Option<f64>,
    pub roof_confidence: Option<f64>,
    pub ground_floor_confidence: Option<f64>,
    pub roof_form: Option<String>,
    pub time_start: Option<String>,
    pub time_end: Option<String>,
    pub geometry_url: Option<String>,
    pub geometry_format: Option<String>,
    pub foundry_asset_url: Option<String>,
    pub sources: Vec<Source>,
}

#[derive(SimpleObject, Default, Clone)]
pub struct EvidenceItem {
    pub id: String,
    pub reconstruction_id: String,
    pub source: Source,
    pub evidence_type: String,
    pub target_node_id: Option<String>,
    pub confidence: f64,
    pub thumbnail_url: Option<String>,
    pub summary: Option<String>,
    pub source_date_label: Option<String>,
}

#[derive(SimpleObject, Default, Clone)]
pub struct EvidenceBundle {
    pub reconstruction_id: String,
    pub items: Vec<EvidenceItem>,
    pub total_count: i32,
}

#[derive(SimpleObject, Default, Clone)]
pub struct MergeDisagreement {
    pub source: Source,
    pub stated_value: String,
    pub confidence: f64,
    pub evidence_item_id: String,
}

#[derive(SimpleObject, Default, Clone)]
pub struct MergeConflict {
    pub id: String,
    pub reconstruction_id: String,
    pub target_node_id: String,
    pub field_label: String,
    pub disagreements: Vec<MergeDisagreement>,
    pub resolved_value: String,
    pub resolution_explanation: String,
    pub resolution_threshold: f64,
}

#[derive(SimpleObject, Clone)]
pub struct BlockNeighbor {
    pub reconstruction: HistoricalReconstruction,
    pub relation: String,
    pub strength: f64,
}

#[derive(SimpleObject, Default, Clone)]
pub struct BlockSubgraph {
    pub reconstruction_id: String,
    pub neighbors: Vec<BlockNeighbor>,
}

#[derive(SimpleObject)]
pub struct ReconstructionDossier {
    pub reconstruction: HistoricalReconstruction,
    pub evidence: EvidenceBundle,
    pub conflicts: Vec<MergeConflict>,
    pub block_subgraph: BlockSubgraph,
    pub node_tree: async_graphql::Json<serde_json::Value>,
    pub summary: String,
    pub debug: Option<async_graphql::Json<serde_json::Value>>,
}

#[derive(InputObject)]
pub struct BboxInput {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

// ---------------------------------------------------------------------------
// Helpers shared with civicResearch results parsing.
// ---------------------------------------------------------------------------

/// Mirrors sidecar `normalizeSpecId`. Maps fixture-historical-ids and
/// building:* ids onto canonical spec_id strings.
pub fn normalize_spec_id(reconstruction_id: &str) -> String {
    if reconstruction_id.starts_with("spec:") {
        return reconstruction_id.to_string();
    }
    match reconstruction_id {
        "historical:carriage-town:whaley-house" => return "spec:carriage-town:1".to_string(),
        "historical:carriage-town:628-kearsley" => return "spec:carriage-town:2".to_string(),
        "historical:carriage-town:storefront" => return "spec:carriage-town:3".to_string(),
        "historical:carriage-town:workers-cottage" => return "spec:carriage-town:4".to_string(),
        "historical:carriage-town:stockton-house" => return "spec:carriage-town:5".to_string(),
        _ => {}
    }
    if let Some(suffix) = reconstruction_id.strip_prefix("building:carriage-town:") {
        if suffix.chars().all(|c| c.is_ascii_digit()) {
            return format!("spec:carriage-town:{suffix}");
        }
    }
    reconstruction_id.to_string()
}

fn default_tenant() -> String {
    env::var("CIVIC_ATLAS_DEFAULT_TENANT").unwrap_or_else(|_| "flint".to_string())
}

pub fn ms_to_iso8601(ms: i64) -> String {
    let secs = ms / 1_000;
    let nanos = ((ms % 1_000).abs() as u32) * 1_000_000;
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

/// Mirrors sidecar normalizeTrustTier. Bucket source trust into the
/// three contract values HIGH | MEDIUM | LOW.
pub fn normalize_trust_tier(raw: &str) -> String {
    let upper = raw.to_ascii_uppercase();
    if upper.contains("HIGH") {
        return "HIGH".to_string();
    }
    if upper.contains("LOW") {
        return "LOW".to_string();
    }
    "MEDIUM".to_string()
}

/// Mirrors sidecar normalizeSourceType. Bucket source type into the
/// contract enum values the GraphQL Source.sourceType expects.
pub fn normalize_source_type(raw: &str) -> String {
    let upper = raw.to_ascii_uppercase();
    if upper.contains("PHOTO") {
        return "PHOTO_ARCHIVE".to_string();
    }
    if upper.contains("MAP") || upper.contains("SURVEY") {
        return "HISTORICAL_ARCHIVE".to_string();
    }
    if upper.contains("PERMIT") || upper.contains("PUBLIC") {
        return "PUBLIC_RECORD".to_string();
    }
    if upper.contains("MODEL") {
        return "ACADEMIC".to_string();
    }
    "HISTORICAL_ARCHIVE".to_string()
}

fn source_type_str(value: ReconstructionSourceType) -> &'static str {
    match value {
        ReconstructionSourceType::Unspecified => "OTHER",
        ReconstructionSourceType::ArchivalPhoto => "PHOTO",
        ReconstructionSourceType::Map => "MAP",
        ReconstructionSourceType::Permit => "PERMIT",
        ReconstructionSourceType::Survey => "SURVEY",
        ReconstructionSourceType::OralHistory => "ORAL_HISTORY",
        ReconstructionSourceType::ModelPrior => "MODEL",
        ReconstructionSourceType::Other => "OTHER",
    }
}

fn source_from_proto(proto: &ProtoSource, fallback_id: &str) -> Source {
    let id = if proto.source_id.is_empty() {
        fallback_id.to_string()
    } else {
        proto.source_id.clone()
    };
    let kind = ReconstructionSourceType::try_from(proto.source_type).unwrap_or_default();
    let name = if !proto.title.is_empty() {
        proto.title.clone()
    } else if !proto.citation.is_empty() {
        proto.citation.clone()
    } else {
        id.clone()
    };
    Source {
        id,
        name,
        homepage_url: if proto.uri.is_empty() {
            None
        } else {
            Some(proto.uri.clone())
        },
        source_type: normalize_source_type(source_type_str(kind)),
        public_use_terms: None,
        trust_tier: normalize_trust_tier("MEDIUM"),
        last_checked: proto.captured_at_ms.map(ms_to_iso8601),
        known_limits: Vec::new(),
        contains_personal_data: false,
    }
}

fn provenance_sources(p: Option<&PartProvenance>, fallback_prefix: &str) -> Vec<Source> {
    match p {
        None => Vec::new(),
        Some(prov) => prov
            .sources
            .iter()
            .enumerate()
            .map(|(i, s)| source_from_proto(s, &format!("{fallback_prefix}:source:{i}")))
            .collect(),
    }
}

fn provenance_confidence(p: Option<&PartProvenance>, fallback: f64) -> f64 {
    p.map(|prov| {
        if prov.part_confidence > 0.0 {
            prov.part_confidence
        } else if prov.coverage_quality > 0.0 {
            prov.coverage_quality
        } else {
            fallback
        }
    })
    .unwrap_or(fallback)
}

fn provenance_has_conflict(p: Option<&PartProvenance>) -> bool {
    p.is_some_and(|prov| {
        prov.moderator_overridden
            || prov
                .moderator_notes
                .to_ascii_lowercase()
                .contains("conflict")
    })
}

fn unique_sources(sources: Vec<Source>) -> Vec<Source> {
    let mut seen = std::collections::HashSet::<String>::new();
    let mut out = Vec::with_capacity(sources.len());
    for src in sources {
        if seen.insert(src.id.clone()) {
            out.push(src);
        }
    }
    out
}

fn target_node_id(reconstruction_id: &str, part: &str) -> String {
    format!("reconstruction-node:{reconstruction_id}:{part}")
}

fn year_start_ms(year: i32) -> async_graphql::Result<i64> {
    if !(1700..=2100).contains(&year) {
        return Err(async_graphql::Error::new(
            "year must be between 1700 and 2100",
        ));
    }
    NaiveDate::from_ymd_opt(year, 1, 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date_time| date_time.and_utc().timestamp_millis())
        .ok_or_else(|| async_graphql::Error::new("invalid reconstruction year"))
}

fn roof_form_from(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("hip") {
        return Some("HIPPED".to_string());
    }
    if lower.contains("gable") {
        return Some("GABLE".to_string());
    }
    if lower.contains("flat") {
        return Some("FLAT".to_string());
    }
    if value.is_empty() {
        None
    } else {
        Some(value.to_ascii_uppercase())
    }
}

fn dimension_meters(d: Option<&civic_atlas_types::civic_atlas::v1::DimensionRange>) -> Option<f64> {
    d.and_then(|range| {
        let max = range.max.unwrap_or(0.0);
        let min = range.min.unwrap_or(0.0);
        if max > 0.0 {
            Some(max)
        } else if min > 0.0 {
            Some(min)
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Spec → GraphQL mappers
// ---------------------------------------------------------------------------

pub fn reconstruction_from_spec(spec: &ReconstructionSpec) -> HistoricalReconstruction {
    let mass = spec.mass.as_ref();
    let facade = spec.facades.first();
    let roof = spec.roof.as_ref();
    let ground = spec.ground_floor.as_ref();

    let mass_provenance = mass.and_then(|m| m.provenance.as_ref());
    let facade_provenance = facade.and_then(|f| f.provenance.as_ref());
    let roof_provenance = roof.and_then(|r| r.provenance.as_ref());
    let ground_provenance = ground.and_then(|g| g.provenance.as_ref());

    let mass_confidence = provenance_confidence(mass_provenance, 0.5);

    // Sidecar uses placeForCivicObject + parseGeometryPoints to derive
    // position and footprint from PostGIS. Until that helper ports here,
    // fall back to the spec's mass dimensions for footprint and leave
    // position as None (the frontend uses position only for the atlas
    // map dot, which the atelier surface doesn't need).
    let width = mass
        .and_then(|m| dimension_meters(m.width.as_ref()))
        .unwrap_or(10.0);
    let depth = mass
        .and_then(|m| dimension_meters(m.depth.as_ref()))
        .unwrap_or(12.0);
    let stories_default = mass.map(|m| m.stories).unwrap_or(1) as f64;
    let height = mass
        .and_then(|m| dimension_meters(m.height.as_ref()))
        .unwrap_or((stories_default * 3.0).max(3.0));

    // Walk spec.assets for "scene" asset types. The geometry URL is the
    // first renderable glb/gltf; the foundry asset URL is the provenance
    // record (a scene asset ending .json), falling back to the first scene
    // asset so older specs with a single manifest pointer keep working.
    let scene_assets: Vec<&ReconstructionAsset> = spec
        .assets
        .iter()
        .filter(|a| a.asset_type.to_ascii_lowercase().contains("scene"))
        .filter(|a| !a.uri.is_empty())
        .collect();
    let renderable_geometry_url = scene_assets
        .iter()
        .map(|a| a.uri.clone())
        .find(|uri| {
            let lower = uri.to_ascii_lowercase();
            lower.ends_with(".glb") || lower.ends_with(".gltf")
        });
    let provenance_record_url = scene_assets
        .iter()
        .map(|a| a.uri.clone())
        .find(|uri| uri.to_ascii_lowercase().ends_with(".json"));
    let scene_uri = provenance_record_url.or_else(|| scene_assets.first().map(|a| a.uri.clone()));

    // Description: form · material · roof_type, separated by middle
    // dots. Mirrors sidecar reconstructionFromSpec line 734-740.
    let mut description_parts: Vec<String> = Vec::new();
    if let Some(m) = mass {
        if !m.form.is_empty() {
            description_parts.push(m.form.clone());
        }
    }
    if let Some(f) = facade {
        let material = if !f.primary_material.is_empty() {
            f.primary_material.clone()
        } else {
            String::new()
        };
        if !material.is_empty() {
            description_parts.push(material);
        }
    }
    if let Some(r) = roof {
        let kind = if !r.roof_type.is_empty() {
            r.roof_type.clone()
        } else {
            String::new()
        };
        if !kind.is_empty() {
            description_parts.push(kind);
        }
    }
    let description = if description_parts.is_empty() {
        "Backend ReconstructionSpec.".to_string()
    } else {
        description_parts.join(" · ")
    };

    let sources = unique_sources(
        [
            provenance_sources(mass_provenance, &format!("{}:mass", spec.spec_id)),
            provenance_sources(facade_provenance, &format!("{}:facade", spec.spec_id)),
            provenance_sources(roof_provenance, &format!("{}:roof", spec.spec_id)),
            provenance_sources(ground_provenance, &format!("{}:ground-floor", spec.spec_id)),
        ]
        .concat(),
    );

    let roof_form = roof.and_then(|r| roof_form_from(&r.roof_type));

    HistoricalReconstruction {
        id: spec.spec_id.clone(),
        civic_object_id: spec.civic_object_id.clone(),
        name: if spec.title.is_empty() {
            spec.spec_id.clone()
        } else {
            spec.title.clone()
        },
        description,
        position: None,
        footprint: ReconstructionFootprint {
            width_meters: width,
            depth_meters: depth,
        },
        height_meters: height,
        bearing_degrees: 0.0,
        confidence: mass_confidence,
        facade_confidence: Some(provenance_confidence(facade_provenance, mass_confidence)),
        roof_confidence: Some(provenance_confidence(roof_provenance, mass_confidence)),
        ground_floor_confidence: Some(provenance_confidence(ground_provenance, mass_confidence)),
        roof_form,
        time_start: spec.t_start_ms.map(ms_to_iso8601),
        time_end: spec.t_end_ms.map(ms_to_iso8601),
        geometry_url: renderable_geometry_url.clone(),
        geometry_format: renderable_geometry_url.as_ref().map(|_| "GLTF".to_string()),
        foundry_asset_url: scene_uri,
        sources,
    }
}

fn evidence_from_spec(spec: &ReconstructionSpec) -> EvidenceBundle {
    evidence_from_pipeline(spec, None)
}

/// Build the dossier's evidence bundle from both the merged spec AND
/// the raw engine `EvidenceBundle` when available.
///
/// Previous shape (spec-only) showed evidence ONLY from per-part
/// provenance. When `extract_direct` could not populate a part from
/// a sparse artifact payload, the merge step filled the part with the
/// GNN "Block-coherent prior" — so the dossier displayed only
/// inferred priors with no record that a real Sanborn sheet or
/// archival photo had been consulted.
///
/// New shape (this function) emits TWO evidence layers:
///
/// 1. **Direct artifacts** — every artifact in
///    `engine_bundle.direct` is surfaced verbatim, using the real
///    artifact_key, title, source_type, captured_at, etc. This is the
///    "we consulted these sources" layer.
/// 2. **Per-part synthetic items** — the existing behavior. Surfaces
///    which parts of the reconstruction were sourced (with real
///    provenance) vs inferred (with the GNN prior).
///
/// Both layers coexist. A user looking at a Carriage Town Storefront
/// 1925 dossier now sees:
///   - "Sanborn fire insurance map, Flint 1899 sheet 18"  (direct)
///   - "E Kearsley storefront circa 1925"                 (direct)
///   - "Block-coherent prior" x 4                         (per-part)
///
/// Task #10 from the autonomous overnight pass. Closes the
/// "engine isn't joining real artifacts" complaint surfaced in the
/// 2026-05-26 live smoke.
fn evidence_from_pipeline(
    spec: &ReconstructionSpec,
    engine_bundle: Option<&EngineEvidenceBundle>,
) -> EvidenceBundle {
    let spec_id = spec.spec_id.clone();
    let mut items: Vec<EvidenceItem> = Vec::new();

    // Layer 1: direct artifacts the engine retrieved from PostGIS.
    if let Some(bundle) = engine_bundle {
        for artifact in &bundle.direct {
            items.push(evidence_item_from_engine_artifact(
                artifact, &spec_id, /* relation */ "direct",
            ));
        }
        for artifact in &bundle.adjacent {
            items.push(evidence_item_from_engine_artifact(
                artifact, &spec_id, /* relation */ "adjacent",
            ));
        }
    }

    // Layer 2: per-part synthetic items, derived from the merged spec's
    // provenance. Same shape as the prior behavior; preserves the
    // GNN-prior + extract_direct signal so callers can see WHICH parts
    // of the reconstruction the engine actually claimed.
    let mass_prov = spec.mass.as_ref().and_then(|m| m.provenance.as_ref());
    let facade_prov = spec.facades.first().and_then(|f| f.provenance.as_ref());
    let roof_prov = spec.roof.as_ref().and_then(|r| r.provenance.as_ref());
    let ground_prov = spec
        .ground_floor
        .as_ref()
        .and_then(|g| g.provenance.as_ref());

    let parts: Vec<(&str, &str, Option<&PartProvenance>)> = vec![
        ("mass", "SANBORN", mass_prov),
        ("facade", "PHOTOGRAPH", facade_prov),
        ("roof", "SANBORN", roof_prov),
        ("ground_floor", "CITY_DIRECTORY", ground_prov),
    ];

    for (key, kind, prov) in parts {
        let fallback_prefix = format!("{spec_id}:{key}");
        let sources = provenance_sources(prov, &fallback_prefix);
        let part_confidence = provenance_confidence(prov, 0.5);
        for (idx, source) in sources.into_iter().enumerate() {
            let summary = source.name.clone();
            items.push(EvidenceItem {
                id: format!("evidence:{spec_id}:{key}:{idx}"),
                reconstruction_id: spec_id.clone(),
                source,
                evidence_type: kind.to_string(),
                target_node_id: Some(target_node_id(&spec_id, key)),
                confidence: part_confidence,
                thumbnail_url: None,
                summary: Some(summary),
                source_date_label: None,
            });
        }
    }

    let total = items.len() as i32;
    EvidenceBundle {
        reconstruction_id: spec_id,
        items,
        total_count: total,
    }
}

/// Convert an engine `Artifact` (raw PostGIS row) to a GraphQL
/// `EvidenceItem`. The engine's `source_type` is a free-form string
/// (`archival_photo`, `map`, `city_directory`, etc.); we map it to the
/// dossier's `evidence_type` enum string + bucket the GraphQL Source's
/// `source_type` via the existing `normalize_source_type` helper.
fn evidence_item_from_engine_artifact(
    artifact: &EngineArtifact,
    spec_id: &str,
    relation: &str,
) -> EvidenceItem {
    let evidence_type = engine_source_type_to_evidence_type(&artifact.source_type);
    let source = Source {
        id: artifact.artifact_key.clone(),
        name: if !artifact.title.is_empty() {
            artifact.title.clone()
        } else {
            artifact.artifact_key.clone()
        },
        homepage_url: if artifact.uri.is_empty() {
            None
        } else {
            Some(artifact.uri.clone())
        },
        source_type: normalize_source_type(&artifact.source_type),
        public_use_terms: None,
        trust_tier: normalize_trust_tier("MEDIUM"),
        last_checked: artifact.captured_at_ms.map(ms_to_iso8601),
        known_limits: Vec::new(),
        contains_personal_data: false,
    };
    EvidenceItem {
        id: format!("evidence:artifact:{}:{relation}", artifact.artifact_key),
        reconstruction_id: spec_id.to_string(),
        source,
        evidence_type,
        target_node_id: None,
        // Direct artifacts arrive from PostGIS with curator-vetted
        // provenance; default to a moderate-high confidence floor.
        // When the engine eventually trains a confidence-per-artifact
        // signal, swap this for the trained value.
        confidence: if relation == "direct" { 0.85 } else { 0.6 },
        thumbnail_url: None,
        summary: if artifact.citation.is_empty() {
            Some(artifact.title.clone())
        } else {
            Some(artifact.citation.clone())
        },
        source_date_label: artifact.captured_at_ms.map(captured_at_to_year_label),
    }
}

/// Bucket the engine's source_type string to the dossier's
/// evidence_type enum (SANBORN, PHOTOGRAPH, CITY_DIRECTORY, TEXT,
/// OTHER). The string is free-form on PostGIS so the matcher is
/// substring-based + case-insensitive.
fn engine_source_type_to_evidence_type(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("sanborn") {
        return "SANBORN".to_string();
    }
    if lower == "map" || lower.contains("map") {
        // Plain "map" (Sanborn fixture seed labels this way) maps to
        // SANBORN in the dossier; other map types fall through to
        // OTHER.
        return "SANBORN".to_string();
    }
    if lower.contains("photo") || lower.contains("image") {
        return "PHOTOGRAPH".to_string();
    }
    if lower.contains("directory") {
        return "CITY_DIRECTORY".to_string();
    }
    if lower.contains("text") || lower.contains("ocr") {
        return "TEXT".to_string();
    }
    "OTHER".to_string()
}

/// Display label for an artifact's captured_at timestamp. Truncates
/// to year because that's what dossier UI surfaces; the full
/// timestamp lives in provenance for callers that want it.
fn captured_at_to_year_label(ms: i64) -> String {
    let secs = ms / 1_000;
    let nanos = ((ms % 1_000).abs() as u32) * 1_000_000;
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos)
        .map(|dt| dt.format("%Y").to_string())
        .unwrap_or_default()
}

fn conflicts_from_spec(spec: &ReconstructionSpec) -> Vec<MergeConflict> {
    let spec_id = spec.spec_id.clone();
    let mass_prov = spec.mass.as_ref().and_then(|m| m.provenance.as_ref());
    let facade_prov = spec.facades.first().and_then(|f| f.provenance.as_ref());
    let roof_prov = spec.roof.as_ref().and_then(|r| r.provenance.as_ref());
    let ground_prov = spec
        .ground_floor
        .as_ref()
        .and_then(|g| g.provenance.as_ref());

    let parts: Vec<(&str, &str, Option<&PartProvenance>)> = vec![
        ("mass", "Shape and size", mass_prov),
        ("facade", "Walls", facade_prov),
        ("roof", "Roof", roof_prov),
        ("ground_floor", "Street level", ground_prov),
    ];

    parts
        .into_iter()
        .filter(|(_, _, prov)| provenance_has_conflict(*prov))
        .enumerate()
        .map(|(idx, (key, label, _prov))| MergeConflict {
            id: format!("merge-conflict:{spec_id}:{key}:{idx}"),
            reconstruction_id: spec_id.clone(),
            target_node_id: target_node_id(&spec_id, key),
            field_label: label.to_string(),
            disagreements: Vec::new(),
            resolved_value: "reviewed value".to_string(),
            resolution_explanation: "Backend marked this part as having a source conflict."
                .to_string(),
            resolution_threshold: 0.7,
        })
        .collect()
}

fn conflict_part_from_field_path(field_path: &str) -> &'static str {
    let lower = field_path.to_ascii_lowercase();
    if lower.contains("facade") {
        "facade"
    } else if lower.contains("roof") {
        "roof"
    } else if lower.contains("ground") {
        "ground_floor"
    } else {
        "mass"
    }
}

fn synthetic_conflict_source(id: &str, name: &str) -> Source {
    Source {
        id: id.to_string(),
        name: name.to_string(),
        homepage_url: None,
        source_type: "MODEL".to_string(),
        public_use_terms: None,
        trust_tier: "MEDIUM".to_string(),
        last_checked: None,
        known_limits: Vec::new(),
        contains_personal_data: false,
    }
}

fn conflicts_from_engine(spec_id: &str, conflicts: &[EngineMergeConflict]) -> Vec<MergeConflict> {
    conflicts
        .iter()
        .enumerate()
        .map(|(idx, conflict)| {
            let part = conflict_part_from_field_path(&conflict.field_path);
            MergeConflict {
                id: format!("merge-conflict:{spec_id}:{part}:{idx}"),
                reconstruction_id: spec_id.to_string(),
                target_node_id: target_node_id(spec_id, part),
                field_label: conflict.field_path.clone(),
                disagreements: vec![
                    MergeDisagreement {
                        source: synthetic_conflict_source("engine:direct", "Direct source extraction"),
                        stated_value: conflict.direct_value.clone(),
                        confidence: conflict.direct_confidence,
                        evidence_item_id: format!("evidence:{spec_id}:{part}:direct"),
                    },
                    MergeDisagreement {
                        source: synthetic_conflict_source("engine:prior", "Block prior"),
                        stated_value: conflict.prior_value.clone(),
                        confidence: conflict.threshold,
                        evidence_item_id: format!("evidence:{spec_id}:{part}:prior"),
                    },
                ],
                resolved_value: conflict.direct_value.clone(),
                resolution_explanation: "The reconstruction engine kept the direct source value because it met the merge threshold.".to_string(),
                resolution_threshold: conflict.threshold,
            }
        })
        .collect()
}

fn engine_assets_to_proto(
    tenant_id: &str,
    spec_id: &str,
    spec_version: u32,
    assets: &[EngineGeneratedAsset],
) -> Vec<ReconstructionAsset> {
    assets
        .iter()
        .map(|asset| ReconstructionAsset {
            asset_id: asset.asset_id.clone(),
            spec_id: spec_id.to_string(),
            spec_version,
            tenant_id: tenant_id.to_string(),
            asset_type: asset.asset_type.clone(),
            uri: asset.uri.clone(),
            content_hash: asset.content_hash.clone(),
            metadata: asset.metadata.clone().into_iter().collect(),
        })
        .collect()
}

/// Build the Pascal-style flat node tree as a JSON value matching the
/// sidecar's nodeTreeForReconstruction shape exactly (schema declares
/// the field as `nodeTree: JSON!`).
fn node_tree_for_reconstruction(reconstruction_id: &str) -> serde_json::Value {
    let root_id = target_node_id(reconstruction_id, "building");
    let level_id = target_node_id(reconstruction_id, "level:0");
    let mass_id = target_node_id(reconstruction_id, "mass");
    let facade_id = target_node_id(reconstruction_id, "facade");
    let roof_id = target_node_id(reconstruction_id, "roof");
    let ground_id = target_node_id(reconstruction_id, "ground_floor");

    json!({
        "version": 1,
        "source": "backend-reconstruction-spec",
        "rootNodeIds": [root_id],
        "nodes": {
            root_id.clone(): {
                "id": root_id,
                "type": "building",
                "parentId": null,
                "children": [level_id],
            },
            level_id.clone(): {
                "id": level_id,
                "type": "level",
                "parentId": root_id,
                "children": [mass_id, facade_id, ground_id, roof_id],
            },
            mass_id.clone(): {
                "id": mass_id,
                "type": "mass",
                "parentId": level_id,
                "children": [],
            },
            facade_id.clone(): {
                "id": facade_id,
                "type": "facade",
                "parentId": level_id,
                "children": [],
            },
            ground_id.clone(): {
                "id": ground_id,
                "type": "ground_floor",
                "parentId": level_id,
                "children": [],
            },
            roof_id.clone(): {
                "id": roof_id,
                "type": "roof",
                "parentId": level_id,
                "children": [],
            },
        },
    })
}

async fn block_subgraph_for_spec(
    state: &AtlasState,
    focus_spec: &ReconstructionSpec,
) -> async_graphql::Result<BlockSubgraph> {
    let spec_id = focus_spec.spec_id.clone();
    let block_id = focus_spec.block_id.clone();
    let tenant_id = default_tenant();
    let service = ReconstructionGrpcService::new(state.clone());

    let mut request = Request::new(ListReconstructionSpecsRequest {
        tenant_context: Some(TenantContext {
            tenant_id: tenant_id.clone(),
            atlas_node_id: format!("atlas:{tenant_id}"),
            metadata: Default::default(),
        }),
        civic_object_id: String::new(),
        parcel_id: String::new(),
        status: 0,
        page_size: 50,
        page_token: String::new(),
    });
    request.metadata_mut().insert(
        "x-atlas-tenant",
        tenant_id
            .parse()
            .map_err(|err| async_graphql::Error::new(format!("invalid tenant id: {err}")))?,
    );

    let specs = match service.list_reconstruction_specs(request).await {
        Ok(resp) => resp.into_inner().specs,
        Err(status) if status.code() == tonic::Code::Unavailable => {
            // Without a DB the block subgraph is empty. Honest answer:
            // no neighbors found rather than fake data.
            return Ok(BlockSubgraph {
                reconstruction_id: spec_id,
                neighbors: Vec::new(),
            });
        }
        Err(status) => {
            return Err(async_graphql::Error::new(format!(
                "ListReconstructionSpecs failed: {} ({})",
                status.message(),
                status.code()
            )))
        }
    };

    let neighbors: Vec<BlockNeighbor> = specs
        .iter()
        .filter(|spec| spec.spec_id != spec_id)
        .filter(|spec| block_id.is_empty() || spec.block_id == block_id)
        .take(8)
        .map(|spec| BlockNeighbor {
            reconstruction: reconstruction_from_spec(spec),
            relation: "same_block_as".to_string(),
            strength: 0.8,
        })
        .collect();

    Ok(BlockSubgraph {
        reconstruction_id: spec_id,
        neighbors,
    })
}

// ---------------------------------------------------------------------------
// Top-level resolver
// ---------------------------------------------------------------------------

pub async fn resolve_reconstruction_dossier(
    state: &AtlasState,
    reconstruction_id: &str,
) -> async_graphql::Result<Option<ReconstructionDossier>> {
    let spec_id = normalize_spec_id(reconstruction_id);
    let tenant_id = default_tenant();

    let service = ReconstructionGrpcService::new(state.clone());
    let mut request = Request::new(GetReconstructionSpecRequest {
        tenant_context: Some(TenantContext {
            tenant_id: tenant_id.clone(),
            atlas_node_id: format!("atlas:{tenant_id}"),
            metadata: Default::default(),
        }),
        spec_id: spec_id.clone(),
    });
    request.metadata_mut().insert(
        "x-atlas-tenant",
        tenant_id
            .parse()
            .map_err(|err| async_graphql::Error::new(format!("invalid tenant id: {err}")))?,
    );

    let spec = match service.get_reconstruction_spec(request).await {
        Ok(response) => match response.into_inner().spec {
            Some(spec) => spec,
            None => return Ok(None),
        },
        Err(status) if status.code() == tonic::Code::NotFound => return Ok(None),
        Err(status) => {
            return Err(async_graphql::Error::new(format!(
                "ReconstructionGrpcService error: {} ({})",
                status.message(),
                status.code()
            )))
        }
    };

    let reconstruction = reconstruction_from_spec(&spec);
    let evidence = evidence_from_spec(&spec);
    let conflicts = conflicts_from_spec(&spec);
    let block_subgraph = block_subgraph_for_spec(state, &spec).await?;
    let node_tree = node_tree_for_reconstruction(&reconstruction.id);

    let summary = if evidence.total_count > 0 {
        let noun = if evidence.total_count == 1 {
            "source item"
        } else {
            "source items"
        };
        let verb = if evidence.total_count == 1 {
            "supports"
        } else {
            "support"
        };
        format!(
            "{} backend {noun} {verb} this reconstruction.",
            evidence.total_count
        )
    } else {
        "Backend ReconstructionSpec loaded; no source records are attached to this spec yet."
            .to_string()
    };

    let debug = json!({
        "source": "ReconstructionGrpcService.get_reconstruction_spec",
        "requested_id": reconstruction_id,
        "resolved_spec_id": spec.spec_id,
        "spec_version": spec.spec_version,
        "transport": "axum-native-graphql-in-process",
    });

    Ok(Some(ReconstructionDossier {
        reconstruction,
        evidence,
        conflicts,
        block_subgraph,
        node_tree: async_graphql::Json(node_tree),
        summary,
        debug: Some(async_graphql::Json(debug)),
    }))
}

pub async fn resolve_reconstruction_for(
    state: &AtlasState,
    parcel_id: &str,
    year: i32,
) -> async_graphql::Result<Option<ReconstructionDossier>> {
    if parcel_id.trim().is_empty() {
        return Err(async_graphql::Error::new("parcelId is required"));
    }

    let pool = state
        .db_pool()
        .ok_or_else(|| async_graphql::Error::new("DATABASE_URL is required for reconstructionFor"))?
        .clone();
    let tenant_id = default_tenant();
    let at_ms = year_start_ms(year)?;
    let tenant_context = TenantContext {
        tenant_id: tenant_id.clone(),
        atlas_node_id: format!("atlas:{tenant_id}"),
        metadata: Default::default(),
    };
    let request = EngineReconstructionRequest {
        tenant_context: tenant_context.clone(),
        parcel_id: parcel_id.to_string(),
        time_slice: TimeSlice {
            at_ms: Some(at_ms),
            start_ms: None,
            end_ms: None,
        },
        requested_by: "graphql:reconstructionFor".to_string(),
        auto_approve: false,
    };

    let repository = PostgisRepository::new(pool.clone());
    let embeddings = ZeroEmbeddingProvider::default();
    let prior_model = BlockCoherentPriorModel::default();
    let store = std::sync::Arc::new(LocalDirAssetStore::from_env());
    let asset_generator = SceneFoundryRenderer::new(store)
        .with_job_queue(std::sync::Arc::new(PgRenderJobQueue::new(pool)));
    let output = run_full_pipeline(
        request,
        &repository,
        &embeddings,
        &prior_model,
        &asset_generator,
    )
    .await
    .map_err(|err| async_graphql::Error::new(format!("run_full_pipeline failed: {err}")))?;

    let mut spec = output.merged.spec.clone();
    // Record what the renderer actually produced: procedural PBR texture
    // provenance until a GPU appearance pass upgrades it.
    let tier_decision = select_tier(&spec, &TierThresholds::default());
    stamp_massing_texture_provenance(&mut spec, tier_decision.tier);
    if spec.assets.is_empty() {
        spec.assets = engine_assets_to_proto(
            &tenant_id,
            &spec.spec_id,
            spec.spec_version,
            &output.asset_manifest.assets,
        );
    }

    let reconstruction = reconstruction_from_spec(&spec);
    // Task #10 fix: surface the engine's raw artifacts (output.evidence)
    // alongside the per-part synthetic items. Previously this resolver
    // only called evidence_from_spec(&spec), which lost track of the
    // real Sanborn / photo artifacts whenever extract_direct couldn't
    // populate spec parts from their sparse payload_jsonb.
    let evidence = evidence_from_pipeline(&spec, Some(&output.evidence));
    let mut conflicts = conflicts_from_engine(&spec.spec_id, &output.merged.conflicts);
    if conflicts.is_empty() {
        conflicts = conflicts_from_spec(&spec);
    }
    let block_subgraph = block_subgraph_for_spec(state, &spec).await?;
    let node_tree = node_tree_for_reconstruction(&reconstruction.id);
    let source_word = if evidence.total_count == 1 {
        "source item"
    } else {
        "source items"
    };
    let summary = format!(
        "Live reconstruction pipeline assembled {} {source_word} for {parcel_id} in {year}.",
        evidence.total_count
    );
    let debug = json!({
        "source": "civic-atlas-reconstruction-engine.run_full_pipeline",
        "parcelId": parcel_id,
        "year": year,
        "specId": spec.spec_id,
        "assetManifestId": output.asset_manifest.manifest_id,
        "conflictCount": output.merged.conflicts.len(),
        "embeddingModel": output.embedded_subgraph.embedding_model,
        "embeddingModelVersion": output.embedded_subgraph.embedding_model_version,
        "priorModelVersion": output.prior.model_version,
    });

    Ok(Some(ReconstructionDossier {
        reconstruction,
        evidence,
        conflicts,
        block_subgraph,
        node_tree: async_graphql::Json(node_tree),
        summary,
        debug: Some(async_graphql::Json(debug)),
    }))
}

pub struct ReconstructionQuery;

#[Object]
impl ReconstructionQuery {
    /// Runs the live reconstruction pipeline for a parcel/civic-object id and
    /// target year. This is the resident-facing atelier entry point.
    async fn reconstruction_for(
        &self,
        ctx: &Context<'_>,
        parcel_id: String,
        year: i32,
    ) -> async_graphql::Result<Option<ReconstructionDossier>> {
        let state = ctx
            .data::<AtlasState>()
            .map_err(|_| async_graphql::Error::new("AtlasState missing from GraphQL context"))?;
        resolve_reconstruction_for(state, &parcel_id, year).await
    }

    /// One-shot atelier payload. Returns null when the reconstruction
    /// spec is not in PostGIS for the active tenant.
    async fn reconstruction_dossier(
        &self,
        ctx: &Context<'_>,
        reconstruction_id: String,
    ) -> async_graphql::Result<Option<ReconstructionDossier>> {
        let state = ctx
            .data::<AtlasState>()
            .map_err(|_| async_graphql::Error::new("AtlasState missing from GraphQL context"))?;
        resolve_reconstruction_dossier(state, &reconstruction_id).await
    }
}
