//! The testable core of the `reconstruct` tool.
//!
//! `run_reconstruct(input)` does all of the real work and is a plain async fn
//! with no MCP types in its signature, so the pipeline can be exercised by a
//! unit test without a live MCP client. The MCP tool in `server.rs` is a thin
//! wrapper that deserializes the input, calls this, and shapes the result into
//! `CallToolResult` content.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use civic_atlas_reconstruction_engine::{
    run_full_pipeline, Artifact, AssetManifest, BlockCoherentPriorModel, DecodedArtifact,
    GeneratedAsset, InMemoryRepository, ReconstructionRequest, ZeroEmbeddingProvider,
};
use civic_atlas_renderer::{LocalDirAssetStore, SceneFoundryRenderer};
use civic_atlas_types::civic_atlas::v1::{CivicObject, TenantContext, TimeSlice};

use schemars::JsonSchema;

/// Convention: a description-only (no photo) request lands in the tier_c
/// description-only render tier; a single facade photo lifts it to tier_a.
/// The ladder is described in `engine_info` and is owned by the renderer's
/// `select_tier`; this string is purely the human-facing summary.
pub const TIER_LADDER: &str = "tier_c_description_only (Sanborn/GIS/text only) -> \
tier_a_single_facade_photo (one rectifiable facade photo) -> \
sparse multi-view -> many-photo dense capture. \
The procedural massing render is produced at every tier; richer tiers add \
GPU refinement dispatched out of band.";

/// Structured evidence about one source for the focus building.
///
/// One `EvidenceInput` maps to one engine `Artifact` whose `DecodedArtifact`
/// variant is chosen by `source_type`. Only the commonly-populated fields are
/// surfaced; everything the engine can read from a decoded artifact is covered
/// for the Sanborn, photo, GIS, directory, and free-text cases.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct EvidenceInput {
    /// One of: "sanborn_sheet", "archival_photo", "gis_feature",
    /// "directory", "text". Unknown values fall back to free text.
    pub source_type: String,
    /// Stable id for this source. Defaults to a derived id when omitted.
    #[serde(default)]
    pub source_id: Option<String>,
    /// Source URI (catalog page, image URL, dataset URL).
    #[serde(default)]
    pub uri: Option<String>,
    /// Human-readable source title.
    #[serde(default)]
    pub title: Option<String>,
    /// Year the source documents/was captured (used for citation + anchoring).
    #[serde(default)]
    pub captured_at_year: Option<i32>,
    /// Number of stories the source attests.
    #[serde(default)]
    pub story_count: Option<u32>,
    /// Sanborn material color code (e.g. "red" = brick, "yellow" = frame).
    #[serde(default)]
    pub material_code: Option<String>,
    /// Roof form attested by the source (e.g. "flat", "gable", "hipped").
    #[serde(default)]
    pub roof_form: Option<String>,
    /// Number of bays observed across the primary facade (photo evidence).
    #[serde(default)]
    pub bay_count: Option<u32>,
    /// Free-text notation (Sanborn use note, GIS attribute blob, caption).
    #[serde(default)]
    pub notation: Option<String>,
    /// Free text for the "text" source type (and photo caption fallback).
    #[serde(default)]
    pub text: Option<String>,
}

/// The footprint + temporal anchor of the focus (often demolished) building.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct BuildingInput {
    /// Footprint as a WKT POLYGON in lon/lat (e.g. "POLYGON((...))").
    #[serde(default)]
    pub footprint_wkt: Option<String>,
    /// Footprint as a GeoJSON Polygon string (alternative to WKT).
    #[serde(default)]
    pub footprint_geojson: Option<String>,
    /// Year the building first stood (sets the focus object's time_start).
    #[serde(default)]
    pub time_start_year: Option<i32>,
}

/// Input to `reconstruct`: a building, its evidence, and the time/tenant
/// constraints. This is the `reconstruct(domain, evidence, constraints)`
/// contract: `tenant` is the domain, `evidence` is the evidence, and
/// `year`/`building` are the constraints that pin the reconstruction in time
/// and space.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReconstructInput {
    /// Tenant/domain id. Defaults to "flint".
    #[serde(default)]
    pub tenant: Option<String>,
    /// Parcel (or building) id to reconstruct. Required.
    pub parcel_id: String,
    /// Reconstruction year (the time slice). Defaults to the building's
    /// time_start_year, then to 1900.
    #[serde(default)]
    pub year: Option<i32>,
    /// Human-readable title for the reconstruction.
    #[serde(default)]
    pub title: Option<String>,
    /// The focus building footprint + temporal anchor.
    #[serde(default)]
    pub building: BuildingInput,
    /// Structured evidence: one entry per source.
    #[serde(default)]
    pub evidence: Vec<EvidenceInput>,
}

/// One asset in the output, mirroring `GeneratedAsset` but flat for JSON.
#[derive(Debug, Clone, Serialize)]
pub struct ReconstructAsset {
    pub asset_id: String,
    pub asset_type: String,
    pub uri: String,
    pub content_hash: String,
    pub metadata: BTreeMap<String, String>,
}

impl From<&GeneratedAsset> for ReconstructAsset {
    fn from(asset: &GeneratedAsset) -> Self {
        Self {
            asset_id: asset.asset_id.clone(),
            asset_type: asset.asset_type.clone(),
            uri: asset.uri.clone(),
            content_hash: asset.content_hash.clone(),
            metadata: asset.metadata.clone(),
        }
    }
}

/// The reference to the scene-IR glTF resource. This is what callers fetch to
/// render; we never inline the mesh bytes.
#[derive(Debug, Clone, Serialize)]
pub struct GltfResource {
    /// Absolute filesystem path to the written GLB.
    pub path: String,
    /// file:// URI for the GLB (the MCP resource link target).
    pub file_uri: String,
    /// sha256-<hex> content hash of the GLB bytes.
    pub content_hash: String,
    /// Byte size of the GLB on disk.
    pub size_bytes: u64,
}

/// The result of one reconstruction. The manifest + provenance are inline; the
/// glTF is a resource reference (path + file:// URI + hash + size).
#[derive(Debug, Clone, Serialize)]
pub struct ReconstructOutput {
    pub manifest_id: String,
    pub spec_id: String,
    pub spec_version: u32,
    pub status: String,
    pub fidelity_tier: String,
    /// renderTier from the renderer metadata, when present.
    pub render_tier: Option<String>,
    pub generator: String,
    pub manifest_metadata: BTreeMap<String, String>,
    pub assets: Vec<ReconstructAsset>,
    /// The full provenance.json document the renderer wrote, as raw JSON.
    pub provenance: serde_json::Value,
    /// The glTF scene IR, returned by reference (resource link), not inline.
    pub gltf: GltfResource,
}

/// Run the full reconstruction pipeline for one request and surface the scene
/// IR. Pure of MCP types: callable directly from tests.
pub async fn run_reconstruct(input: ReconstructInput) -> Result<ReconstructOutput> {
    let tenant = input
        .tenant
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "flint".to_string());

    let year = input
        .year
        .or(input.building.time_start_year)
        .unwrap_or(1900);

    // Focus building footprint: prefer explicit GeoJSON, then WKT. The engine's
    // footprint parser accepts both GeoJSON Polygon objects and WKT POLYGON
    // strings, so we store whichever the caller gave us verbatim.
    let geometry_json = input
        .building
        .footprint_geojson
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            input
                .building
                .footprint_wkt
                .clone()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_default();

    let building_start_year = input.building.time_start_year.unwrap_or(year);

    let focus = CivicObject {
        id: format!("building:{}", input.parcel_id),
        tenant_id: tenant.clone(),
        name: input
            .title
            .clone()
            .unwrap_or_else(|| format!("building:{}", input.parcel_id)),
        object_type: "BuildingPresence".to_string(),
        geometry_json,
        time_start_ms: Some(year_to_epoch_ms(building_start_year)),
        time_end_ms: None,
        confidence: 1.0,
        source_ids: input
            .evidence
            .iter()
            .enumerate()
            .map(|(idx, evidence)| evidence_source_id(evidence, idx))
            .collect(),
        dossier_path: String::new(),
        attributes: HashMap::from([("block_id".to_string(), format!("block:{}", input.parcel_id))]),
    };

    let direct_artifacts: Vec<Artifact> = input
        .evidence
        .iter()
        .enumerate()
        .map(|(idx, evidence)| evidence_to_artifact(evidence, idx))
        .collect();

    let repository = InMemoryRepository {
        parcel_history: vec![focus],
        direct_artifacts,
        adjacent_artifacts: Vec::new(),
        graph: None,
    };

    let request = ReconstructionRequest {
        tenant_context: TenantContext {
            tenant_id: tenant.clone(),
            atlas_node_id: format!("atlas:{tenant}"),
            metadata: Default::default(),
        },
        parcel_id: input.parcel_id.clone(),
        time_slice: TimeSlice {
            at_ms: Some(year_to_epoch_ms(year)),
            start_ms: None,
            end_ms: None,
        },
        requested_by: "mcp:civic-atlas-reconstruction-mcp".to_string(),
        auto_approve: false,
    };

    // Per-call asset directory under the system temp dir. The renderer is
    // content-addressed, so re-running an identical reconstruct lands on the
    // same bytes/URI. We mint a file:// public base URL so the asset URIs the
    // manifest carries ARE the resource links a host can fetch.
    let asset_root: PathBuf = std::env::temp_dir()
        .join("scene-foundry-mcp")
        .join(sanitize_dir(&input.parcel_id));
    let public_base_url = format!("file://{}", asset_root.display());

    let store = Arc::new(LocalDirAssetStore::new(&asset_root, &public_base_url));
    let renderer = SceneFoundryRenderer::new(store);
    let embeddings = ZeroEmbeddingProvider::default();
    let prior_model = BlockCoherentPriorModel::default();

    let output = run_full_pipeline(request, &repository, &embeddings, &prior_model, &renderer)
        .await
        .context("running reconstruction pipeline")?;

    let manifest: &AssetManifest = &output.asset_manifest;

    // Resolve the on-disk path for each produced asset from its file:// URI by
    // stripping the public base URL prefix and joining onto the asset root.
    let glb_asset = manifest
        .assets
        .iter()
        .find(|asset| asset.uri.ends_with(".glb"))
        .context("renderer produced no .glb geometry asset")?;
    let glb_path = asset_path(&asset_root, &public_base_url, &glb_asset.uri);
    let glb_size = tokio::fs::metadata(&glb_path)
        .await
        .with_context(|| format!("stat GLB at {}", glb_path.display()))?
        .len();

    // Read the provenance.json the renderer wrote, inline it as JSON.
    let provenance_value = match manifest
        .assets
        .iter()
        .find(|asset| asset.uri.ends_with(".json"))
    {
        Some(record_asset) => {
            let record_path = asset_path(&asset_root, &public_base_url, &record_asset.uri);
            let bytes = tokio::fs::read(&record_path).await.with_context(|| {
                format!("reading provenance record at {}", record_path.display())
            })?;
            serde_json::from_slice(&bytes).with_context(|| {
                format!("parsing provenance record at {}", record_path.display())
            })?
        }
        None => serde_json::Value::Null,
    };

    let render_tier = manifest.metadata.get("renderTier").cloned();

    Ok(ReconstructOutput {
        manifest_id: manifest.manifest_id.clone(),
        spec_id: manifest.spec_id.clone(),
        spec_version: manifest.spec_version,
        status: manifest.status.clone(),
        fidelity_tier: manifest.fidelity_tier.clone(),
        render_tier,
        generator: manifest.generator.clone(),
        manifest_metadata: manifest.metadata.clone(),
        assets: manifest.assets.iter().map(ReconstructAsset::from).collect(),
        provenance: provenance_value,
        gltf: GltfResource {
            path: glb_path.display().to_string(),
            file_uri: glb_asset.uri.clone(),
            content_hash: glb_asset.content_hash.clone(),
            size_bytes: glb_size,
        },
    })
}

/// Map one `EvidenceInput` to the engine `Artifact` whose decoded variant the
/// engine's direct-extraction stage knows how to read.
fn evidence_to_artifact(evidence: &EvidenceInput, index: usize) -> Artifact {
    let source_id = evidence_source_id(evidence, index);
    let captured_at_ms = evidence.captured_at_year.map(year_to_epoch_ms);
    let title = evidence
        .title
        .clone()
        .unwrap_or_else(|| format!("{} source {}", evidence.source_type, index + 1));
    let citation = match evidence.captured_at_year {
        Some(year) => format!("{title} ({year})"),
        None => title.clone(),
    };

    let decoded = match evidence.source_type.trim().to_ascii_lowercase().as_str() {
        "sanborn_sheet" | "sanborn" | "map" => DecodedArtifact::SanbornSheet {
            footprint_wkt: None,
            story_count: evidence.story_count,
            material_code: evidence.material_code.clone(),
            notation: evidence.notation.clone(),
            roof_form: evidence.roof_form.clone(),
        },
        "archival_photo" | "photo" => DecodedArtifact::Photo {
            visible_facades: vec!["primary".to_string()],
            story_count: evidence.story_count,
            bay_count: evidence.bay_count,
            roof_form: evidence.roof_form.clone(),
            caption_text: evidence.text.clone().or_else(|| evidence.notation.clone()),
            scale_height_m: None,
        },
        "gis_feature" | "gis" => {
            let mut attributes: BTreeMap<String, serde_json::Value> = BTreeMap::new();
            if let Some(stories) = evidence.story_count {
                attributes.insert("stories".to_string(), serde_json::json!(stories));
            }
            if let Some(material) = evidence.material_code.as_ref() {
                attributes.insert("primary_material".to_string(), serde_json::json!(material));
            }
            if let Some(notation) = evidence.notation.as_ref() {
                attributes.insert("use_type".to_string(), serde_json::json!(notation));
            }
            DecodedArtifact::GisFeature {
                footprint_wkt: None,
                attributes,
                source_layer: evidence.title.clone(),
                capture_date_ms: captured_at_ms,
            }
        }
        "directory" | "directory_entry" => DecodedArtifact::DirectoryEntry {
            business_name: evidence.title.clone(),
            residents: Vec::new(),
            address: None,
            use_type: evidence.notation.clone().or_else(|| evidence.text.clone()),
        },
        // "text" and any unrecognized source_type fall back to free text.
        _ => DecodedArtifact::Text {
            text: evidence
                .text
                .clone()
                .or_else(|| evidence.notation.clone())
                .unwrap_or_default(),
        },
    };

    Artifact {
        artifact_id: source_id.clone(),
        artifact_key: source_id,
        source_type: evidence.source_type.clone(),
        title,
        uri: evidence.uri.clone().unwrap_or_default(),
        citation,
        captured_at_ms,
        fetched_at_ms: None,
        content_hash: String::new(),
        decoded,
        metadata: BTreeMap::new(),
    }
}

fn evidence_source_id(evidence: &EvidenceInput, index: usize) -> String {
    evidence
        .source_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("evidence:{}:{}", evidence.source_type, index + 1))
}

/// Convert a calendar year to epoch milliseconds at Jan 1 00:00 UTC.
///
/// Uses a days-since-epoch computation with a civil leap-day count so years
/// before 1970 (the common case here, since these buildings predate the epoch)
/// land on real Jan 1 boundaries. No chrono dependency.
fn year_to_epoch_ms(year: i32) -> i64 {
    days_from_civil(year, 1, 1) * 86_400_000
}

/// Days from the Unix epoch (1970-01-01) to the given civil date. Negative for
/// dates before the epoch. Howard Hinnant's `days_from_civil` algorithm.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let m = month as i64;
    let d = day as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Strip the public base URL prefix from an asset URI and join the remaining
/// store-relative key onto the asset root to recover the on-disk path.
fn asset_path(root: &std::path::Path, public_base_url: &str, uri: &str) -> PathBuf {
    let prefix = format!("{}/", public_base_url.trim_end_matches('/'));
    let key = uri.strip_prefix(&prefix).unwrap_or(uri);
    root.join(key)
}

fn sanitize_dir(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sanborn_evidence_reconstructs_a_completed_glb() {
        // A Whaley-House-like Sanborn input: 3 stories, brick (red), hipped
        // roof, with a small footprint WKT in lon/lat near Flint.
        let input = ReconstructInput {
            tenant: Some("flint".to_string()),
            parcel_id: "parcel:test:whaley".to_string(),
            year: Some(1900),
            title: Some("Test Whaley House".to_string()),
            building: BuildingInput {
                footprint_wkt: Some(
                    "POLYGON((-83.7082 43.0118, -83.70802806 43.0118, \
                     -83.70802806 43.01196170, -83.7082 43.01196170, -83.7082 43.0118))"
                        .to_string(),
                ),
                footprint_geojson: None,
                time_start_year: Some(1885),
            },
            evidence: vec![EvidenceInput {
                source_type: "sanborn_sheet".to_string(),
                source_id: Some("sanborn:test:1899:s18".to_string()),
                uri: Some("https://example.org/sanborn".to_string()),
                title: Some("Sanborn Flint 1899 sheet 18".to_string()),
                captured_at_year: Some(1899),
                story_count: Some(3),
                material_code: Some("red".to_string()),
                roof_form: Some("hipped".to_string()),
                bay_count: None,
                notation: None,
                text: None,
            }],
        };

        let output = run_reconstruct(input).await.expect("reconstruct succeeds");

        // The manifest must be a real, completed render.
        assert_eq!(output.status, "completed", "manifest status");

        // At least one asset must carry a real sha256 content hash.
        assert!(
            output
                .assets
                .iter()
                .any(|asset| asset.content_hash.starts_with("sha256-")
                    && asset.content_hash.len() > 15),
            "expected a real sha256 content hash on an asset; got {:?}",
            output.assets
        );

        // The glTF resource hash matches and the file exists and is a real GLB.
        assert!(
            output.gltf.content_hash.starts_with("sha256-"),
            "gltf content hash: {}",
            output.gltf.content_hash
        );
        assert!(output.gltf.size_bytes > 0, "gltf must have bytes");
        let bytes = std::fs::read(&output.gltf.path).expect("GLB file exists on disk");
        assert_eq!(&bytes[0..4], b"glTF", "GLB must start with the glTF magic");

        // The provenance record is inlined and non-null.
        assert!(
            !output.provenance.is_null(),
            "provenance record should be inlined"
        );

        // No-photo Sanborn evidence selects the description-only tier.
        assert_eq!(
            output.render_tier.as_deref(),
            Some("tier_c_description_only"),
            "render tier for description-only evidence"
        );
    }

    #[test]
    fn year_to_epoch_ms_matches_known_boundaries() {
        // 1970-01-01 is the epoch.
        assert_eq!(year_to_epoch_ms(1970), 0);
        // 1900-01-01 UTC is -2_208_988_800_000 ms (the value the renderer
        // example hardcodes for 1900).
        assert_eq!(year_to_epoch_ms(1900), -2_208_988_800_000);
    }
}
