//! Unified civic data layer contract.
//!
//! This module adds the generic `layers` catalog and `layerView` projection
//! without replacing the richer producer-specific queries. Traffic, event
//! planner, and reconstruction stay the source of truth for their detail
//! surfaces; this contract is the additive catalog/read-model spine that later
//! producers conform to.

use async_graphql::{Context, Enum, InputObject, Json, Object, SimpleObject, ID};
use civic_atlas_types::civic_atlas::v1::{
    reconstruction_service_server::ReconstructionService, ListReconstructionSpecsRequest,
    TenantContext,
};
use civic_atlas_types::event_planner::{
    EventLayerListRequest, EventPlannerService, PlacementListRequest,
};
use civic_atlas_types::theseus_bridge::v1::{
    MorphologicalGraphEdge, MorphologicalGraphRequest, MorphologicalGraphResponse,
    MorphologicalMovement, MorphologicalPlace,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tonic::{Code, Request};

use crate::event_planner::EventPlannerGrpcService;
use crate::graphql::reconstruction::{
    ms_to_iso8601, reconstruction_from_spec, BboxInput, HistoricalReconstruction,
};
use crate::graphql::traffic::{snapshot_for_network, TrafficFeedStatus, TrafficSegment};
use crate::reconstruction::ReconstructionGrpcService;
use crate::AtlasState;

const DEFAULT_TENANT_SLUG: &str = "flint";
const TRAFFIC_NETWORK_ID: &str = "flint-downtown";
const TRAFFIC_LAYER_ID: &str = "layer:traffic:flint-downtown";
const RECONSTRUCTION_LAYER_ID: &str = "layer:reconstruction:flint:historical";
const MORPHOLOGICAL_LAYER_ID: &str = "layer:morphological-graph:flint:city2graph";
const MORPHOLOGICAL_MODEL_RUN_ID: &str = "model-run:morphological-graph:city2graph:flint";

#[derive(Enum, Copy, Clone, Eq, PartialEq, Hash)]
pub enum LayerKind {
    #[graphql(name = "PLACE")]
    Place,
    #[graphql(name = "SIGNAL")]
    Signal,
    #[graphql(name = "EVENT")]
    Event,
    #[graphql(name = "RECONSTRUCTION")]
    Reconstruction,
    #[graphql(name = "TRAFFIC")]
    Traffic,
    #[graphql(name = "METRIC")]
    Metric,
    #[graphql(name = "SCENARIO")]
    Scenario,
    #[graphql(name = "EVENT_SURFACE")]
    EventSurface,
    #[graphql(name = "FRESH_SIGNAL")]
    FreshSignal,
    #[graphql(name = "UPLOAD")]
    Upload,
    #[graphql(name = "MODEL_OUTPUT")]
    ModelOutput,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum LayerSourceAction {
    #[graphql(name = "SEARCH")]
    Search,
    #[graphql(name = "UPLOAD")]
    Upload,
    #[graphql(name = "MODEL")]
    Model,
    #[graphql(name = "BASE")]
    Base,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum LayerLifecycleState {
    #[graphql(name = "RAW")]
    Raw,
    #[graphql(name = "CANDIDATE")]
    Candidate,
    #[graphql(name = "REVIEWED")]
    Reviewed,
    #[graphql(name = "PUBLIC")]
    Public,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum LayerViewStatus {
    #[graphql(name = "PUBLIC")]
    Public,
    #[graphql(name = "REVIEW_PENDING")]
    ReviewPending,
    #[graphql(name = "FIXTURE")]
    Fixture,
    #[graphql(name = "UNAVAILABLE")]
    Unavailable,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum ReviewStatus {
    #[graphql(name = "ACCEPTED")]
    Accepted,
    #[graphql(name = "NEEDS_REVIEW")]
    NeedsReview,
    #[graphql(name = "CORROBORATED")]
    Corroborated,
    #[graphql(name = "CONTESTED")]
    Contested,
    #[graphql(name = "RETRACTED")]
    Retracted,
    #[graphql(name = "OUTDATED")]
    Outdated,
    #[graphql(name = "WITHDRAWN")]
    Withdrawn,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum VisibilityLevel {
    #[graphql(name = "PUBLIC")]
    Public,
    #[graphql(name = "REVIEW_ONLY")]
    ReviewOnly,
    #[graphql(name = "PRIVATE")]
    Private,
}

#[derive(SimpleObject, Clone)]
pub struct LayerTemporalRange {
    pub start: Option<String>,
    pub end: Option<String>,
}

#[derive(SimpleObject, Clone)]
pub struct LayerConfidenceRange {
    pub min: f64,
    pub max: f64,
}

#[derive(SimpleObject, Clone)]
pub struct LayerReviewStatusCount {
    pub status: ReviewStatus,
    pub count: i32,
}

#[derive(SimpleObject, Clone)]
pub struct LayerProvenanceSummary {
    pub source_count: i32,
    pub confidence_range: LayerConfidenceRange,
    pub review_status_mix: Vec<LayerReviewStatusCount>,
}

#[derive(SimpleObject, Clone)]
pub struct Layer {
    pub id: ID,
    pub kind: LayerKind,
    pub source_action: LayerSourceAction,
    pub title: String,
    pub lifecycle_state: LayerLifecycleState,
    pub renderer_boundary_id: String,
    pub record_count: i32,
    pub temporal_range: Option<LayerTemporalRange>,
    pub provenance_summary: LayerProvenanceSummary,
    pub updated_at: String,
}

#[derive(SimpleObject, Clone)]
pub struct LayerRecord {
    pub id: ID,
    pub geometry: Json<Value>,
    pub properties: Json<Value>,
    pub confidence: f64,
    pub review_status: ReviewStatus,
    pub visibility: VisibilityLevel,
    pub provenance_summary: LayerProvenanceSummary,
    pub observed_at: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(SimpleObject, Clone)]
pub struct LayerViewSummary {
    pub record_count: i32,
    pub source_count: i32,
    pub min_confidence: f64,
    pub max_confidence: f64,
}

#[derive(SimpleObject, Clone)]
pub struct LayerView {
    pub layer_id: ID,
    pub status: LayerViewStatus,
    pub records: Vec<LayerRecord>,
    pub summary: LayerViewSummary,
    pub generated_at: String,
}

#[derive(SimpleObject, Clone)]
pub struct LayerRecipeSourceRef {
    pub layer_id: Option<ID>,
    pub search_query: Option<String>,
    pub upload_id: Option<ID>,
    pub model_run_id: Option<ID>,
}

#[derive(SimpleObject, Clone)]
pub struct LayerRecipeTransform {
    pub duckdb_sql: Option<String>,
}

#[derive(SimpleObject, Clone)]
pub struct LayerDisplayEncoding {
    pub renderer_boundary_id: String,
    pub deck_gl_layer_type: String,
    pub color_field: Option<String>,
    pub scale_field: Option<String>,
    pub opacity_by_confidence: bool,
}

#[derive(SimpleObject, Clone)]
pub struct LayerProvenancePolicy {
    pub visibility_floor: VisibilityLevel,
    pub ghost_inferred_records: bool,
}

#[derive(SimpleObject, Clone)]
pub struct LayerRecipe {
    pub id: ID,
    pub layer_id: ID,
    pub title: String,
    pub source_ref: LayerRecipeSourceRef,
    pub transform: Option<LayerRecipeTransform>,
    pub display_encoding: LayerDisplayEncoding,
    pub provenance_policy: LayerProvenancePolicy,
    pub updated_at: String,
}

#[derive(InputObject)]
pub struct TimeRangeInput {
    pub start: Option<String>,
    pub end: Option<String>,
}

#[derive(Default)]
pub struct LayerQuery;

#[Object]
impl LayerQuery {
    async fn layers(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = "flint")] tenant_slug: String,
        kinds: Option<Vec<LayerKind>>,
        lifecycle_state: Option<LayerLifecycleState>,
    ) -> async_graphql::Result<Vec<Layer>> {
        let state = state(ctx)?;
        let mut layers = Vec::new();
        // Best-effort catalog assembly: one producer's backing store being
        // unavailable (e.g. no DATABASE_URL for the event planner) must not
        // null the whole catalog. Each producer contributes what it can; an
        // unavailable producer is logged and skipped, mirroring the honest
        // fallback discipline the per-producer detail queries already follow
        // (trafficRealtime returns a fixture fallback instead of erroring).
        // Never fabricate a descriptor for a producer that could not be read.
        match traffic_layer(state).await {
            Ok(layer) => layers.push(layer),
            Err(error) => {
                tracing::warn!(?error, "layers(): traffic producer unavailable; skipping")
            }
        }
        match reconstruction_layer(state).await {
            Ok(layer) => layers.push(layer),
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "layers(): reconstruction producer unavailable; skipping"
                )
            }
        }
        if state.theseus_bridge_url().is_some() {
            layers.push(morphological_layer());
        }
        match event_surface_layers(state, &tenant_slug).await {
            Ok(event_layers) => layers.extend(event_layers),
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "layers(): event-surface producer unavailable; skipping"
                )
            }
        }

        Ok(layers
            .into_iter()
            .filter(|layer| {
                kinds
                    .as_ref()
                    .map_or(true, |wanted| wanted.contains(&layer.kind))
            })
            .filter(|layer| lifecycle_state.map_or(true, |wanted| layer.lifecycle_state == wanted))
            .collect())
    }

    async fn layer_view(
        &self,
        ctx: &Context<'_>,
        layer_id: ID,
        bbox: Option<BboxInput>,
        time_range: Option<TimeRangeInput>,
        #[graphql(default = 0.0)] min_confidence: f64,
    ) -> async_graphql::Result<LayerView> {
        let _ = (bbox, time_range);
        let state = state(ctx)?;
        let id = layer_id.0.as_str();
        if id == TRAFFIC_LAYER_ID || id == "traffic:flint-downtown" {
            return traffic_layer_view(state, min_confidence).await;
        }
        if id == RECONSTRUCTION_LAYER_ID {
            return reconstruction_layer_view(state, min_confidence).await;
        }
        if id == MORPHOLOGICAL_LAYER_ID || id == "morphological-graph:city2graph" {
            return morphological_layer_view(state, min_confidence).await;
        }
        if let Some(event_slug) = id.strip_prefix("layer:event-surface:flint:") {
            return event_surface_layer_view(
                state,
                DEFAULT_TENANT_SLUG,
                event_slug,
                min_confidence,
            )
            .await;
        }

        Ok(empty_layer_view(layer_id, LayerViewStatus::Unavailable))
    }

    async fn layer_recipe(
        &self,
        ctx: &Context<'_>,
        layer_id: ID,
    ) -> async_graphql::Result<Option<LayerRecipe>> {
        let state = state(ctx)?;
        let generated_at = chrono::Utc::now().to_rfc3339();
        let id = layer_id.0.as_str();
        if id == TRAFFIC_LAYER_ID || id == "traffic:flint-downtown" {
            return Ok(Some(traffic_recipe(&generated_at)));
        }
        if id == RECONSTRUCTION_LAYER_ID {
            return Ok(Some(reconstruction_recipe(&generated_at)));
        }
        if id == MORPHOLOGICAL_LAYER_ID || id == "morphological-graph:city2graph" {
            return Ok(Some(morphological_recipe(&generated_at)));
        }
        if id.starts_with("layer:event-surface:flint:") {
            return Ok(Some(event_surface_recipe(layer_id, &generated_at)));
        }

        let known = self
            .layers(ctx, DEFAULT_TENANT_SLUG.to_string(), None, None)
            .await?
            .into_iter()
            .any(|layer| layer.id.0 == id);
        if known {
            Ok(Some(generic_overlay_recipe(layer_id, &generated_at)))
        } else {
            let _ = state;
            Ok(None)
        }
    }
}

fn state<'a>(ctx: &'a Context<'_>) -> async_graphql::Result<&'a AtlasState> {
    ctx.data::<AtlasState>()
        .map_err(|_| async_graphql::Error::new("AtlasState missing from GraphQL context"))
}

async fn traffic_layer(state: &AtlasState) -> async_graphql::Result<Layer> {
    let snapshot = snapshot_for_network(state, TRAFFIC_NETWORK_ID).await?;
    let records = snapshot
        .segments
        .iter()
        .map(traffic_record)
        .collect::<Vec<_>>();

    Ok(Layer {
        id: ID(TRAFFIC_LAYER_ID.to_string()),
        kind: LayerKind::Traffic,
        source_action: LayerSourceAction::Model,
        title: "Flint traffic flow".to_string(),
        lifecycle_state: LayerLifecycleState::Public,
        renderer_boundary_id: "data_overlay".to_string(),
        record_count: snapshot.segments.len() as i32,
        temporal_range: temporal_range_from_records(&records),
        provenance_summary: provenance_summary(&records),
        updated_at: snapshot.generated_at,
    })
}

async fn traffic_layer_view(
    state: &AtlasState,
    min_confidence: f64,
) -> async_graphql::Result<LayerView> {
    let snapshot = snapshot_for_network(state, TRAFFIC_NETWORK_ID).await?;
    let records = snapshot
        .segments
        .iter()
        .map(traffic_record)
        .filter(|record| public_projection(record, min_confidence))
        .collect::<Vec<_>>();

    Ok(LayerView {
        layer_id: ID(TRAFFIC_LAYER_ID.to_string()),
        status: layer_status_from_traffic(snapshot.status),
        summary: view_summary(&records),
        records,
        generated_at: snapshot.generated_at,
    })
}

fn traffic_record(segment: &TrafficSegment) -> LayerRecord {
    let source_count = if segment.source_label.trim().is_empty() {
        0
    } else {
        1
    };
    LayerRecord {
        id: segment.segment_id.clone(),
        geometry: segment.geometry.clone(),
        properties: Json(json!({
            "kind": "traffic",
            "segmentId": segment.segment_id.0.clone(),
            "corridorName": &segment.corridor_name,
            "directionLabel": &segment.direction_label,
            "estimateBasis": traffic_estimate_basis_name(segment),
            "sourceStatus": traffic_source_status_name(segment),
            "sourceLabel": &segment.source_label,
            "supportNote": &segment.support_note,
            "speedMph": segment.speed_mph,
            "freeFlowSpeedMph": segment.free_flow_speed_mph,
            "volumePerHour": segment.volume_per_hour,
            "congestionRatio": segment.congestion_ratio,
        })),
        confidence: segment.confidence,
        review_status: ReviewStatus::Accepted,
        visibility: VisibilityLevel::Public,
        provenance_summary: single_provenance(
            source_count,
            segment.confidence,
            ReviewStatus::Accepted,
        ),
        observed_at: Some(segment.observed_at.clone()),
        expires_at: segment.expires_at.clone(),
    }
}

fn traffic_estimate_basis_name(segment: &TrafficSegment) -> &'static str {
    use crate::graphql::traffic::TrafficEstimateBasis;
    match segment.estimate_basis {
        TrafficEstimateBasis::LiveFeed => "LIVE_FEED",
        TrafficEstimateBasis::HourlyPattern => "HOURLY_PATTERN",
        TrafficEstimateBasis::ScenarioModel => "SCENARIO_MODEL",
    }
}

fn traffic_source_status_name(segment: &TrafficSegment) -> &'static str {
    use crate::graphql::traffic::TrafficSourceStatus;
    match segment.source_status {
        TrafficSourceStatus::Live => "LIVE",
        TrafficSourceStatus::HistoricAverage => "HISTORIC_AVERAGE",
        TrafficSourceStatus::Fixture => "FIXTURE",
        TrafficSourceStatus::PendingLiveSource => "PENDING_LIVE_SOURCE",
    }
}

fn layer_status_from_traffic(status: TrafficFeedStatus) -> LayerViewStatus {
    match status {
        TrafficFeedStatus::Live | TrafficFeedStatus::HistoricAverage => LayerViewStatus::Public,
        TrafficFeedStatus::FixtureFallback => LayerViewStatus::Fixture,
        TrafficFeedStatus::Unavailable => LayerViewStatus::Unavailable,
    }
}

async fn event_surface_layers(
    state: &AtlasState,
    tenant_slug: &str,
) -> async_graphql::Result<Vec<Layer>> {
    let layers = list_event_layers(state, tenant_slug).await?;
    let mut out = Vec::with_capacity(layers.len());

    for layer in layers {
        let placements = list_placements(state, tenant_slug, &layer.slug).await?;
        let records = placements
            .iter()
            .filter_map(|placement| placement_record(placement).ok())
            .collect::<Vec<_>>();
        let updated_at = chrono::Utc::now().to_rfc3339();
        out.push(Layer {
            id: ID(format!("layer:event-surface:{tenant_slug}:{}", layer.slug)),
            kind: LayerKind::EventSurface,
            source_action: LayerSourceAction::Base,
            title: layer.title,
            lifecycle_state: LayerLifecycleState::Public,
            renderer_boundary_id: "data_overlay".to_string(),
            record_count: records.len() as i32,
            temporal_range: Some(LayerTemporalRange {
                start: millis_to_iso(layer.starts_at_ms),
                end: millis_to_iso(layer.ends_at_ms),
            }),
            provenance_summary: provenance_summary(&records),
            updated_at,
        });
    }

    Ok(out)
}

async fn event_surface_layer_view(
    state: &AtlasState,
    tenant_slug: &str,
    event_slug: &str,
    min_confidence: f64,
) -> async_graphql::Result<LayerView> {
    let generated_at = chrono::Utc::now().to_rfc3339();
    let records = list_placements(state, tenant_slug, event_slug)
        .await?
        .into_iter()
        .filter_map(|placement| placement_record(&placement).ok())
        .filter(|record| public_projection(record, min_confidence))
        .collect::<Vec<_>>();

    Ok(LayerView {
        layer_id: ID(format!("layer:event-surface:{tenant_slug}:{event_slug}")),
        status: LayerViewStatus::Public,
        summary: view_summary(&records),
        records,
        generated_at,
    })
}

fn placement_record(
    placement: &civic_atlas_types::event_planner::Placement,
) -> async_graphql::Result<LayerRecord> {
    let geometry = serde_json::from_str::<Value>(&placement.geometry_geojson).map_err(|error| {
        async_graphql::Error::new(format!("placement geometry is invalid GeoJSON: {error}"))
    })?;
    let confidence = if placement.status == "placed" {
        0.9
    } else {
        0.72
    };

    Ok(LayerRecord {
        id: ID(placement.id.clone()),
        geometry: Json(geometry),
        properties: Json(json!({
            "kind": "event_surface",
            "placementId": &placement.id,
            "eventLayerId": &placement.event_layer_id,
            "category": &placement.category,
            "sublabel": empty_to_null(&placement.sublabel),
            "label": &placement.label,
            "status": &placement.status,
            "notes": empty_to_null(&placement.notes),
            "version": placement.version,
        })),
        confidence,
        review_status: ReviewStatus::Accepted,
        visibility: VisibilityLevel::Public,
        provenance_summary: single_provenance(1, confidence, ReviewStatus::Accepted),
        observed_at: millis_to_iso(placement.updated_at_ms),
        expires_at: None,
    })
}

async fn reconstruction_layer(state: &AtlasState) -> async_graphql::Result<Layer> {
    let generated_at = chrono::Utc::now().to_rfc3339();
    let records = list_reconstructions(state)
        .await?
        .iter()
        .map(reconstruction_record)
        .collect::<Vec<_>>();

    Ok(Layer {
        id: ID(RECONSTRUCTION_LAYER_ID.to_string()),
        kind: LayerKind::Reconstruction,
        source_action: LayerSourceAction::Model,
        title: "Lost Flint reconstructions".to_string(),
        lifecycle_state: LayerLifecycleState::Public,
        renderer_boundary_id: "object_scene".to_string(),
        record_count: records.len() as i32,
        temporal_range: temporal_range_from_records(&records),
        provenance_summary: provenance_summary(&records),
        updated_at: generated_at,
    })
}

async fn reconstruction_layer_view(
    state: &AtlasState,
    min_confidence: f64,
) -> async_graphql::Result<LayerView> {
    let generated_at = chrono::Utc::now().to_rfc3339();
    let records = list_reconstructions(state)
        .await?
        .iter()
        .map(reconstruction_record)
        .filter(|record| public_projection(record, min_confidence))
        .collect::<Vec<_>>();

    Ok(LayerView {
        layer_id: ID(RECONSTRUCTION_LAYER_ID.to_string()),
        status: LayerViewStatus::Public,
        summary: view_summary(&records),
        records,
        generated_at,
    })
}

fn reconstruction_record(reconstruction: &HistoricalReconstruction) -> LayerRecord {
    let source_count = reconstruction.sources.len() as i32;
    let geometry = match reconstruction.position.as_ref() {
        Some(position) => {
            let [lng, lat] = position.0;
            json!({ "type": "Point", "coordinates": [lng, lat] })
        }
        None => json!(null),
    };

    LayerRecord {
        id: ID(reconstruction.id.clone()),
        geometry: Json(geometry),
        properties: Json(json!({
            "kind": "reconstruction",
            "civicObjectId": &reconstruction.civic_object_id,
            "name": &reconstruction.name,
            "description": &reconstruction.description,
            "footprint": {
                "widthMeters": reconstruction.footprint.width_meters,
                "depthMeters": reconstruction.footprint.depth_meters,
            },
            "heightMeters": reconstruction.height_meters,
            "bearingDegrees": reconstruction.bearing_degrees,
            "geometryUrl": &reconstruction.geometry_url,
            "geometryFormat": &reconstruction.geometry_format,
            "foundryAssetUrl": &reconstruction.foundry_asset_url,
        })),
        confidence: reconstruction.confidence,
        review_status: ReviewStatus::Accepted,
        visibility: VisibilityLevel::Public,
        provenance_summary: single_provenance(
            source_count,
            reconstruction.confidence,
            ReviewStatus::Accepted,
        ),
        observed_at: reconstruction.time_start.clone(),
        expires_at: reconstruction.time_end.clone(),
    }
}

fn morphological_layer() -> Layer {
    let generated_at = chrono::Utc::now().to_rfc3339();
    Layer {
        id: ID(MORPHOLOGICAL_LAYER_ID.to_string()),
        kind: LayerKind::ModelOutput,
        source_action: LayerSourceAction::Model,
        title: "Flint morphological relation graph".to_string(),
        lifecycle_state: LayerLifecycleState::Candidate,
        renderer_boundary_id: "data_overlay".to_string(),
        record_count: 0,
        temporal_range: None,
        provenance_summary: single_provenance(0, 0.0, ReviewStatus::NeedsReview),
        updated_at: generated_at,
    }
}

async fn morphological_layer_view(
    state: &AtlasState,
    min_confidence: f64,
) -> async_graphql::Result<LayerView> {
    let generated_at = chrono::Utc::now().to_rfc3339();
    let Some(response) = build_morphological_graph(state).await? else {
        return Ok(empty_layer_view(
            ID(MORPHOLOGICAL_LAYER_ID.to_string()),
            LayerViewStatus::Unavailable,
        ));
    };
    let records = response
        .edges
        .iter()
        .map(|edge| morphological_record(edge, &response))
        .filter(|record| public_projection(record, min_confidence))
        .collect::<Vec<_>>();

    Ok(LayerView {
        layer_id: ID(MORPHOLOGICAL_LAYER_ID.to_string()),
        status: LayerViewStatus::ReviewPending,
        summary: view_summary(&records),
        records,
        generated_at,
    })
}

async fn build_morphological_graph(
    state: &AtlasState,
) -> async_graphql::Result<Option<MorphologicalGraphResponse>> {
    let Some(url) = state.theseus_bridge_url() else {
        return Ok(None);
    };
    let tenant_slug = DEFAULT_TENANT_SLUG.to_string();
    let request = MorphologicalGraphRequest {
        tenant_context: Some(tenant_context(&tenant_slug)),
        model_run_id: MORPHOLOGICAL_MODEL_RUN_ID.to_string(),
        places: morphological_places(state, &tenant_slug),
        movements: morphological_movements(state).await?,
        parameters: HashMap::from([
            (
                "producer".to_string(),
                "city2graph.morphological_graph".to_string(),
            ),
            (
                "edge_schema".to_string(),
                "touched_to,connected_to,faced_to".to_string(),
            ),
        ]),
    };
    if request.places.is_empty() || request.movements.is_empty() {
        return Ok(Some(MorphologicalGraphResponse {
            model_run_id: request.model_run_id,
            status: "insufficient_inputs".to_string(),
            edges: Vec::new(),
            model: "city2graph".to_string(),
            model_version: String::new(),
            warnings: vec![
                "morphological graph requires non-null place geometry and street centerlines"
                    .to_string(),
            ],
        }));
    }

    let mut client = theseus_client::TheseusClient::connect(url)
        .await
        .map_err(|error| {
            async_graphql::Error::new(format!("Theseus bridge connect failed: {error}"))
        })?;
    match client
        .bridge()
        .build_morphological_graph(Request::new(request))
        .await
    {
        Ok(response) => Ok(Some(response.into_inner())),
        Err(status) if matches!(status.code(), Code::Unavailable | Code::Unimplemented) => Ok(None),
        Err(status) => Err(graphql_status(status)),
    }
}

fn morphological_places(state: &AtlasState, tenant_slug: &str) -> Vec<MorphologicalPlace> {
    state
        .places_for_tenant(tenant_slug)
        .into_iter()
        .filter(|place| {
            let geometry = place.geometry_json.trim();
            !geometry.is_empty() && geometry != "null"
        })
        .map(|place| MorphologicalPlace {
            place_id: place.id,
            geometry_geojson: place.geometry_json,
            metadata: HashMap::from([
                ("name".to_string(), place.name),
                ("object_type".to_string(), place.object_type),
            ]),
        })
        .collect()
}

async fn morphological_movements(
    state: &AtlasState,
) -> async_graphql::Result<Vec<MorphologicalMovement>> {
    let snapshot = snapshot_for_network(state, TRAFFIC_NETWORK_ID).await?;
    Ok(snapshot
        .segments
        .into_iter()
        .map(|segment| MorphologicalMovement {
            movement_id: segment.segment_id.0,
            geometry_geojson: segment.geometry.0.to_string(),
            metadata: HashMap::from([
                ("corridor_name".to_string(), segment.corridor_name),
                ("direction_label".to_string(), segment.direction_label),
            ]),
        })
        .collect())
}

fn morphological_record(
    edge: &MorphologicalGraphEdge,
    response: &MorphologicalGraphResponse,
) -> LayerRecord {
    let confidence = if edge.confidence > 0.0 {
        edge.confidence.min(1.0)
    } else {
        1.0
    };
    let geometry = if edge.geometry_geojson.trim().is_empty() {
        json!(null)
    } else {
        serde_json::from_str::<Value>(&edge.geometry_geojson).unwrap_or_else(|_| json!(null))
    };
    let id = if edge.edge_id.trim().is_empty() {
        format!(
            "morphological:{}:{}:{}",
            edge.source_id, edge.relation, edge.target_id
        )
    } else {
        edge.edge_id.clone()
    };

    LayerRecord {
        id: ID(id),
        geometry: Json(geometry),
        properties: Json(json!({
            "kind": "morphological_graph",
            "sourceId": &edge.source_id,
            "sourceKind": &edge.source_kind,
            "relation": &edge.relation,
            "targetId": &edge.target_id,
            "targetKind": &edge.target_kind,
            "modelRunId": &response.model_run_id,
            "model": &response.model,
            "modelVersion": empty_to_null(&response.model_version),
            "status": &response.status,
            "properties": &edge.properties,
            "warnings": &response.warnings,
        })),
        confidence,
        review_status: ReviewStatus::Corroborated,
        visibility: VisibilityLevel::Public,
        provenance_summary: single_provenance(1, confidence, ReviewStatus::Corroborated),
        observed_at: None,
        expires_at: None,
    }
}

async fn list_event_layers(
    state: &AtlasState,
    tenant_slug: &str,
) -> async_graphql::Result<Vec<civic_atlas_types::event_planner::EventLayer>> {
    let response = match EventPlannerGrpcService::new(state.clone())
        .list_event_layers(Request::new(EventLayerListRequest {
            tenant_context: Some(tenant_context(tenant_slug)),
        }))
        .await
    {
        Ok(response) => response.into_inner(),
        Err(status) if status.code() == tonic::Code::Unavailable => return Ok(Vec::new()),
        Err(status) => return Err(graphql_status(status)),
    };
    Ok(response.layers)
}

async fn list_placements(
    state: &AtlasState,
    tenant_slug: &str,
    event_slug: &str,
) -> async_graphql::Result<Vec<civic_atlas_types::event_planner::Placement>> {
    let response = match EventPlannerGrpcService::new(state.clone())
        .list_placements(Request::new(PlacementListRequest {
            tenant_context: Some(tenant_context(tenant_slug)),
            event_layer_slug: event_slug.to_string(),
        }))
        .await
    {
        Ok(response) => response.into_inner(),
        Err(status) if status.code() == tonic::Code::Unavailable => return Ok(Vec::new()),
        Err(status) => return Err(graphql_status(status)),
    };
    Ok(response.placements)
}

async fn list_reconstructions(
    state: &AtlasState,
) -> async_graphql::Result<Vec<HistoricalReconstruction>> {
    let tenant_slug = DEFAULT_TENANT_SLUG.to_string();
    let mut request = Request::new(ListReconstructionSpecsRequest {
        tenant_context: Some(tenant_context(&tenant_slug)),
        civic_object_id: String::new(),
        parcel_id: String::new(),
        status: 0,
        page_size: 200,
        page_token: String::new(),
    });
    request.metadata_mut().insert(
        "x-atlas-tenant",
        tenant_slug
            .parse()
            .map_err(|err| async_graphql::Error::new(format!("invalid tenant id: {err}")))?,
    );

    let response = match ReconstructionGrpcService::new(state.clone())
        .list_reconstruction_specs(request)
        .await
    {
        Ok(response) => response.into_inner(),
        Err(status) if status.code() == tonic::Code::Unavailable => return Ok(Vec::new()),
        Err(status) => return Err(graphql_status(status)),
    };

    Ok(response
        .specs
        .iter()
        .map(reconstruction_from_spec)
        .collect())
}

fn tenant_context(tenant_slug: &str) -> TenantContext {
    TenantContext {
        tenant_id: tenant_slug.to_string(),
        atlas_node_id: format!("atlas:{tenant_slug}"),
        metadata: Default::default(),
    }
}

fn graphql_status(status: tonic::Status) -> async_graphql::Error {
    async_graphql::Error::new(format!("{} ({})", status.message(), status.code()))
}

fn empty_to_null(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn millis_to_iso(ms: i64) -> Option<String> {
    if ms <= 0 {
        None
    } else {
        Some(ms_to_iso8601(ms))
    }
}

fn public_projection(record: &LayerRecord, min_confidence: f64) -> bool {
    matches!(record.visibility, VisibilityLevel::Public)
        && matches!(
            record.review_status,
            ReviewStatus::Accepted | ReviewStatus::Corroborated
        )
        && record.confidence >= min_confidence
}

fn empty_layer_view(layer_id: ID, status: LayerViewStatus) -> LayerView {
    LayerView {
        layer_id,
        status,
        records: Vec::new(),
        summary: LayerViewSummary {
            record_count: 0,
            source_count: 0,
            min_confidence: 0.0,
            max_confidence: 0.0,
        },
        generated_at: chrono::Utc::now().to_rfc3339(),
    }
}

fn view_summary(records: &[LayerRecord]) -> LayerViewSummary {
    let provenance = provenance_summary(records);
    LayerViewSummary {
        record_count: records.len() as i32,
        source_count: provenance.source_count,
        min_confidence: provenance.confidence_range.min,
        max_confidence: provenance.confidence_range.max,
    }
}

fn provenance_summary(records: &[LayerRecord]) -> LayerProvenanceSummary {
    if records.is_empty() {
        return single_provenance(0, 0.0, ReviewStatus::Accepted);
    }

    let mut sources = 0;
    let mut min_confidence = f64::MAX;
    let mut max_confidence = f64::MIN;
    let mut counts = BTreeMap::<ReviewStatus, i32>::new();
    for record in records {
        sources += record.provenance_summary.source_count;
        min_confidence = min_confidence.min(record.confidence);
        max_confidence = max_confidence.max(record.confidence);
        *counts.entry(record.review_status).or_insert(0) += 1;
    }

    LayerProvenanceSummary {
        source_count: sources,
        confidence_range: LayerConfidenceRange {
            min: round2(min_confidence),
            max: round2(max_confidence),
        },
        review_status_mix: counts
            .into_iter()
            .map(|(status, count)| LayerReviewStatusCount { status, count })
            .collect(),
    }
}

fn single_provenance(
    source_count: i32,
    confidence: f64,
    status: ReviewStatus,
) -> LayerProvenanceSummary {
    LayerProvenanceSummary {
        source_count,
        confidence_range: LayerConfidenceRange {
            min: round2(confidence),
            max: round2(confidence),
        },
        review_status_mix: vec![LayerReviewStatusCount { status, count: 1 }],
    }
}

fn temporal_range_from_records(records: &[LayerRecord]) -> Option<LayerTemporalRange> {
    let starts = records
        .iter()
        .filter_map(|record| record.observed_at.as_ref())
        .cloned()
        .collect::<BTreeSet<_>>();
    let ends = records
        .iter()
        .filter_map(|record| record.expires_at.as_ref())
        .cloned()
        .collect::<BTreeSet<_>>();
    if starts.is_empty() && ends.is_empty() {
        return None;
    }
    Some(LayerTemporalRange {
        start: starts.first().cloned(),
        end: ends.last().cloned(),
    })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn traffic_recipe(updated_at: &str) -> LayerRecipe {
    LayerRecipe {
        id: ID("recipe:traffic:flint-downtown".to_string()),
        layer_id: ID(TRAFFIC_LAYER_ID.to_string()),
        title: "Traffic deck.gl LineString recipe".to_string(),
        source_ref: LayerRecipeSourceRef {
            layer_id: Some(ID(TRAFFIC_LAYER_ID.to_string())),
            search_query: None,
            upload_id: None,
            model_run_id: Some(ID("model-run:traffic:historic-average".to_string())),
        },
        transform: Some(LayerRecipeTransform {
            duckdb_sql: Some("SELECT * FROM layer_records WHERE confidence >= ?".to_string()),
        }),
        display_encoding: LayerDisplayEncoding {
            renderer_boundary_id: "data_overlay".to_string(),
            deck_gl_layer_type: "GeoJsonLayer".to_string(),
            color_field: Some("congestionRatio".to_string()),
            scale_field: Some("volumePerHour".to_string()),
            opacity_by_confidence: true,
        },
        provenance_policy: LayerProvenancePolicy {
            visibility_floor: VisibilityLevel::Public,
            ghost_inferred_records: true,
        },
        updated_at: updated_at.to_string(),
    }
}

fn reconstruction_recipe(updated_at: &str) -> LayerRecipe {
    LayerRecipe {
        id: ID("recipe:reconstruction:flint:historical".to_string()),
        layer_id: ID(RECONSTRUCTION_LAYER_ID.to_string()),
        title: "Lost Flint object-scene recipe".to_string(),
        source_ref: LayerRecipeSourceRef {
            layer_id: Some(ID(RECONSTRUCTION_LAYER_ID.to_string())),
            search_query: None,
            upload_id: None,
            model_run_id: Some(ID("model-run:reconstruction:scene-foundry".to_string())),
        },
        transform: None,
        display_encoding: LayerDisplayEncoding {
            renderer_boundary_id: "object_scene".to_string(),
            deck_gl_layer_type: "ScenegraphLayer".to_string(),
            color_field: Some("confidence".to_string()),
            scale_field: Some("heightMeters".to_string()),
            opacity_by_confidence: true,
        },
        provenance_policy: LayerProvenancePolicy {
            visibility_floor: VisibilityLevel::Public,
            ghost_inferred_records: true,
        },
        updated_at: updated_at.to_string(),
    }
}

fn morphological_recipe(updated_at: &str) -> LayerRecipe {
    LayerRecipe {
        id: ID("recipe:morphological-graph:flint:city2graph".to_string()),
        layer_id: ID(MORPHOLOGICAL_LAYER_ID.to_string()),
        title: "City2Graph morphological relation recipe".to_string(),
        source_ref: LayerRecipeSourceRef {
            layer_id: Some(ID(MORPHOLOGICAL_LAYER_ID.to_string())),
            search_query: None,
            upload_id: None,
            model_run_id: Some(ID(MORPHOLOGICAL_MODEL_RUN_ID.to_string())),
        },
        transform: Some(LayerRecipeTransform {
            duckdb_sql: Some(
                "SELECT * FROM layer_records WHERE relation IN ('touched_to', 'connected_to', 'faced_to')".to_string(),
            ),
        }),
        display_encoding: LayerDisplayEncoding {
            renderer_boundary_id: "data_overlay".to_string(),
            deck_gl_layer_type: "GeoJsonLayer".to_string(),
            color_field: Some("relation".to_string()),
            scale_field: None,
            opacity_by_confidence: true,
        },
        provenance_policy: LayerProvenancePolicy {
            visibility_floor: VisibilityLevel::Public,
            ghost_inferred_records: true,
        },
        updated_at: updated_at.to_string(),
    }
}

fn event_surface_recipe(layer_id: ID, updated_at: &str) -> LayerRecipe {
    LayerRecipe {
        id: ID(format!(
            "recipe:{}",
            layer_id.0.trim_start_matches("layer:")
        )),
        title: "Event surface placement recipe".to_string(),
        source_ref: LayerRecipeSourceRef {
            layer_id: Some(layer_id.clone()),
            search_query: None,
            upload_id: None,
            model_run_id: None,
        },
        layer_id,
        transform: None,
        display_encoding: LayerDisplayEncoding {
            renderer_boundary_id: "data_overlay".to_string(),
            deck_gl_layer_type: "GeoJsonLayer".to_string(),
            color_field: Some("category".to_string()),
            scale_field: None,
            opacity_by_confidence: true,
        },
        provenance_policy: LayerProvenancePolicy {
            visibility_floor: VisibilityLevel::Public,
            ghost_inferred_records: true,
        },
        updated_at: updated_at.to_string(),
    }
}

fn generic_overlay_recipe(layer_id: ID, updated_at: &str) -> LayerRecipe {
    LayerRecipe {
        id: ID(format!(
            "recipe:{}",
            layer_id.0.trim_start_matches("layer:")
        )),
        title: "Generic civic layer recipe".to_string(),
        source_ref: LayerRecipeSourceRef {
            layer_id: Some(layer_id.clone()),
            search_query: None,
            upload_id: None,
            model_run_id: None,
        },
        layer_id,
        transform: None,
        display_encoding: LayerDisplayEncoding {
            renderer_boundary_id: "data_overlay".to_string(),
            deck_gl_layer_type: "GeoJsonLayer".to_string(),
            color_field: Some("confidence".to_string()),
            scale_field: None,
            opacity_by_confidence: true,
        },
        provenance_policy: LayerProvenancePolicy {
            visibility_floor: VisibilityLevel::Public,
            ghost_inferred_records: true,
        },
        updated_at: updated_at.to_string(),
    }
}
