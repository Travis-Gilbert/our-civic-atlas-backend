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
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use tenant_resolver::require_tenant_context;
use tonic::{Request, Response, Status};
use tracing::warn;
use uuid::Uuid;

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
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.block_id.trim().is_empty() {
            return Err(Status::invalid_argument("block_id is required"));
        }
        // Read approved reconstruction parts for the block from PostGIS.
        // The block is identified by reconstruction_specs.block_id (a text
        // column populated when a spec is authored against a block). We
        // gather:
        //   - buildings whose latest approved spec is in this block
        //   - building_parts of those buildings
        //   - artifact_anchors attached to those buildings or parts
        // Each is returned as a CivicObject with object_type set so the
        // caller can dispatch.
        match self.state.db_pool() {
            Some(pool) => {
                let depth = request.depth;
                let nodes = fetch_block_subgraph_nodes(pool, tenant.as_str(), &request.block_id, depth)
                    .await?;
                let artifact_anchors =
                    fetch_block_subgraph_artifact_anchors(pool, tenant.as_str(), &request.block_id)
                        .await?;
                Ok(Response::new(GetBlockSubgraphResponse {
                    nodes,
                    artifact_anchors,
                }))
            }
            None => {
                // Without DATABASE_URL the server runs against the fixture
                // dataset (CIVIC_ATLAS_PLACES_FIXTURE). Return the legacy
                // empty-response so existing smoke tests keep passing.
                Ok(Response::new(GetBlockSubgraphResponse {
                    nodes: Vec::new(),
                    artifact_anchors: Vec::new(),
                }))
            }
        }
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

// --- PostGIS-backed helpers for SpacetimeAtlasService ---------------------

async fn fetch_block_subgraph_nodes(
    pool: &PgPool,
    tenant_slug: &str,
    block_id: &str,
    depth: u32,
) -> Result<Vec<CivicObject>, Status> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let tenant_id = resolve_tenant_uuid(&mut tx, tenant_slug).await?;
    set_tx_tenant_uuid(&mut tx, tenant_id).await?;

    // Buildings in the block come from reconstruction_specs.block_id. A
    // building "belongs to" the block if its latest approved spec
    // references that block. depth=1 returns those buildings and their
    // parts. depth>=2 expands to neighbor buildings (TODO: implement
    // neighbor logic via parcels.geom ST_DWithin; out of scope for
    // the Phase 4 gate).
    let _ = depth; // depth>=2 expansion pending RustyRed integration

    let buildings_rows = sqlx::query(
        r#"
        SELECT DISTINCT b.id, b.civic_object_id, b.t_start_ms, b.t_end_ms,
                        ST_AsGeoJSON(b.geom) AS geometry_json,
                        b.properties
        FROM buildings b
        INNER JOIN reconstruction_specs rs
          ON rs.tenant_id = b.tenant_id
         AND rs.building_id = b.id
        WHERE b.tenant_id = $1
          AND rs.block_id = $2
          AND rs.status = 'approved'
        "#,
    )
    .bind(tenant_id)
    .bind(block_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;

    let mut nodes: Vec<CivicObject> = Vec::with_capacity(buildings_rows.len() * 4);
    let mut building_uuids: Vec<Uuid> = Vec::with_capacity(buildings_rows.len());
    for row in &buildings_rows {
        let id: Uuid = row.try_get("id").map_err(map_db_error)?;
        let civic_object_id: String = row.try_get("civic_object_id").unwrap_or_default();
        let t_start: Option<i64> = row.try_get("t_start_ms").unwrap_or(None);
        let t_end: Option<i64> = row.try_get("t_end_ms").unwrap_or(None);
        let geometry_json: Option<String> = row.try_get("geometry_json").unwrap_or_default();
        building_uuids.push(id);
        nodes.push(CivicObject {
            id: id.to_string(),
            tenant_id: tenant_slug.to_string(),
            name: civic_object_id.clone(),
            object_type: "building".to_string(),
            geometry_json: geometry_json.unwrap_or_default(),
            time_start_ms: t_start,
            time_end_ms: t_end,
            confidence: 1.0,
            source_ids: Vec::new(),
            dossier_path: format!("/dossier/building/{}", civic_object_id),
            attributes: Default::default(),
        });
    }

    // Building parts of those buildings.
    if !building_uuids.is_empty() {
        let part_rows = sqlx::query(
            r#"
            SELECT bp.id, bp.building_id, bp.part_key, bp.part_type,
                   bp.confidence, bp.source_ids,
                   ST_AsGeoJSON(bp.geom) AS geometry_json,
                   bp.t_start_ms, bp.t_end_ms
            FROM building_parts bp
            WHERE bp.tenant_id = $1
              AND bp.building_id = ANY($2)
            ORDER BY bp.building_id, bp.part_key
            "#,
        )
        .bind(tenant_id)
        .bind(&building_uuids[..])
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db_error)?;

        for row in &part_rows {
            let id: Uuid = row.try_get("id").map_err(map_db_error)?;
            let building_id: Uuid = row.try_get("building_id").unwrap_or_default();
            let part_key: String = row.try_get("part_key").unwrap_or_default();
            let part_type: String = row.try_get("part_type").unwrap_or_default();
            let confidence: f64 = row.try_get("confidence").unwrap_or(0.0);
            let source_ids: Vec<String> = row.try_get("source_ids").unwrap_or_default();
            let geometry_json: Option<String> = row.try_get("geometry_json").unwrap_or_default();
            let t_start: Option<i64> = row.try_get("t_start_ms").unwrap_or(None);
            let t_end: Option<i64> = row.try_get("t_end_ms").unwrap_or(None);
            nodes.push(CivicObject {
                id: id.to_string(),
                tenant_id: tenant_slug.to_string(),
                name: format!("{}::{}", building_id, part_key),
                object_type: format!("building_part::{part_type}"),
                geometry_json: geometry_json.unwrap_or_default(),
                time_start_ms: t_start,
                time_end_ms: t_end,
                confidence,
                source_ids,
                dossier_path: format!("/dossier/building_part/{}", id),
                attributes: Default::default(),
            });
        }
    }

    tx.commit().await.map_err(map_db_error)?;
    Ok(nodes)
}

async fn fetch_block_subgraph_artifact_anchors(
    pool: &PgPool,
    tenant_slug: &str,
    block_id: &str,
) -> Result<Vec<CivicObject>, Status> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let tenant_id = resolve_tenant_uuid(&mut tx, tenant_slug).await?;
    set_tx_tenant_uuid(&mut tx, tenant_id).await?;

    // Artifact anchors are scoped to buildings (or their parts) that
    // belong to this block via the approved-spec linkage.
    let rows = sqlx::query(
        r#"
        SELECT aa.id, aa.artifact_id, aa.anchor_kind,
               ST_AsGeoJSON(aa.geom) AS geometry_json,
               aa.t_start_ms, aa.t_end_ms,
               a.title, a.source_type
        FROM artifact_anchors aa
        INNER JOIN artifacts a
          ON a.tenant_id = aa.tenant_id AND a.id = aa.artifact_id
        WHERE aa.tenant_id = $1
          AND (
               aa.building_id IN (
                  SELECT DISTINCT b.id FROM buildings b
                  INNER JOIN reconstruction_specs rs
                    ON rs.tenant_id = b.tenant_id AND rs.building_id = b.id
                  WHERE b.tenant_id = $1
                    AND rs.block_id = $2
                    AND rs.status = 'approved'
               )
            OR aa.building_part_id IN (
                  SELECT DISTINCT bp.id FROM building_parts bp
                  INNER JOIN buildings b
                    ON b.tenant_id = bp.tenant_id AND b.id = bp.building_id
                  INNER JOIN reconstruction_specs rs
                    ON rs.tenant_id = b.tenant_id AND rs.building_id = b.id
                  WHERE bp.tenant_id = $1
                    AND rs.block_id = $2
                    AND rs.status = 'approved'
               )
          )
        "#,
    )
    .bind(tenant_id)
    .bind(block_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;

    let mut anchors: Vec<CivicObject> = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: Uuid = row.try_get("id").map_err(map_db_error)?;
        let artifact_id: Uuid = row.try_get("artifact_id").unwrap_or_default();
        let anchor_kind: String = row.try_get("anchor_kind").unwrap_or_default();
        let title: String = row.try_get("title").unwrap_or_default();
        let source_type: String = row.try_get("source_type").unwrap_or_default();
        let geometry_json: Option<String> = row.try_get("geometry_json").unwrap_or_default();
        let t_start: Option<i64> = row.try_get("t_start_ms").unwrap_or(None);
        let t_end: Option<i64> = row.try_get("t_end_ms").unwrap_or(None);

        let mut attributes = std::collections::HashMap::new();
        attributes.insert("anchor_kind".to_string(), anchor_kind);
        attributes.insert("source_type".to_string(), source_type);
        attributes.insert("artifact_id".to_string(), artifact_id.to_string());

        anchors.push(CivicObject {
            id: id.to_string(),
            tenant_id: tenant_slug.to_string(),
            name: title,
            object_type: "artifact_anchor".to_string(),
            geometry_json: geometry_json.unwrap_or_default(),
            time_start_ms: t_start,
            time_end_ms: t_end,
            confidence: 0.0,
            source_ids: vec![artifact_id.to_string()],
            dossier_path: format!("/dossier/artifact/{}", artifact_id),
            attributes,
        });
    }

    tx.commit().await.map_err(map_db_error)?;
    Ok(anchors)
}

fn map_db_error(error: sqlx::Error) -> Status {
    Status::internal(format!("database error: {error}"))
}

async fn resolve_tenant_uuid(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_slug: &str,
) -> Result<Uuid, Status> {
    let row = sqlx::query("SELECT id FROM tenants WHERE slug = $1")
        .bind(tenant_slug)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_db_error)?;
    row.and_then(|row| row.try_get::<Uuid, _>("id").ok())
        .ok_or_else(|| Status::unauthenticated(format!("unknown tenant: {tenant_slug}")))
}

async fn set_tx_tenant_uuid(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: Uuid,
) -> Result<(), Status> {
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(map_db_error)?;
    Ok(())
}
