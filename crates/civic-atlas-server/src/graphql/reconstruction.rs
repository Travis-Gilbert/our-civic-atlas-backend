//! GraphQL types and resolvers for the reconstruction surface.
//!
//! Mirrors the sidecar's `reconstructionDossier(reconstructionId)` query
//! (apps/graphql-server/src/schema.ts), but runs in-process: the resolver
//! calls the existing `ReconstructionGrpcService` directly without a
//! network hop, reusing the SQL, validation, and tenant logic Codex
//! shipped in the gRPC handler.
//!
//! Field coverage: this commit ships a minimum-viable shape (reconstruction
//! id/name/confidence/civicObjectId + summary + debug). Subsequent commits
//! expand to full parity: footprint, height/bearing, per-part confidences,
//! roof form, time range, sources array, evidence bundle, conflicts, block
//! subgraph, node tree.

use std::env;

use async_graphql::{Context, Object, SimpleObject};
use civic_atlas_types::civic_atlas::v1::{
    reconstruction_service_server::ReconstructionService, GetReconstructionSpecRequest,
    ReconstructionSpec, TenantContext,
};
use serde_json::json;
use tonic::Request;

use crate::reconstruction::ReconstructionGrpcService;
use crate::AtlasState;

/// HistoricalReconstruction surface. Field set matches what frontend
/// queries fetch today (atelier-dossier + civic-research). Expansion
/// follow-ups: footprint (typed nested struct), heightMeters,
/// bearingDegrees, per-part confidences, roofForm, geometryUrl, sources.
#[derive(SimpleObject, Default, Clone)]
pub struct HistoricalReconstruction {
    pub id: String,
    pub civic_object_id: String,
    pub name: String,
    pub description: String,
    pub confidence: f64,
    /// [longitude, latitude] tuple serialized as a JSON array, matching
    /// the sidecar's LatLng scalar. None when the spec carries no
    /// centroid (rare; civic_atlas seeds geometry via PostGIS).
    pub position: Option<async_graphql::Json<[f64; 2]>>,
    /// ISO 8601 timestamp marking when this building's lifespan began.
    pub time_start: Option<String>,
    /// ISO 8601 timestamp marking when this building's lifespan ended.
    pub time_end: Option<String>,
}

/// Minimum-viable ReconstructionDossier surface for this commit.
/// `node_tree` and `debug` are JSON scalars per the sidecar schema
/// (apps/graphql-server/src/schema.ts:1067).
#[derive(SimpleObject)]
pub struct ReconstructionDossier {
    pub reconstruction: HistoricalReconstruction,
    pub summary: String,
    pub debug: Option<async_graphql::Json<serde_json::Value>>,
}

/// Normalize a frontend-visible reconstructionId (e.g.,
/// `historical:carriage-town:storefront` or `building:carriage-town:3`)
/// into the backend spec_id (`spec:carriage-town:3`). Mirrors the
/// sidecar's `normalizeSpecId` (apps/graphql-server/src/schema.ts:538).
fn normalize_spec_id(reconstruction_id: &str) -> String {
    if reconstruction_id.starts_with("spec:") {
        return reconstruction_id.to_string();
    }
    // Direct lookups for fixture-historical-id → spec_id.
    match reconstruction_id {
        "historical:carriage-town:whaley-house" => return "spec:carriage-town:1".to_string(),
        "historical:carriage-town:628-kearsley" => return "spec:carriage-town:2".to_string(),
        "historical:carriage-town:storefront" => return "spec:carriage-town:3".to_string(),
        "historical:carriage-town:workers-cottage" => return "spec:carriage-town:4".to_string(),
        "historical:carriage-town:stockton-house" => return "spec:carriage-town:5".to_string(),
        _ => {}
    }
    // building:carriage-town:N → spec:carriage-town:N pattern.
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

/// Map a backend ReconstructionSpec proto into the GraphQL
/// HistoricalReconstruction surface. Overall confidence is the mean of
/// the per-part confidences that are present (matches the sidecar's
/// implicit averaging in reconstructionFromSpec).
fn reconstruction_from_spec(spec: &ReconstructionSpec) -> HistoricalReconstruction {
    HistoricalReconstruction {
        id: spec.spec_id.clone(),
        civic_object_id: spec.civic_object_id.clone(),
        name: if spec.title.is_empty() {
            spec.spec_id.clone()
        } else {
            spec.title.clone()
        },
        // The proto does not carry a top-level description. metadata
        // may include a free-form note; fall back to empty when absent.
        description: spec
            .metadata
            .get("description")
            .or_else(|| spec.metadata.get("notes"))
            .cloned()
            .unwrap_or_default(),
        confidence: overall_confidence(spec),
        // Position is sourced from PostGIS geometry on the civic_object
        // row, not from the spec proto. Future commit joins through
        // civic_objects → ST_Centroid() to populate this. Until then,
        // None is the honest answer.
        position: None,
        // Time range maps from spec.t_start_ms / t_end_ms (epoch
        // milliseconds) into ISO 8601 strings when set.
        time_start: spec.t_start_ms.map(ms_to_iso8601),
        time_end: spec.t_end_ms.map(ms_to_iso8601),
    }
}

/// Convert a Unix epoch milliseconds value into an ISO 8601 timestamp
/// (UTC). Used for `time_start` / `time_end` on the GraphQL
/// HistoricalReconstruction. Out-of-range values fall back to an empty
/// string so the resolver never panics on bad data.
fn ms_to_iso8601(ms: i64) -> String {
    let secs = ms / 1_000;
    let nanos = ((ms % 1_000).abs() as u32) * 1_000_000;
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

/// Mean of per-part confidences across mass, primary facade, roof, and
/// ground floor. Returns 0.0 when no part has provenance.
fn overall_confidence(spec: &ReconstructionSpec) -> f64 {
    let mut values: Vec<f64> = Vec::with_capacity(4);
    if let Some(mass) = spec.mass.as_ref() {
        if let Some(p) = mass.provenance.as_ref() {
            values.push(p.part_confidence);
        }
    }
    if let Some(facade) = spec.facades.first() {
        if let Some(p) = facade.provenance.as_ref() {
            values.push(p.part_confidence);
        }
    }
    if let Some(roof) = spec.roof.as_ref() {
        if let Some(p) = roof.provenance.as_ref() {
            values.push(p.part_confidence);
        }
    }
    if let Some(gf) = spec.ground_floor.as_ref() {
        if let Some(p) = gf.provenance.as_ref() {
            values.push(p.part_confidence);
        }
    }
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

/// Resolve a reconstruction dossier for the given reconstructionId by
/// calling the existing ReconstructionGrpcService in-process. Returns
/// None when the spec is not found in PostGIS.
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
    // The gRPC interceptor in tenant-resolver expects the tenant in
    // metadata for the auth-tenant path. We attach it both ways so an
    // in-process call without the metadata layer still validates.
    request.metadata_mut().insert(
        "x-atlas-tenant",
        tenant_id
            .parse()
            .map_err(|err| async_graphql::Error::new(format!("invalid tenant id: {err}")))?,
    );

    match service.get_reconstruction_spec(request).await {
        Ok(response) => {
            let Some(spec) = response.into_inner().spec else {
                return Ok(None);
            };
            let reconstruction = reconstruction_from_spec(&spec);
            let summary = format!(
                "Backend ReconstructionSpec loaded for {} (version {}).",
                reconstruction.id, spec.spec_version
            );
            let debug = json!({
                "source": "ReconstructionGrpcService.get_reconstruction_spec",
                "requested_id": reconstruction_id,
                "resolved_spec_id": spec.spec_id,
                "spec_version": spec.spec_version,
                "transport": "axum-native-graphql-in-process",
            });
            Ok(Some(ReconstructionDossier {
                reconstruction,
                summary,
                debug: Some(async_graphql::Json(debug)),
            }))
        }
        Err(status) if status.code() == tonic::Code::NotFound => Ok(None),
        Err(status) => Err(async_graphql::Error::new(format!(
            "ReconstructionGrpcService error: {} ({})",
            status.message(),
            status.code()
        ))),
    }
}

pub struct ReconstructionQuery;

#[Object]
impl ReconstructionQuery {
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
