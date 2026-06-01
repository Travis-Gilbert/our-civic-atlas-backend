pub mod corrections;
pub mod event_planner;
pub mod event_planner_auth;
pub mod fixture;
pub mod graphql;
pub mod reconstruction;
pub mod tenant_db;
pub mod validation;

use std::{env, net::SocketAddr, sync::Arc};

use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use civic_atlas_types::civic_atlas::v1::spacetime_atlas_service_server::SpacetimeAtlasService as SpacetimeAtlasGrpc;
use civic_atlas_types::civic_atlas::v1::{
    civic_atlas_service_server::CivicAtlasService, CivicObject, CivicResearchRequest,
    CivicResearchResponse, GetBlockSubgraphRequest, GetBlockSubgraphResponse, GetDossierRequest,
    GetDossierResponse, GetNearbyArtifactsRequest, GetNearbyArtifactsResponse, GetNodeRequest,
    GetNodeResponse, GetParcelHistoryRequest, GetParcelHistoryResponse, GetPlaceRequest,
    GetPlaceResponse, GetViewportAtTimeRequest, GetViewportAtTimeResponse, HealthRequest,
    HealthResponse, ListPlacesRequest, ListPlacesResponse, PersistArtifactRequest,
    PersistArtifactResponse, ResolveTenantRequest, ResolveTenantResponse, TenantContext, TimeSlice,
    ViewportBounds,
};
use rustyred_client::{
    Client as RustyRedClient, FullTextSearchRequest, RustyRedError, SpatialBboxRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, types::Json as SqlJson, PgPool, Row};
use std::collections::HashSet;
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

    /// Apply embedded SQL migrations against the live PostGIS pool.
    ///
    /// Called once at server startup from `main.rs` after `from_env`.
    /// No-op when `DATABASE_URL` is unset (the server falls back to
    /// fixture mode in that case).
    ///
    /// Migration files live at `<workspace_root>/migrations/`. They
    /// are embedded into the binary at compile time by the
    /// `sqlx::migrate!` macro so the runtime image doesn't need to
    /// ship the .sql files separately. sqlx tracks applied versions
    /// in the `_sqlx_migrations` table inside the same database.
    pub async fn run_migrations(&self) -> anyhow::Result<()> {
        let Some(pool) = self.db.as_ref() else {
            tracing::info!(
                "DATABASE_URL not set; skipping migrations (server runs in fixture mode)",
            );
            return Ok(());
        };
        tracing::info!("running embedded migrations");
        sqlx::migrate!("../../migrations").run(pool).await?;
        tracing::info!("migrations applied");
        Ok(())
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

    /// Civic research. Reads the Civic Atlas RustyRed GraphDatabase
    /// directly (full-text search, plus an optional spatial bounding-box
    /// intersection) via the `rustyred-client` HTTP client. Does NOT call
    /// Theorem/Theseus: Theorem is reserved for the epistemic enrichment
    /// slice (new_evidence / gap_closures / provenance), which stays
    /// honestly empty here and is named in the result_json comments as the
    /// next slice. Backs the Node sidecar's `Mutation.civicResearch`
    /// GraphQL resolver; the wire contract (`results_json` camelCase keys)
    /// is unchanged so the sidecar parse is byte-compatible.
    ///
    /// Pipeline: full-text search over a designated `(label, property)`
    /// pair returns `{ node_id, score }` hits. When `scope_json` carries a
    /// bbox, a spatial bounding-box search runs over the designated
    /// `(label, lat/lon)` pair and the two result sets are intersected
    /// client-side (RustyRed has no combined endpoint). Surviving hits are
    /// hydrated with a label/snippet/url via per-node `get_node` fetches
    /// and projected into `priorKnowledge`.
    ///
    /// Auth + tenancy: the RustyRed bearer lives server-side in the Axum
    /// process env (`RUSTYRED_API_TOKEN`), never in the frontend bundle.
    /// TenantContext is required on every call (per the multi-tenancy
    /// invariant) and the tenant slug is interpolated into every RustyRed
    /// URL path so the read is tenant-scoped.
    ///
    /// Failure modes:
    /// - `Status::unauthenticated` when TenantContext is missing.
    /// - Honest-empty `results_json` (with a `rustyred_unconfigured`
    ///   gapClosure sentinel the sidecar renders as "Research sources are
    ///   not connected yet") when `RUSTYRED_URL` is unset. This keeps the
    ///   "backend not wired" state a calm empty state, not a network error.
    /// - `Status::internal` when RustyRed is reachable but errors.
    async fn civic_research(
        &self,
        request: Request<CivicResearchRequest>,
    ) -> Result<Response<CivicResearchResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let query = request.query.clone();

        // newEvidence: live provider fanout (keyless public APIs), independent
        // of the RustyRed graph read below. Runs even when the graph is empty,
        // so a research query returns fresh sources instead of nothing.
        // Best-effort: a provider error yields an empty list, never fails the
        // call. Library of Congress is the first provider (free-text, keyless);
        // ArcGIS REST + OSM Overpass follow. Computed up front so every return
        // path (unconfigured, store-unavailable, success) can carry it.
        let mut new_evidence = fetch_loc_new_evidence(&query).await;

        // Frontier crawl: seed RustyRed's crawler from the provider URLs, then
        // read the crawl substrate back. RustyRed fetches the seeds live,
        // expands its link frontier, and commits crawled pages into the tenant
        // graph (acquisition that feeds RustyRed -> Postgres). Best-effort: any
        // failure (unconfigured, missing graph:write scope, slow crawl) is
        // logged and skipped; civic_research still returns the provider hits.
        append_frontier_crawl(tenant.as_str(), &query, &mut new_evidence).await;

        // ArcGIS REST: structured Flint civic features (parcels, addresses) for
        // the query, from the configured ARCGIS_REST_ENDPOINTS layers. Appended
        // after the crawl so the synthetic feature URLs are not crawl seeds.
        // Best-effort: unset env or any error yields nothing.
        new_evidence.extend(fetch_arcgis_new_evidence(&query).await);

        // OSM Overpass: building footprints in the query's spatial scope, when
        // the request carries a bbox (parcel/area-scoped research). Free-text
        // queries have no bbox, so OSM is skipped. Best-effort, structured.
        new_evidence.extend(fetch_osm_new_evidence(&request.scope_json).await);

        // Reach RustyRed via env construction per-call. The expected usage
        // pattern is one call per user research query (low rate, long
        // tail), so the per-call client build is acceptable; if call
        // volume grows, hold a long-lived `rustyred_client::Client` on
        // `AtlasState`. The bearer lives server-side in the Axum process
        // env (RUSTYRED_API_TOKEN) per the service-tier-auth rule.
        //
        // Unconfigured path: when RUSTYRED_URL is unset, `from_env`
        // returns `RustyRedError::Config`. We do NOT hard-error the user;
        // we emit an honest-empty results_json carrying a single
        // `rustyred_unconfigured` gapClosure sentinel. The Node sidecar
        // special-cases that token and renders "Research sources are not
        // connected yet" instead of a network error.
        let rustyred = match RustyRedClient::from_env() {
            Ok(client) => client,
            Err(RustyRedError::Config(_)) => {
                // newEvidence empty: web-retrieval / source-paired new
                // evidence is produced by the Theorem epistemic enrichment
                // slice (next slice), not by the direct RustyRed graph
                // read. Theorem is reserved for epistemics and is
                // intentionally NOT in this search path.
                //
                // The only gapClosure we emit here is the
                // rustyred_unconfigured sentinel; gap detection + closure
                // is otherwise a Theorem epistemic-reasoning concern.
                //
                // provenanceRootId empty + synthetic run_id: provenance
                // graph rooting belongs to the Theorem epistemic slice.
                return Ok(Response::new(rustyred_unconfigured_response(
                    &query,
                    new_evidence.clone(),
                )));
            }
            Err(err) => {
                return Err(Status::unavailable(format!(
                    "civic research is unavailable: {err}"
                )));
            }
        };

        // Parse optional scope hints from scope_json (proto field 4). The
        // current resolver previously ignored scope/budget entirely; we now
        // extract the searchable property, label, top_k, and an optional
        // bbox. A malformed scope blob must NOT 4xx/5xx the call: fail soft
        // to an empty scope and run an unfiltered full-text search.
        let scope = ResearchScope::from_scope_json(&request.scope_json);

        // (1) Full-text search over the designated (label, property) pair.
        // RustyRed requires the pair to have been pre-designated via
        // graph/fulltext/designate; an undesignated property returns empty.
        // The property defaults to a civic searchable-text field and is
        // overridable from scope_json so it is never hardcoded wrong.
        let ft_req = FullTextSearchRequest {
            label: scope.label.clone(),
            property: scope.property.clone(),
            query: query.clone(),
            k: scope.top_k,
        };
        // RustyRed returns an *error*, not an empty set, when this tenant has
        // no fulltext designation yet OR when a search matches nothing (both
        // surface as a `store_unavailable` / `store_mode_unsupported` upstream
        // response). That is a not-wired empty state, not a server fault, so we
        // degrade it to the same calm `rustyred_unconfigured` response as the
        // unset path. Genuine failures (network, other 5xx) still surface as
        // Status::internal.
        let ft = match rustyred.fulltext_search(tenant.as_str(), &ft_req).await {
            Ok(ft) => ft,
            Err(err) if is_rustyred_store_unavailable(&err) => {
                return Ok(Response::new(rustyred_unconfigured_response(
                    &query,
                    new_evidence.clone(),
                )));
            }
            Err(err) => {
                return Err(Status::internal(format!(
                    "rustyred fulltext search failed: {err}"
                )));
            }
        };

        // (2) Optional spatial bbox intersection. RustyRed has no combined
        // fulltext+spatial endpoint, so when scope_json carries a bbox we
        // run a separate bounding-box search and keep only the full-text
        // hits whose node_id also appears in the bbox node-id set. A
        // spatial error (e.g. the label was never spatially designated)
        // logs + skips the filter rather than blanking every bbox-scoped
        // search to zero.
        let mut hits = ft.results;
        if let Some(bbox) = scope.bbox.as_ref() {
            let bbox_req = SpatialBboxRequest {
                label: scope.spatial_label.clone(),
                lat_property: scope.lat_property.clone(),
                lon_property: scope.lon_property.clone(),
                min_lat: bbox.min_lat,
                min_lon: bbox.min_lon,
                max_lat: bbox.max_lat,
                max_lon: bbox.max_lon,
            };
            match rustyred.spatial_bounding_box(tenant.as_str(), &bbox_req).await {
                Ok(bbox_resp) => {
                    let in_box: HashSet<String> = bbox_resp.node_ids.into_iter().collect();
                    hits.retain(|hit| {
                        hit.node_id
                            .as_ref()
                            .map(|id| in_box.contains(id))
                            .unwrap_or(false)
                    });
                }
                Err(err) => {
                    warn!(%err, "rustyred spatial bbox search failed; skipping spatial filter");
                }
            }
        }

        // (3) Hydrate the surviving hits into priorKnowledge. The full-text
        // route returns only {node_id, score}, so we fetch each node and
        // pull a human label/snippet/url from its properties. Hydration is
        // best-effort and bounded to the top-k hits; on a get_node error or
        // a missing node we emit a minimal hit (label = node_id).
        let mut prior_knowledge: Vec<Value> = Vec::with_capacity(hits.len());
        for hit in hits.iter().take(scope.top_k) {
            let node_id = match hit.node_id.as_deref() {
                Some(id) if !id.is_empty() => id,
                _ => continue,
            };
            let node = rustyred
                .get_node(tenant.as_str(), node_id)
                .await
                .ok()
                .and_then(|resp| resp.node);
            prior_knowledge.push(project_fulltext_hit(node_id, hit.score, node.as_ref()));
        }

        let total = prior_knowledge.len();

        // newEvidence stays empty: web-retrieval / source-paired new
        // evidence is the Theorem epistemic enrichment slice (next slice),
        // not the direct RustyRed graph read. Theorem is reserved for
        // epistemics and is intentionally NOT in this search path.
        //
        // gapClosures stays empty in the configured path: gap detection +
        // closure is a Theorem epistemic-reasoning concern (next slice).
        //
        // provenanceRootId stays "" and run_id is a fresh synthetic uuid:
        // provenance graph rooting belongs to the Theorem epistemic slice;
        // the direct RustyRed read has no provenance root yet.
        //
        // metadata.degraded is false here because RustyRed returned (a
        // possibly empty) hit set without error; it is true only on the
        // unconfigured sentinel path above.
        let run_id = format!("civic-atlas:{}", Uuid::new_v4());
        let results_json = serde_json::to_string(&json!({
            "query": query,
            "runId": &run_id,
            "totalReturned": total,
            "totalAdmitted": total,
            "roundsExecuted": 1,
            "latencyMs": 0,
            "metadata": {
                "mode": "civic_atlas",
                "substrate": "rustyred",
                "searchService": "rustyred.graph.fulltext",
                "degraded": false,
            },
            "priorKnowledge": prior_knowledge,
            "newEvidence": new_evidence,
            "gapClosures": [],
            "provenanceRootId": "",
        }))
        .unwrap_or_else(|_| String::from("{}"));

        Ok(Response::new(CivicResearchResponse {
            run_id,
            skill: String::from("civic_atlas"),
            results_json,
        }))
    }

    async fn persist_artifact(
        &self,
        request: Request<PersistArtifactRequest>,
    ) -> Result<Response<PersistArtifactResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.artifact_key.trim().is_empty() {
            return Err(Status::invalid_argument("artifact_key is required"));
        }
        if request.source_type.trim().is_empty() {
            return Err(Status::invalid_argument("source_type is required"));
        }
        if request.title.trim().is_empty() {
            return Err(Status::invalid_argument("title is required"));
        }
        if !artifact_anchor_present(&request) {
            return Err(Status::invalid_argument(
                "persist artifact requires parcel_ref, building_id, building_part_id, or anchor_geometry_wkt",
            ));
        }

        let payload_jsonb = parse_json_object_str(&request.payload_json, "payload_json")?;
        let anchor_payload_jsonb =
            parse_json_object_str(&request.anchor_payload_json, "anchor_payload_json")?;
        let building_id = optional_uuid_status(&request.building_id, "building_id")?;
        let building_part_id = optional_uuid_status(&request.building_part_id, "building_part_id")?;
        let anchor_kind = normalized_anchor_kind(&request.anchor_kind);

        let pool = self
            .state
            .db_pool()
            .ok_or_else(|| Status::unavailable("DATABASE_URL is required for PersistArtifact"))?;
        let mut tx = pool.begin().await.map_err(map_db_error)?;
        let tenant_id = resolve_tenant_uuid(&mut tx, tenant.as_str()).await?;
        set_tx_tenant_uuid(&mut tx, tenant_id).await?;
        let parcel_id =
            resolve_optional_parcel_uuid(&mut tx, tenant_id, &request.parcel_ref).await?;

        let artifact_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO artifacts (
              tenant_id, artifact_key, source_type, title, uri, citation,
              captured_at_ms, payload_jsonb, updated_at
            )
            VALUES ($1, $2, $3, $4, NULLIF($5, ''), NULLIF($6, ''), $7, $8, now())
            ON CONFLICT (tenant_id, artifact_key) DO UPDATE
            SET source_type = EXCLUDED.source_type,
                title = EXCLUDED.title,
                uri = EXCLUDED.uri,
                citation = EXCLUDED.citation,
                captured_at_ms = EXCLUDED.captured_at_ms,
                payload_jsonb = EXCLUDED.payload_jsonb,
                updated_at = now()
            RETURNING id
            "#,
        )
        .bind(tenant_id)
        .bind(request.artifact_key.trim())
        .bind(request.source_type.trim())
        .bind(request.title.trim())
        .bind(request.uri.trim())
        .bind(request.citation.trim())
        .bind(request.captured_at_ms)
        .bind(SqlJson(payload_jsonb))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;

        sqlx::query(
            r#"
            DELETE FROM artifact_anchors
            WHERE tenant_id = $1 AND artifact_id = $2 AND anchor_kind = $3
            "#,
        )
        .bind(tenant_id)
        .bind(artifact_id)
        .bind(anchor_kind)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;

        sqlx::query(
            r#"
            INSERT INTO artifact_anchors (
              tenant_id, artifact_id, parcel_id, building_id, building_part_id,
              anchor_kind, geom, t_start_ms, t_end_ms, payload_jsonb
            )
            VALUES (
              $1, $2, $3, $4, $5, $6,
              CASE
                WHEN NULLIF($7, '') IS NULL THEN NULL
                ELSE ST_GeomFromText($7, 4326)
              END,
              $8, $9, $10
            )
            "#,
        )
        .bind(tenant_id)
        .bind(artifact_id)
        .bind(parcel_id)
        .bind(building_id)
        .bind(building_part_id)
        .bind(anchor_kind)
        .bind(request.anchor_geometry_wkt.trim())
        .bind(request.anchor_time_start_ms)
        .bind(request.anchor_time_end_ms)
        .bind(SqlJson(anchor_payload_jsonb))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;

        tx.commit().await.map_err(map_db_error)?;
        Ok(Response::new(PersistArtifactResponse {
            artifact_id: artifact_id.to_string(),
            status: "persisted".to_string(),
        }))
    }
}

/// Bounding box pulled from `scope_json` for the spatial intersection.
#[derive(Debug, Clone)]
struct ScopeBbox {
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
}

/// Scope hints extracted from `CivicResearchRequest.scope_json` (proto
/// field 4). Every field has a civic default so an empty or malformed
/// scope blob still runs an unfiltered full-text search. The resolver
/// fails soft: a bad scope JSON never errors the call.
#[derive(Debug, Clone)]
struct ResearchScope {
    /// Full-text label filter (None = the designated default label).
    label: Option<String>,
    /// Designated full-text property to search.
    property: String,
    /// Top-k for both the full-text request and the hydration bound.
    top_k: usize,
    /// Optional bounding box; when present, the spatial endpoint runs and
    /// its node-id set is intersected with the full-text hits.
    bbox: Option<ScopeBbox>,
    /// Designated spatial label / lat / lon properties for the bbox query.
    spatial_label: String,
    lat_property: String,
    lon_property: String,
}

impl ResearchScope {
    /// Default full-text property the civic-atlas RustyRed deployment is
    /// expected to have designated its searchable text under. Overridable
    /// from `scope_json.property` so it is not hardcoded wrong if the
    /// deployment designated a different field (e.g. "search_text").
    const DEFAULT_PROPERTY: &'static str = "name";
    const DEFAULT_TOP_K: usize = 20;
    const DEFAULT_SPATIAL_LABEL: &'static str = "Place";
    const DEFAULT_LAT_PROPERTY: &'static str = "lat";
    const DEFAULT_LON_PROPERTY: &'static str = "lon";

    fn from_scope_json(raw: &str) -> Self {
        let parsed: Option<Value> = if raw.trim().is_empty() {
            None
        } else {
            // Fail soft: a malformed scope blob is ignored, not surfaced.
            serde_json::from_str(raw).ok()
        };
        let obj = parsed.as_ref().and_then(Value::as_object);

        let str_field = |key: &str| -> Option<String> {
            obj.and_then(|o| o.get(key))
                .and_then(Value::as_str)
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
        };
        let f64_field = |key: &str| -> Option<f64> {
            obj.and_then(|o| o.get(key)).and_then(Value::as_f64)
        };

        let property = str_field("property").unwrap_or_else(|| Self::DEFAULT_PROPERTY.to_string());
        let label = str_field("label");
        let top_k = obj
            .and_then(|o| o.get("topK").or_else(|| o.get("top_k")))
            .and_then(Value::as_u64)
            .map(|k| k as usize)
            .filter(|k| *k > 0)
            .unwrap_or(Self::DEFAULT_TOP_K);

        // bbox accepted as either a nested object {minLat,minLon,maxLat,
        // maxLon} (camel or snake) or a flat [minLon,minLat,maxLon,maxLat]
        // GeoJSON-style array. Only construct it when all four bounds
        // resolve; a partial bbox is treated as no bbox.
        let bbox = Self::parse_bbox(obj);

        let spatial_label = str_field("spatialLabel")
            .or_else(|| str_field("spatial_label"))
            .or_else(|| label.clone())
            .unwrap_or_else(|| Self::DEFAULT_SPATIAL_LABEL.to_string());
        let lat_property = str_field("latProperty")
            .or_else(|| str_field("lat_property"))
            .unwrap_or_else(|| Self::DEFAULT_LAT_PROPERTY.to_string());
        let lon_property = str_field("lonProperty")
            .or_else(|| str_field("lon_property"))
            .unwrap_or_else(|| Self::DEFAULT_LON_PROPERTY.to_string());

        let _ = f64_field; // reserved for future numeric scope knobs
        Self {
            label,
            property,
            top_k,
            bbox,
            spatial_label,
            lat_property,
            lon_property,
        }
    }

    fn parse_bbox(obj: Option<&serde_json::Map<String, Value>>) -> Option<ScopeBbox> {
        let obj = obj?;
        let bbox_val = obj.get("bbox")?;
        if let Some(bbox_obj) = bbox_val.as_object() {
            let get = |a: &str, b: &str| -> Option<f64> {
                bbox_obj
                    .get(a)
                    .or_else(|| bbox_obj.get(b))
                    .and_then(Value::as_f64)
            };
            Some(ScopeBbox {
                min_lat: get("minLat", "min_lat")?,
                min_lon: get("minLon", "min_lon")?,
                max_lat: get("maxLat", "max_lat")?,
                max_lon: get("maxLon", "max_lon")?,
            })
        } else if let Some(arr) = bbox_val.as_array() {
            // GeoJSON-style [minLon, minLat, maxLon, maxLat].
            if arr.len() != 4 {
                return None;
            }
            let n = |i: usize| arr.get(i).and_then(Value::as_f64);
            Some(ScopeBbox {
                min_lon: n(0)?,
                min_lat: n(1)?,
                max_lon: n(2)?,
                max_lat: n(3)?,
            })
        } else {
            None
        }
    }
}

/// Build the response for a research query whose RustyRed graph read produced
/// nothing usable: `RUSTYRED_URL` unset (`RustyRedError::Config`) or RustyRed
/// reporting its fulltext store unavailable for this tenant (no `(label,
/// property)` designation yet, or a zero-result search; RustyRed returns an
/// error for both, not an empty set). `priorKnowledge` is empty, but
/// `new_evidence` (the live provider fanout) is carried through, so a query
/// still returns fresh sources when the graph is empty. `degraded: true`
/// reflects the empty graph read. The `rustyred_unconfigured` gapClosure is
/// emitted ONLY when there is no provider evidence either: with evidence
/// present the sources ARE connected, just not the graph.
fn rustyred_unconfigured_response(query: &str, new_evidence: Vec<Value>) -> CivicResearchResponse {
    let run_id = format!("civic-atlas:{}", Uuid::new_v4());
    let total = new_evidence.len();
    let has_evidence = total > 0;
    let gap_closures: Vec<Value> = if has_evidence {
        Vec::new()
    } else {
        vec![json!({
            "gapId": "rustyred_unconfigured",
            "description": "rustyred_unconfigured: research sources are not connected yet",
            "closed": false,
            "evidenceCount": 0,
        })]
    };
    let results_json = serde_json::to_string(&json!({
        "query": query,
        "runId": &run_id,
        "totalReturned": total,
        "totalAdmitted": total,
        "roundsExecuted": if has_evidence { 1 } else { 0 },
        "latencyMs": 0,
        "metadata": {
            "mode": "civic_atlas",
            "substrate": "rustyred",
            "searchService": "rustyred.graph.fulltext",
            "degraded": true,
        },
        "priorKnowledge": [],
        "newEvidence": new_evidence,
        "gapClosures": gap_closures,
        "provenanceRootId": "",
    }))
    .unwrap_or_else(|_| String::from("{}"));
    CivicResearchResponse {
        run_id,
        skill: String::from("civic_atlas"),
        results_json,
    }
}

/// Fetch Library of Congress catalog hits for `query` as `newEvidence` items.
/// Keyless public JSON API (`loc.gov/search/?fo=json`): HABS/HAER surveys,
/// Sanborn maps, photo collections, etc. Best-effort: any error (network,
/// non-2xx, non-JSON, missing fields) yields an empty Vec and never fails
/// `civic_research`. Item keys mirror `project_fulltext_hit` so the Node
/// sidecar folds them into `sources`/`signals` with no frontend change.
async fn fetch_loc_new_evidence(query: &str) -> Vec<Value> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("CivicAtlas/1.0 (+https://ourcivicatlas.org)")
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            warn!(%err, "loc provider: client build failed");
            return Vec::new();
        }
    };
    let response = match client
        .get("https://www.loc.gov/search/")
        .query(&[("q", trimmed), ("fo", "json"), ("c", "12")])
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            warn!(status = response.status().as_u16(), "loc provider: non-2xx");
            return Vec::new();
        }
        Err(err) => {
            warn!(%err, "loc provider: request failed");
            return Vec::new();
        }
    };
    let payload: Value = match response.json().await {
        Ok(payload) => payload,
        Err(err) => {
            warn!(%err, "loc provider: non-JSON body");
            return Vec::new();
        }
    };
    let Some(results) = payload.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = Vec::with_capacity(results.len());
    for result in results {
        let url = loc_pick_url(result);
        if url.is_empty() {
            continue;
        }
        let title = loc_stringify(result.get("title"));
        if title.is_empty() {
            continue;
        }
        let partof = loc_stringify(result.get("partof"));
        let snippet = loc_stringify(result.get("description"));
        out.push(json!({
            "resultId": loc_stringify(result.get("id")),
            "kind": loc_source_type(&partof),
            "label": loc_truncate(&title, 300),
            "snippet": loc_truncate(&snippet, 400),
            "relevanceScore": 0.0,
            "confidence": 0.85,
            "source": "library_of_congress",
            "url": url,
            "closesGapId": "",
        }));
    }
    out
}

/// Canonical landing URL for a LoC result: prefer `id` (a full URL), fall back
/// to the first `url` resource. Empty string when neither is http(s).
fn loc_pick_url(result: &Value) -> String {
    if let Some(primary) = result.get("id").and_then(Value::as_str) {
        if primary.starts_with("http") {
            return primary.to_string();
        }
    }
    if let Some(first) = result
        .get("url")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(Value::as_str)
    {
        if first.starts_with("http") {
            return first.to_string();
        }
    }
    String::new()
}

/// LoC fields are sometimes a string, sometimes a list of strings, sometimes a
/// `{value|name|label: ...}` object. Coerce to a single display string.
fn loc_stringify(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter(|item| !item.is_null())
            .map(|item| loc_stringify(Some(item)))
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Object(obj)) => {
            for key in ["value", "name", "label"] {
                if let Some(inner) = obj.get(key) {
                    return loc_stringify(Some(inner));
                }
            }
            Value::Object(obj.clone()).to_string()
        }
        Some(other) => other.to_string(),
    }
}

/// Map a LoC `partof` collection string to a civic-meaningful source kind.
fn loc_source_type(partof: &str) -> &'static str {
    let lower = partof.to_lowercase();
    if lower.contains("sanborn") {
        "loc_sanborn_map"
    } else if lower.contains("historic american") {
        "loc_habs_haer"
    } else if lower.contains("photograph") {
        "loc_photograph"
    } else if lower.contains("panoramic") {
        "loc_panoramic"
    } else if lower.contains("newspaper") {
        "loc_newspaper"
    } else if lower.contains("map") {
        "loc_map"
    } else {
        "loc_item"
    }
}

/// Truncate to at most `max` chars on a char boundary (LoC titles can be long).
fn loc_truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// True when a RustyRed error means the fulltext store is not usable for this
/// tenant yet: no `(label, property)` designation, or a zero-result search.
/// RustyRed surfaces both as a `store_unavailable` / `store_mode_unsupported`
/// upstream response rather than an empty result set, so `civic_research`
/// treats them as a calm empty state instead of a hard failure.
fn is_rustyred_store_unavailable(err: &RustyRedError) -> bool {
    matches!(
        err,
        RustyRedError::Upstream { body, .. }
            if body.contains("store_unavailable") || body.contains("store_mode_unsupported")
    )
}

/// Address-suffix + redundant-geo stop-words dropped from ArcGIS query tokens.
/// Street types are dropped because the parcel data abbreviates them ("ST", not
/// "STREET"), so a literal LIKE on the spelled-out word would miss; the city /
/// state are dropped because the layer is already Flint-scoped.
const ARCGIS_STOPWORDS: &[&str] = &[
    "flint", "michigan", "mi", "usa", "street", "st", "avenue", "ave", "road", "rd", "drive",
    "dr", "lane", "ln", "boulevard", "blvd", "court", "ct", "place", "pl", "circle", "cir",
    "way", "terrace", "ter", "highway", "hwy", "parkway", "pkwy", "n", "s", "e", "w", "north",
    "south", "east", "west",
];

/// Build a portable ArcGIS WHERE clause from a free-text query. Tokenizes the
/// query, drops stop-words, and ANDs the remaining meaningful tokens (each
/// matching ANY field via OR). So "Saginaw Street Flint Michigan" reduces to
/// just "saginaw" and matches records like "2513 N SAGINAW ST", instead of
/// requiring the whole phrase as one substring (which never matches). Falls back
/// to a whole-query LIKE when every token is a stop-word, and to `1=0` when no
/// fields are configured. All literals are single-quote-escaped.
fn arcgis_where_clause(query: &str, fields: &[&str]) -> String {
    if fields.is_empty() {
        return "1=0".to_string();
    }
    let any_field = |needle: &str| -> String {
        let esc = needle.replace('\'', "''");
        let ors = fields
            .iter()
            .map(|field| format!("UPPER({field}) LIKE UPPER('%{esc}%')"))
            .collect::<Vec<_>>()
            .join(" OR ");
        format!("({ors})")
    };
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|token| token.len() >= 2 && !ARCGIS_STOPWORDS.contains(&token.as_str()))
        .collect();
    if tokens.is_empty() {
        any_field(query.trim())
    } else {
        tokens
            .iter()
            .map(|token| any_field(token))
            .collect::<Vec<_>>()
            .join(" AND ")
    }
}

/// Fetch ArcGIS REST feature hits for `query` as `newEvidence` items. Reads the
/// comma-separated `ARCGIS_REST_ENDPOINTS` env (per-tenant feature-layer URLs,
/// e.g. the City of Flint GIS parcel layer) and queries each with the universal
/// free-text `text=` param. Keyless. Best-effort: unset env or any error yields
/// an empty Vec. ArcGIS returns failures as `200` + an `error` key, handled here.
async fn fetch_arcgis_new_evidence(query: &str) -> Vec<Value> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let endpoints_raw = std::env::var("ARCGIS_REST_ENDPOINTS").unwrap_or_default();
    let endpoints: Vec<&str> = endpoints_raw
        .split(',')
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
        .collect();
    if endpoints.is_empty() {
        return Vec::new();
    }
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("CivicAtlas/1.0 (+https://ourcivicatlas.org)")
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            warn!(%err, "arcgis provider: client build failed");
            return Vec::new();
        }
    };
    // Free-text via a WHERE LIKE across configurable string fields. ArcGIS
    // layers vary: the universal `text=` param is NOT supported by all of them
    // (the Flint parcel layer rejects it with "Invalid query parameters"), so
    // build a portable WHERE. Fields default to the Flint parcel address/owner
    // columns and are overridable via `ARCGIS_SEARCH_FIELDS` so other layers
    // are not hardcoded wrong. The query is single-quote-escaped for the SQL
    // string literal.
    let fields_raw = std::env::var("ARCGIS_SEARCH_FIELDS")
        .unwrap_or_else(|_| "Full_Prop,Prop_Stree,Tx_PayName".to_string());
    let fields: Vec<&str> = fields_raw
        .split(',')
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .collect();
    let where_clause = arcgis_where_clause(trimmed, &fields);
    let mut out: Vec<Value> = Vec::new();
    for endpoint in endpoints {
        let endpoint = endpoint.trim_end_matches('/');
        let url = format!("{endpoint}/query");
        let response = match client
            .get(&url)
            .query(&[
                ("where", where_clause.as_str()),
                ("outFields", "*"),
                ("returnGeometry", "false"),
                ("outSR", "4326"),
                ("resultRecordCount", "10"),
                ("f", "json"),
            ])
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                warn!(status = response.status().as_u16(), endpoint, "arcgis: non-2xx");
                continue;
            }
            Err(err) => {
                warn!(%err, endpoint, "arcgis: request failed");
                continue;
            }
        };
        let payload: Value = match response.json().await {
            Ok(payload) => payload,
            Err(err) => {
                warn!(%err, endpoint, "arcgis: non-JSON body");
                continue;
            }
        };
        // ArcGIS reports layer errors as 200 + an `error` object.
        if payload.get("error").is_some() {
            warn!(endpoint, "arcgis: layer returned an error payload");
            continue;
        }
        let Some(features) = payload.get("features").and_then(Value::as_array) else {
            continue;
        };
        for feature in features {
            let Some(attributes) = feature.get("attributes").and_then(Value::as_object) else {
                continue;
            };
            let title = arcgis_pick_title(attributes);
            if title.is_empty() {
                continue;
            }
            let oid = attributes
                .get("OBJECTID")
                .or_else(|| attributes.get("OID"))
                .and_then(|value| {
                    value
                        .as_i64()
                        .map(|n| n.to_string())
                        .or_else(|| value.as_str().map(String::from))
                });
            let hit_url = match oid {
                Some(oid) if !oid.is_empty() => format!("{endpoint}/{oid}"),
                _ => endpoint.to_string(),
            };
            out.push(json!({
                "resultId": hit_url,
                "kind": "arcgis_feature",
                "label": title.chars().take(300).collect::<String>(),
                "snippet": arcgis_snippet(attributes).chars().take(400).collect::<String>(),
                "relevanceScore": 0.0,
                "confidence": 0.7,
                "source": "arcgis_rest",
                "url": hit_url,
                "closesGapId": "",
            }));
        }
    }
    out
}

/// Pick a human title from ArcGIS feature attributes (layers vary in schema).
/// Covers common municipal parcel columns including the City of Flint layer
/// (`Full_Prop` = full property address), then falls back to the first non-empty
/// string attribute so a feature is NEVER dropped just because its schema is
/// unfamiliar (the original list only had generic names and silently skipped
/// every Flint parcel).
fn arcgis_pick_title(attributes: &serde_json::Map<String, Value>) -> String {
    for key in [
        "Full_Prop", "FULL_PROP", "NAME", "Name", "name", "BUILDING_NAME", "Building_Name",
        "ADDRESS", "Address", "address", "Prop_Add", "SITE_ADDRESS", "SiteAddress",
        "site_address", "Prop_Stree", "STREETNAME", "StreetName", "street_name", "PARCEL_ID",
        "ParcelID", "parcel_id", "PIDdash", "PIDText", "PIN", "Pin", "pin",
    ] {
        if let Some(value) = attributes.get(key) {
            let text = arcgis_attr_str(value);
            if !text.is_empty() {
                return text;
            }
        }
    }
    // Fallback: first non-empty string attribute (skip shape/id housekeeping) so
    // an unfamiliar layer's feature still gets a label instead of being skipped.
    for (key, value) in attributes {
        if key.starts_with("Shape")
            || key.eq_ignore_ascii_case("objectid")
            || key.eq_ignore_ascii_case("oid")
            || key.eq_ignore_ascii_case("fid")
        {
            continue;
        }
        if let Value::String(text) = value {
            if !text.is_empty() {
                return text.clone();
            }
        }
    }
    String::new()
}

/// Compact snippet from the most useful ArcGIS attributes (max 4 fields).
/// Owner / use / class / year first (the useful supplementary info, since the
/// title is usually the address), covering the City of Flint columns plus the
/// generic equivalents on other layers.
fn arcgis_snippet(attributes: &serde_json::Map<String, Value>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for key in [
        "Tx_PayName", "OWNER", "Owner", "Use_Type", "USE", "Use", "Prop_Class", "PROPCLASS",
        "PropClass", "Year_Built", "YEAR_BUILT", "YearBuilt", "Prop_Add", "ADDRESS", "Address",
    ] {
        if parts.len() >= 4 {
            break;
        }
        if let Some(value) = attributes.get(key) {
            let text = arcgis_attr_str(value);
            if !text.is_empty() {
                parts.push(text);
            }
        }
    }
    parts.join(" · ")
}

/// Coerce an ArcGIS attribute value (string or number) to a display string.
fn arcgis_attr_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Fetch OpenStreetMap building footprints in the query's spatial scope as
/// `newEvidence` items. OSM Overpass is a STRUCTURED (spatial) provider: it
/// fires only when `scope_json` carries a bbox (a parcel/area-scoped research
/// query). Free-text queries with no bbox are skipped. Keyless. Best-effort:
/// any error yields an empty Vec. Source: the public Overpass API.
async fn fetch_osm_new_evidence(scope_json: &str) -> Vec<Value> {
    let scope = ResearchScope::from_scope_json(scope_json);
    let Some(bbox) = scope.bbox.as_ref() else {
        return Vec::new();
    };
    // Overpass QL: buildings (ways) within the bbox (south,west,north,east).
    let overpass_query = format!(
        "[out:json][timeout:10];\n(\n  way[\"building\"]({},{},{},{});\n);\nout tags center;\n",
        bbox.min_lat, bbox.min_lon, bbox.max_lat, bbox.max_lon
    );
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent("CivicAtlas/1.0 (+https://ourcivicatlas.org)")
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            warn!(%err, "osm provider: client build failed");
            return Vec::new();
        }
    };
    let response = match client
        .post("https://overpass-api.de/api/interpreter")
        .form(&[("data", overpass_query.as_str())])
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            warn!(status = response.status().as_u16(), "osm: non-2xx");
            return Vec::new();
        }
        Err(err) => {
            warn!(%err, "osm: request failed");
            return Vec::new();
        }
    };
    let payload: Value = match response.json().await {
        Ok(payload) => payload,
        Err(err) => {
            warn!(%err, "osm: non-JSON body");
            return Vec::new();
        }
    };
    let Some(elements) = payload.get("elements").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = Vec::with_capacity(elements.len().min(25));
    for element in elements.iter().take(25) {
        let osm_type = element.get("type").and_then(Value::as_str).unwrap_or("");
        let osm_id = element.get("id").and_then(Value::as_i64);
        let (osm_type, osm_id) = match (osm_type, osm_id) {
            (kind, Some(id)) if !kind.is_empty() => (kind, id),
            _ => continue,
        };
        let tags = element.get("tags").and_then(Value::as_object);
        let name = tags
            .and_then(|tags| tags.get("name").or_else(|| tags.get("addr:housename")))
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| format!("{osm_type} {osm_id}"));
        let url = format!("https://www.openstreetmap.org/{osm_type}/{osm_id}");
        out.push(json!({
            "resultId": &url,
            "kind": "osm_feature",
            "label": name.chars().take(300).collect::<String>(),
            "snippet": osm_snippet(tags).chars().take(400).collect::<String>(),
            "relevanceScore": 0.0,
            "confidence": 0.6,
            "source": "osm_overpass",
            "url": url,
            "closesGapId": "",
        }));
    }
    out
}

/// Compact snippet from OSM tags (building/historic/address/start_date), max 6.
fn osm_snippet(tags: Option<&serde_json::Map<String, Value>>) -> String {
    let Some(tags) = tags else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for key in [
        "building", "amenity", "historic", "addr:street", "addr:housenumber",
        "start_date", "description",
    ] {
        if parts.len() >= 6 {
            break;
        }
        if let Some(value) = tags.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                parts.push(format!("{key}={value}"));
            }
        }
    }
    parts.join(" · ")
}

/// Frontier crawl: seed RustyRed's live crawler from the URLs already in
/// `new_evidence` (the provider hits), then read the crawl substrate back and
/// append the crawled-page hits. RustyRed fetches the seeds, expands its link
/// frontier, and commits crawled pages into `tenant`'s graph: the acquisition
/// path that populates RustyRed (and downstream Postgres). Entirely
/// best-effort: unconfigured RustyRed, a token without `graph:write`, a slow
/// crawl, or a SERP miss are all logged and skipped, leaving the provider hits
/// intact. A small page budget keeps per-query latency bounded.
async fn append_frontier_crawl(tenant: &str, query: &str, new_evidence: &mut Vec<Value>) {
    let seeds: Vec<String> = new_evidence
        .iter()
        .filter_map(|item| item.get("url").and_then(Value::as_str))
        .filter(|url| url.starts_with("http"))
        .take(5)
        .map(String::from)
        .collect();
    if seeds.is_empty() {
        return;
    }
    let client = match RustyRedClient::from_env() {
        Ok(client) => client,
        // Unconfigured RustyRed: provider hits are already present; no frontier.
        Err(_) => return,
    };
    // Crawl the seeds into the tenant graph (best-effort, bounded budget).
    if let Err(err) = client.crawl(tenant, &seeds, 8).await {
        warn!(%err, "frontier crawl failed; continuing with provider hits");
    }
    // Build a url -> rich label map from the provider hits (LoC etc.) so a
    // crawled page re-fetched from a provider URL gets the provider's real title
    // instead of RustyRed's URL-derived id. Keyed scheme-insensitively (LoC emits
    // http, the crawler may canonicalize to https).
    let provider_titles: std::collections::HashMap<String, String> = new_evidence
        .iter()
        .filter_map(|item| {
            let url = item.get("url").and_then(Value::as_str)?;
            let label = item.get("label").and_then(Value::as_str)?;
            Some((normalize_url_key(url), label.to_string()))
        })
        .collect();
    // Read the crawl substrate for the query; append crawled-page hits.
    match client.serp_search(tenant, query).await {
        Ok(serp) => {
            for hit in &serp.search.hits {
                if let Some(mut item) = serp_hit_to_evidence(hit) {
                    if let Some(url) = item.get("url").and_then(Value::as_str).map(String::from) {
                        if let Some(label) = provider_titles.get(&normalize_url_key(&url)) {
                            item["label"] = json!(label);
                        }
                    }
                    new_evidence.push(item);
                }
            }
        }
        Err(err) => warn!(%err, "frontier serp read failed"),
    }
}

/// Normalize a URL to a scheme-insensitive, trailing-slash-insensitive key, for
/// matching crawled hits back to provider hits by URL.
fn normalize_url_key(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_lowercase()
}

/// Map a RustyRed crawl-substrate hit (loose JSON) into a `newEvidence` item,
/// mirroring `project_fulltext_hit`'s key set. Probes the common
/// url/title/snippet/score keys; returns `None` for a hit without a usable URL.
fn serp_hit_to_evidence(hit: &Value) -> Option<Value> {
    let url = hit.get("url").and_then(Value::as_str).unwrap_or("");
    if url.is_empty() {
        return None;
    }
    let title = hit
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| hit.get("label").and_then(Value::as_str))
        .unwrap_or(url);
    let snippet = hit
        .get("snippet")
        .and_then(Value::as_str)
        .or_else(|| hit.get("excerpt").and_then(Value::as_str))
        .or_else(|| hit.get("text").and_then(Value::as_str))
        .unwrap_or("");
    let score = hit.get("score").and_then(Value::as_f64).unwrap_or(0.0);
    Some(json!({
        "resultId": url,
        "kind": "web_page",
        "label": title.chars().take(300).collect::<String>(),
        "snippet": snippet.chars().take(400).collect::<String>(),
        "relevanceScore": score,
        "confidence": 0.5,
        "source": "rustyred_crawl",
        "url": url,
        "closesGapId": "",
    }))
}

/// Project a single RustyRed full-text hit (plus its hydrated node, when
/// available) into a `priorKnowledge` item. The key set MUST match what
/// the Node sidecar's `searchEvidence` reader consumes
/// (`apps/graphql-server/src/schema.ts`): `resultId`, `kind`, `label`,
/// `snippet`, `relevanceScore`, `confidence`, `source`, `url`,
/// `closesGapId`. The sidecar folds items with a `url`/`source` into
/// `SearchResults.sources`, and folds every item into
/// `SearchResults.signals`.
fn project_fulltext_hit(
    node_id: &str,
    score: Option<f32>,
    node: Option<&rustyred_client::NodeRecord>,
) -> serde_json::Value {
    let props = node.map(|n| &n.properties);
    let prop_str = |keys: &[&str]| -> String {
        props
            .and_then(|p| {
                keys.iter()
                    .find_map(|k| p.get(*k).and_then(Value::as_str))
            })
            .unwrap_or("")
            .to_string()
    };

    // label falls back to the node_id so a missing/sparse node still
    // renders something identifiable rather than an empty row.
    let mut label = prop_str(&["name", "title"]);
    if label.is_empty() {
        label = node_id.to_string();
    }
    let snippet = prop_str(&["summary", "description", "snippet"]);
    let source = prop_str(&["source"]);
    let url = prop_str(&["url", "homepage_url"]);
    let relevance = score.unwrap_or(0.0);

    json!({
        "resultId": node_id,
        "kind": "civic_object",
        "label": label,
        "snippet": snippet,
        "relevanceScore": relevance,
        "confidence": relevance,
        "source": source,
        "url": url,
        "closesGapId": "",
    })
}

fn parse_json_object_str(raw: &str, field_name: &str) -> Result<Value, Status> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|err| {
        Status::invalid_argument(format!("{field_name} must be valid JSON: {err}"))
    })?;
    if !value.is_object() {
        return Err(Status::invalid_argument(format!(
            "{field_name} must decode to a JSON object",
        )));
    }
    Ok(value)
}

fn optional_uuid_status(value: &str, field_name: &str) -> Result<Option<Uuid>, Status> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<Uuid>()
        .map(Some)
        .map_err(|err| Status::invalid_argument(format!("{field_name} must be a UUID: {err}")))
}

fn normalized_anchor_kind(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "reference"
    } else {
        trimmed
    }
}

fn artifact_anchor_present(request: &PersistArtifactRequest) -> bool {
    !request.parcel_ref.trim().is_empty()
        || !request.building_id.trim().is_empty()
        || !request.building_part_id.trim().is_empty()
        || !request.anchor_geometry_wkt.trim().is_empty()
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
                let nodes =
                    fetch_block_subgraph_nodes(pool, tenant.as_str(), &request.block_id, depth)
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
    // GraphQL surface ships on its own state (the composed schema), so
    // it lives as a sibling Router and is merged onto the JSON shim
    // router below. Both end up under the same Axum HTTP listener.
    let graphql = crate::graphql::graphql_router(state.clone());

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
        .merge(graphql)
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

    #[test]
    fn persist_artifact_json_parser_requires_object_payload() {
        let error = parse_json_object_str("[]", "payload_json").unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn persist_artifact_anchor_presence_accepts_parcel_ref() {
        let request = PersistArtifactRequest {
            tenant_context: None,
            artifact_key: String::new(),
            source_type: String::new(),
            title: String::new(),
            uri: String::new(),
            citation: String::new(),
            captured_at_ms: None,
            payload_json: String::new(),
            parcel_ref: "40-01-154-012".to_string(),
            building_id: String::new(),
            building_part_id: String::new(),
            anchor_kind: String::new(),
            anchor_geometry_wkt: String::new(),
            anchor_time_start_ms: None,
            anchor_time_end_ms: None,
            anchor_payload_json: String::new(),
        };
        assert!(artifact_anchor_present(&request));
        assert_eq!(normalized_anchor_kind(""), "reference");
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

async fn resolve_optional_parcel_uuid(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: Uuid,
    parcel_ref: &str,
) -> Result<Option<Uuid>, Status> {
    let trimmed = parcel_ref.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed_uuid = trimmed.parse::<Uuid>().ok();
    sqlx::query_scalar(
        r#"
        SELECT id
        FROM parcels
        WHERE tenant_id = $1
          AND (($2::uuid IS NOT NULL AND id = $2) OR parcel_key = $3)
        LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .bind(parsed_uuid)
    .bind(trimmed)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_db_error)
}
