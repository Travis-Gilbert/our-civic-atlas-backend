pub mod corrections;
pub mod fixture;
pub mod reconstruction;
pub mod tenant_db;

use std::{env, net::SocketAddr, sync::Arc};

use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use civic_atlas_types::civic_atlas::v1::spacetime_atlas_service_server::SpacetimeAtlasService as SpacetimeAtlasGrpc;
use civic_atlas_types::civic_atlas::v1::{
    civic_atlas_service_server::CivicAtlasService, CivicObject, GetBlockSubgraphRequest,
    GetBlockSubgraphResponse, GetDossierRequest, GetDossierResponse, GetNearbyArtifactsRequest,
    GetNearbyArtifactsResponse, GetNodeRequest, GetNodeResponse, GetParcelHistoryRequest,
    GetParcelHistoryResponse, GetPlaceRequest, GetPlaceResponse, GetViewportAtTimeRequest,
    GetViewportAtTimeResponse, HealthRequest, HealthResponse, ListPlacesRequest,
    ListPlacesResponse, ResolveTenantRequest, ResolveTenantResponse, TenantContext, TimeSlice,
    ViewportBounds,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tenant_resolver::require_tenant_context;
use tonic::{Request, Response, Status};
use tracing::warn;

#[derive(Clone)]
pub struct AtlasState {
    places: Arc<Vec<CivicObject>>,
    db: Option<PgPool>,
}

impl AtlasState {
    pub fn from_env() -> anyhow::Result<Self> {
        let tenant_id =
            env::var("CIVIC_ATLAS_DEFAULT_TENANT").unwrap_or_else(|_| "flint".to_string());
        let places = match env::var("CIVIC_ATLAS_PLACES_FIXTURE") {
            Ok(path) => fixture::load_places_from_geojson(path, &tenant_id)?,
            Err(_) => fixture::seed_places(&tenant_id),
        };
        let db = match env::var("DATABASE_URL") {
            Ok(url) if !url.trim().is_empty() => {
                match PgPoolOptions::new().max_connections(5).connect_lazy(&url) {
                    Ok(pool) => Some(pool),
                    Err(error) => {
                        warn!(%error, "DATABASE_URL could not initialize; DB-backed methods disabled");
                        None
                    }
                }
            }
            _ => None,
        };
        Ok(Self {
            places: Arc::new(places),
            db,
        })
    }

    pub fn places_for_tenant(&self, tenant_id: &str) -> Vec<CivicObject> {
        self.places
            .iter()
            .filter(|place| place.tenant_id == tenant_id)
            .cloned()
            .collect()
    }

    pub fn db_pool(&self) -> Option<&PgPool> {
        self.db.as_ref()
    }
}

#[derive(Clone)]
pub struct CivicAtlasGrpcService {
    state: AtlasState,
}

impl CivicAtlasGrpcService {
    pub fn new(state: AtlasState) -> Self {
        Self { state }
    }
}

#[derive(Clone)]
pub struct SpacetimeAtlasGrpcService {
    state: AtlasState,
}

impl SpacetimeAtlasGrpcService {
    pub fn new(state: AtlasState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl CivicAtlasService for CivicAtlasGrpcService {
    async fn get_place(
        &self,
        request: Request<GetPlaceRequest>,
    ) -> Result<Response<GetPlaceResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let place = self
            .state
            .places_for_tenant(tenant.as_str())
            .into_iter()
            .find(|place| place.id == request.place_id)
            .ok_or_else(|| Status::not_found("place not found"))?;
        Ok(Response::new(GetPlaceResponse { place: Some(place) }))
    }

    async fn list_places(
        &self,
        request: Request<ListPlacesRequest>,
    ) -> Result<Response<ListPlacesResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let page_size = request.page_size.max(1) as usize;
        let places = self
            .state
            .places_for_tenant(tenant.as_str())
            .into_iter()
            .take(page_size)
            .collect();
        Ok(Response::new(ListPlacesResponse {
            places,
            next_page_token: String::new(),
        }))
    }

    async fn get_node(
        &self,
        request: Request<GetNodeRequest>,
    ) -> Result<Response<GetNodeResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let node = self
            .state
            .places_for_tenant(tenant.as_str())
            .into_iter()
            .find(|place| place.id == request.node_id)
            .ok_or_else(|| Status::not_found("node not found"))?;
        Ok(Response::new(GetNodeResponse { node: Some(node) }))
    }

    async fn get_dossier(
        &self,
        request: Request<GetDossierRequest>,
    ) -> Result<Response<GetDossierResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let object = self
            .state
            .places_for_tenant(tenant.as_str())
            .into_iter()
            .find(|place| place.id == request.object_id)
            .ok_or_else(|| Status::not_found("dossier object not found"))?;
        let dossier_json = json!({
            "object_id": object.id,
            "tenant_id": tenant.as_str(),
            "title": object.name,
            "sources": object.source_ids,
        })
        .to_string();
        Ok(Response::new(GetDossierResponse { dossier_json }))
    }

    async fn resolve_tenant(
        &self,
        request: Request<ResolveTenantRequest>,
    ) -> Result<Response<ResolveTenantResponse>, Status> {
        let slug = request.into_inner().slug;
        let tenant_id = tenant_resolver::TenantId::parse(slug.clone())
            .map_err(|err| Status::invalid_argument(err.to_string()))?
            .as_str()
            .to_string();
        Ok(Response::new(ResolveTenantResponse {
            tenant_context: Some(TenantContext {
                tenant_id,
                atlas_node_id: format!("atlas:{slug}"),
                metadata: Default::default(),
            }),
            display_name: slug.replace('-', " "),
        }))
    }

    async fn health(
        &self,
        request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        Ok(Response::new(HealthResponse {
            status: "ok".to_string(),
            service: "civic-atlas-server".to_string(),
            tenant_id: tenant.as_str().to_string(),
        }))
    }
}

#[tonic::async_trait]
impl SpacetimeAtlasGrpc for SpacetimeAtlasGrpcService {
    async fn get_viewport_at_time(
        &self,
        request: Request<GetViewportAtTimeRequest>,
    ) -> Result<Response<GetViewportAtTimeResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let bounds = request
            .bounds
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("bounds are required"))?;
        let limit = request.limit.max(1) as usize;
        let objects = self
            .state
            .places_for_tenant(tenant.as_str())
            .into_iter()
            .filter(|object| object_intersects_bounds(object, bounds))
            .filter(|object| object_matches_time(object, request.time.as_ref()))
            .take(limit)
            .collect();
        Ok(Response::new(GetViewportAtTimeResponse { objects }))
    }

    async fn get_block_subgraph(
        &self,
        request: Request<GetBlockSubgraphRequest>,
    ) -> Result<Response<GetBlockSubgraphResponse>, Status> {
        let request = request.into_inner();
        require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.block_id.trim().is_empty() {
            return Err(Status::invalid_argument("block_id is required"));
        }
        Ok(Response::new(GetBlockSubgraphResponse {
            nodes: Vec::new(),
            artifact_anchors: Vec::new(),
        }))
    }

    async fn get_parcel_history(
        &self,
        request: Request<GetParcelHistoryRequest>,
    ) -> Result<Response<GetParcelHistoryResponse>, Status> {
        let request = request.into_inner();
        require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.parcel_id.trim().is_empty() {
            return Err(Status::invalid_argument("parcel_id is required"));
        }
        Ok(Response::new(GetParcelHistoryResponse {
            events: Vec::new(),
        }))
    }

    async fn get_nearby_artifacts(
        &self,
        request: Request<GetNearbyArtifactsRequest>,
    ) -> Result<Response<GetNearbyArtifactsResponse>, Status> {
        let request = request.into_inner();
        require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.radius_km <= 0.0 {
            return Err(Status::invalid_argument("radius_km must be positive"));
        }
        Ok(Response::new(GetNearbyArtifactsResponse {
            artifacts: Vec::new(),
        }))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListPlacesJsonRequest {
    tenant_context: TenantContextJson,
    page_size: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetViewportAtTimeJsonRequest {
    tenant_context: TenantContextJson,
    bounds: ViewportBoundsJson,
    time: Option<TimeSliceJson>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TenantContextJson {
    tenant_id: String,
    atlas_node_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ViewportBoundsJson {
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimeSliceJson {
    at_ms: Option<i64>,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListPlacesJsonResponse {
    places: Vec<Value>,
    next_page_token: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GetViewportAtTimeJsonResponse {
    objects: Vec<Value>,
}

pub fn http_router(state: AtlasState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/civic_atlas.v1.CivicAtlasService/ListPlaces",
            axum::routing::post(list_places_json),
        )
        .route(
            "/spacetime-atlas/v1/GetViewportAtTime",
            axum::routing::post(get_viewport_at_time_json),
        )
        .with_state(state)
}

async fn healthz() -> Json<Value> {
    Json(json!({"status": "ok", "service": "civic-atlas-server"}))
}

async fn list_places_json(
    State(state): State<AtlasState>,
    Json(request): Json<ListPlacesJsonRequest>,
) -> Result<Json<ListPlacesJsonResponse>, (StatusCode, Json<Value>)> {
    let tenant =
        tenant_resolver::TenantId::parse(request.tenant_context.tenant_id).map_err(|err| {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": err.to_string()})),
            )
        })?;
    let _atlas_node_id = request
        .tenant_context
        .atlas_node_id
        .as_deref()
        .unwrap_or("");
    let limit = request.page_size.unwrap_or(500).max(1) as usize;
    let places = state
        .places_for_tenant(tenant.as_str())
        .into_iter()
        .take(limit)
        .map(civic_object_json)
        .collect();
    Ok(Json(ListPlacesJsonResponse {
        places,
        next_page_token: String::new(),
    }))
}

async fn get_viewport_at_time_json(
    State(state): State<AtlasState>,
    Json(request): Json<GetViewportAtTimeJsonRequest>,
) -> Result<Json<GetViewportAtTimeJsonResponse>, (StatusCode, Json<Value>)> {
    let tenant =
        tenant_resolver::TenantId::parse(request.tenant_context.tenant_id).map_err(|err| {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": err.to_string()})),
            )
        })?;
    let _atlas_node_id = request
        .tenant_context
        .atlas_node_id
        .as_deref()
        .unwrap_or("");
    let bounds = request.bounds.into_proto();
    let time = request.time.map(TimeSliceJson::into_proto);
    let limit = request.limit.unwrap_or(500).max(1) as usize;
    let objects = state
        .places_for_tenant(tenant.as_str())
        .into_iter()
        .filter(|object| object_intersects_bounds(object, &bounds))
        .filter(|object| object_matches_time(object, time.as_ref()))
        .take(limit)
        .map(civic_object_json)
        .collect();
    Ok(Json(GetViewportAtTimeJsonResponse { objects }))
}

fn civic_object_json(object: CivicObject) -> Value {
    json!({
        "id": object.id,
        "tenantId": object.tenant_id,
        "name": object.name,
        "objectType": object.object_type,
        "geometryJson": object.geometry_json,
        "timeStartMs": object.time_start_ms,
        "timeEndMs": object.time_end_ms,
        "confidence": object.confidence,
        "sourceIds": object.source_ids,
        "dossierPath": object.dossier_path,
        "attributes": object.attributes,
    })
}

impl ViewportBoundsJson {
    fn into_proto(self) -> ViewportBounds {
        ViewportBounds {
            min_lat: self.min_lat,
            min_lon: self.min_lon,
            max_lat: self.max_lat,
            max_lon: self.max_lon,
        }
    }
}

impl TimeSliceJson {
    fn into_proto(self) -> TimeSlice {
        TimeSlice {
            at_ms: self.at_ms,
            start_ms: self.start_ms,
            end_ms: self.end_ms,
        }
    }
}

fn object_matches_time(object: &CivicObject, time: Option<&TimeSlice>) -> bool {
    let Some(time) = time else {
        return true;
    };
    if time.at_ms.is_none() && time.start_ms.is_none() && time.end_ms.is_none() {
        return true;
    }
    if object.time_start_ms.is_none() && object.time_end_ms.is_none() {
        return false;
    }
    if let Some(at_ms) = time.at_ms {
        return interval_contains(object.time_start_ms, object.time_end_ms, at_ms);
    }
    intervals_overlap(
        object.time_start_ms,
        object.time_end_ms,
        time.start_ms,
        time.end_ms,
    )
}

fn interval_contains(start_ms: Option<i64>, end_ms: Option<i64>, at_ms: i64) -> bool {
    start_ms.map(|start| at_ms >= start).unwrap_or(true)
        && end_ms.map(|end| at_ms <= end).unwrap_or(true)
}

fn intervals_overlap(
    left_start: Option<i64>,
    left_end: Option<i64>,
    right_start: Option<i64>,
    right_end: Option<i64>,
) -> bool {
    let left_start = left_start.unwrap_or(i64::MIN);
    let left_end = left_end.unwrap_or(i64::MAX);
    let right_start = right_start.unwrap_or(i64::MIN);
    let right_end = right_end.unwrap_or(i64::MAX);
    left_start <= right_end && right_start <= left_end
}

fn object_intersects_bounds(object: &CivicObject, bounds: &ViewportBounds) -> bool {
    serde_json::from_str::<Value>(&object.geometry_json)
        .ok()
        .map(|geometry| value_has_coordinate_in_bounds(&geometry, bounds))
        .unwrap_or(false)
}

fn value_has_coordinate_in_bounds(value: &Value, bounds: &ViewportBounds) -> bool {
    match value {
        Value::Array(values) if values.len() >= 2 => {
            let lon = values.first().and_then(Value::as_f64);
            let lat = values.get(1).and_then(Value::as_f64);
            if let (Some(lon), Some(lat)) = (lon, lat) {
                return lat >= bounds.min_lat
                    && lat <= bounds.max_lat
                    && lon >= bounds.min_lon
                    && lon <= bounds.max_lon;
            }
            values
                .iter()
                .any(|item| value_has_coordinate_in_bounds(item, bounds))
        }
        Value::Array(values) => values
            .iter()
            .any(|item| value_has_coordinate_in_bounds(item, bounds)),
        Value::Object(map) => map
            .values()
            .any(|item| value_has_coordinate_in_bounds(item, bounds)),
        _ => false,
    }
}

pub fn parse_addr(value: Option<String>, fallback: &str) -> anyhow::Result<SocketAddr> {
    Ok(value.unwrap_or_else(|| fallback.to_string()).parse()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_seed_is_tenant_scoped() {
        let state = AtlasState {
            places: Arc::new(fixture::seed_places("flint")),
            db: None,
        };

        assert_eq!(state.places_for_tenant("flint").len(), 3);
        assert!(state.places_for_tenant("test-city").is_empty());
    }

    #[test]
    fn time_slice_rejects_objects_without_intervals() {
        let object = CivicObject {
            id: "place:1".to_string(),
            tenant_id: "flint".to_string(),
            name: "Place".to_string(),
            object_type: "Place".to_string(),
            geometry_json: "{}".to_string(),
            time_start_ms: None,
            time_end_ms: None,
            confidence: 1.0,
            source_ids: Vec::new(),
            dossier_path: String::new(),
            attributes: Default::default(),
        };

        assert!(!object_matches_time(
            &object,
            Some(&TimeSlice {
                at_ms: Some(0),
                start_ms: None,
                end_ms: None,
            })
        ));
    }

    #[test]
    fn bounds_match_geojson_coordinate_pairs() {
        let object = CivicObject {
            id: "place:1".to_string(),
            tenant_id: "flint".to_string(),
            name: "Place".to_string(),
            object_type: "Place".to_string(),
            geometry_json: json!({
                "type": "Point",
                "coordinates": [-83.7, 43.02],
            })
            .to_string(),
            time_start_ms: Some(0),
            time_end_ms: Some(10),
            confidence: 1.0,
            source_ids: Vec::new(),
            dossier_path: String::new(),
            attributes: Default::default(),
        };

        assert!(object_intersects_bounds(
            &object,
            &ViewportBounds {
                min_lat: 43.0,
                min_lon: -83.8,
                max_lat: 43.1,
                max_lon: -83.6,
            }
        ));
    }
}
