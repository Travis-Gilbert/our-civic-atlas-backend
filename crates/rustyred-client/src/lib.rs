//! HTTP client for the Civic Atlas RustyRed-Graph-Database deployment.
//!
//! # Architecture context
//!
//! RustyRed (`https://github.com/Travis-Gilbert/RustyRed-Graph-Database`) is a
//! standalone, RAM-first graph + vector database. The Civic Atlas backend
//! deploys its OWN instance of RustyRed (separate from Theseus's
//! `RustyRedCore-THG`, which is the harness-bound customization not intended
//! for external consumers).
//!
//! This crate is the typed Rust HTTP client for the Civic Atlas RustyRed
//! deployment. It speaks the `/v1/tenants/:tenant_id/...` OpenAPI surface
//! described in the RustyRed repo's `crates/rustyred-server/src/router.rs`.
//!
//! # What this client owns
//!
//! - Full-text search (`graph/fulltext/search`) over a designated
//!   `(label, property)` pair. This is the entrypoint the `civicResearch`
//!   GraphQL mutation uses to read prior knowledge from the graph.
//! - Spatial bounding-box search (`graph/spatial/bbox`) over a designated
//!   `(label, lat_property, lon_property)` pair. Used to intersect a
//!   bbox-scoped research query against the full-text hit set client-side
//!   (RustyRed has no combined fulltext+spatial endpoint).
//! - Node hydration (`GET graph/nodes/:node_id`) so full-text hits, which
//!   carry only `node_id` + `score`, can be projected with a real
//!   label/snippet/url from the node's properties.
//! - Hybrid vector + graph search (HNSW + graph proximity blend) for
//!   civic-atlas search surfaces that have a designated vector property.
//! - (Future scope) Node + edge bulk upsert for the
//!   `civic-atlas-outbox-worker` write path. Currently the outbox writes
//!   via the `theseus-client` crate (which lands in Theseus's THG); that
//!   write path will migrate to this client once the Civic Atlas RustyRed
//!   deployment is live.
//!
//! # Auth + tenancy
//!
//! RustyRed authenticates via `Authorization: Bearer <token>` and scopes
//! every query by tenant id in the URL path. The bearer token is configured
//! per deployment via the `RUSTYRED_API_TOKEN` env var on the civic-atlas
//! backend. The tenant id is supplied by the calling resolver from the
//! `TenantContext` of the incoming request.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Default scoring alpha for hybrid vector + graph search. Matches the
/// RustyRed server default (`config::TenantConfig::hybrid_scoring`).
/// Callers can override via `HybridSearchRequest::alpha`.
pub const DEFAULT_HYBRID_ALPHA: f32 = 0.7;

/// Errors returned by the client. Categorized so callers can decide whether
/// to retry, surface to the user, or wrap as a gRPC `Status`.
#[derive(Error, Debug)]
pub enum RustyRedError {
    #[error("RustyRed config error: {0}")]
    Config(String),

    #[error("RustyRed network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("RustyRed returned {status} for {path}: {body}")]
    Upstream {
        status: u16,
        path: String,
        body: String,
    },

    #[error("RustyRed JSON decode error: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Client configuration. Build via `Client::new` or `Client::from_env`.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// Base URL of the RustyRed deployment (no trailing slash).
    /// Example: `https://civic-atlas-rustyred.up.railway.app`.
    pub base_url: String,
    /// Bearer token. Required when the RustyRed deployment runs with
    /// `RUSTY_RED_REQUIRE_AUTH=true`. The civic-atlas RustyRed deployment
    /// always requires auth per the project's "Service-Tier Auth Stays
    /// Server-Side" rule.
    pub api_token: String,
    /// Per-request timeout. Default 30s. RustyRed is fast (RAM-first) but
    /// the network round-trip from Axum to Railway may add latency; the
    /// civic-atlas search panel UX expects sub-second responses, so this
    /// timeout exists to surface upstream slowness as an honest error.
    pub timeout: Duration,
}

impl ClientConfig {
    /// Read config from env vars. Returns `Config` errors when required
    /// vars are missing so the calling service can fail fast at startup.
    ///
    /// Environment:
    /// - `RUSTYRED_URL` (required): base URL of the Civic Atlas RustyRed
    ///   instance.
    /// - `RUSTYRED_API_TOKEN` (required): bearer token authorized for the
    ///   scopes the civic-atlas backend uses (`graph:read`, `graph:write`,
    ///   `context:write`).
    /// - `RUSTYRED_TIMEOUT_MS` (optional, default 30000): per-request
    ///   timeout.
    pub fn from_env() -> Result<Self, RustyRedError> {
        let base_url = std::env::var("RUSTYRED_URL").map_err(|_| {
            RustyRedError::Config(
                "RUSTYRED_URL is not set. Point the civic-atlas backend at \
                 its RustyRed deployment, e.g. \
                 https://civic-atlas-rustyred.up.railway.app"
                    .into(),
            )
        })?;
        let api_token = std::env::var("RUSTYRED_API_TOKEN").map_err(|_| {
            RustyRedError::Config(
                "RUSTYRED_API_TOKEN is not set. The Civic Atlas RustyRed \
                 deployment requires bearer auth."
                    .into(),
            )
        })?;
        let timeout_ms = std::env::var("RUSTYRED_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(30_000);
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_token,
            timeout: Duration::from_millis(timeout_ms),
        })
    }
}

/// HTTP client for the Civic Atlas RustyRed deployment.
#[derive(Clone, Debug)]
pub struct Client {
    config: ClientConfig,
    http: reqwest::Client,
}

impl Client {
    /// Build a client from a fully-configured `ClientConfig`.
    pub fn new(config: ClientConfig) -> Result<Self, RustyRedError> {
        let mut headers = HeaderMap::new();
        let bearer = format!("Bearer {}", config.api_token);
        let mut auth = HeaderValue::from_str(&bearer)
            .map_err(|err| RustyRedError::Config(format!("Invalid bearer token: {err}")))?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .default_headers(headers)
            .build()?;

        Ok(Self { config, http })
    }

    /// Convenience constructor that pulls config from env.
    pub fn from_env() -> Result<Self, RustyRedError> {
        Self::new(ClientConfig::from_env()?)
    }

    /// Base URL the client is talking to. Useful for logging.
    pub fn base_url(&self) -> &str {
        &self.config.base_url
    }

    /// `GET /health`. Useful for startup probes and reachability checks.
    pub async fn health(&self) -> Result<HealthResponse, RustyRedError> {
        let url = format!("{}/health", self.config.base_url);
        let response = self.http.get(&url).send().await?;
        if !response.status().is_success() {
            return Err(make_upstream_error(response, "/health").await);
        }
        Ok(response.json().await?)
    }

    /// `POST /v1/tenants/:tenant_id/graph/vector/hybrid`.
    ///
    /// Hybrid search blends HNSW vector similarity with graph proximity.
    /// `alpha` controls the blend weight (defaults to RustyRed's tenant
    /// config when unset). `graph_seeds` lets the caller anchor the search
    /// around known node ids (e.g., "from this place's neighborhood").
    ///
    /// This is the entrypoint the `CivicResearch` GraphQL mutation uses
    /// today. The resolver constructs `HybridSearchRequest` from the user's
    /// query + tenant scope and returns the ranked `results` to the
    /// frontend.
    pub async fn graph_vector_hybrid(
        &self,
        tenant_id: &str,
        request: &HybridSearchRequest,
    ) -> Result<HybridSearchResponse, RustyRedError> {
        let path = format!("/v1/tenants/{tenant_id}/graph/vector/hybrid");
        let url = format!("{}{path}", self.config.base_url);
        let response = self.http.post(&url).json(request).send().await?;
        if !response.status().is_success() {
            return Err(make_upstream_error(response, &path).await);
        }
        Ok(response.json().await?)
    }

    /// `POST /v1/tenants/:tenant_id/graph/fulltext/search`.
    ///
    /// Full-text search over a designated `(label, property)` pair. The
    /// `(label, property)` must have been pre-designated on the RustyRed
    /// deployment via `graph/fulltext/designate`; searching an
    /// undesignated property returns an empty result set.
    ///
    /// This is the entrypoint the `civicResearch` GraphQL mutation uses to
    /// read prior knowledge from the graph. RustyRed returns ONLY
    /// `{ node_id, score }` per hit (no node payload), so callers that need
    /// a human label/snippet hydrate each hit with [`Client::get_node`].
    pub async fn fulltext_search(
        &self,
        tenant_id: &str,
        request: &FullTextSearchRequest,
    ) -> Result<FullTextSearchResponse, RustyRedError> {
        let path = format!("/v1/tenants/{tenant_id}/graph/fulltext/search");
        let url = format!("{}{path}", self.config.base_url);
        let response = self.http.post(&url).json(request).send().await?;
        if !response.status().is_success() {
            return Err(make_upstream_error(response, &path).await);
        }
        Ok(response.json().await?)
    }

    /// `POST /v1/tenants/:tenant_id/graph/spatial/bbox`.
    ///
    /// Returns the unranked node-id list whose `(lat_property,
    /// lon_property)` falls inside the bounding box. The
    /// `(label, lat_property, lon_property)` triple must have been
    /// pre-designated via `graph/spatial/designate`; an undesignated label
    /// errors. There are no scores and no node payloads in the response.
    ///
    /// Used to intersect a bbox-scoped `civicResearch` query against the
    /// full-text hit set client-side: RustyRed has no combined
    /// fulltext+spatial endpoint, so the resolver runs both and keeps the
    /// node ids present in both result sets.
    pub async fn spatial_bounding_box(
        &self,
        tenant_id: &str,
        request: &SpatialBboxRequest,
    ) -> Result<SpatialBboxResponse, RustyRedError> {
        let path = format!("/v1/tenants/{tenant_id}/graph/spatial/bbox");
        let url = format!("{}{path}", self.config.base_url);
        let response = self.http.post(&url).json(request).send().await?;
        if !response.status().is_success() {
            return Err(make_upstream_error(response, &path).await);
        }
        Ok(response.json().await?)
    }

    /// `GET /v1/tenants/:tenant_id/graph/nodes/:node_id`.
    ///
    /// Fetches a single node so full-text hits (which carry only
    /// `node_id` + `score`) can be projected with a real label, snippet,
    /// and url drawn from the node's `properties`.
    ///
    /// RustyRed returns a bare `404` when the node is missing. This method
    /// degrades that to `Ok(NodeFetchResponse { ok: false, node: None, .. })`
    /// so a missing node yields a minimal hit instead of erroring the whole
    /// research call. Any other non-2xx status is surfaced as an
    /// [`RustyRedError::Upstream`].
    pub async fn get_node(
        &self,
        tenant_id: &str,
        node_id: &str,
    ) -> Result<NodeFetchResponse, RustyRedError> {
        let path = format!("/v1/tenants/{tenant_id}/graph/nodes/{node_id}");
        let url = format!("{}{path}", self.config.base_url);
        let response = self.http.get(&url).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(NodeFetchResponse {
                ok: false,
                node: None,
                extra: serde_json::Map::new(),
            });
        }
        if !response.status().is_success() {
            return Err(make_upstream_error(response, &path).await);
        }
        Ok(response.json().await?)
    }

    /// `POST /crawl`. Submit seed URLs for a live frontier crawl. RustyRed
    /// fetches each seed, expands its link frontier, and commits the crawled
    /// `Page`/`ContentSnapshot`/`Domain` nodes into `tenant_id`'s graph: the
    /// acquisition path that populates RustyRed (and, downstream, Postgres).
    /// `/crawl` is root-level; the tenant rides in the body. Requires the
    /// `graph:write` scope on the bearer; a token without it gets a 403
    /// surfaced as [`RustyRedError::Upstream`] (callers treat crawl as
    /// best-effort and continue).
    pub async fn crawl(
        &self,
        tenant_id: &str,
        seeds: &[String],
        max_pages: usize,
    ) -> Result<CrawlResponse, RustyRedError> {
        let url = format!("{}/crawl", self.config.base_url);
        let body = CrawlRequest {
            tenant: tenant_id.to_string(),
            seeds: seeds.to_vec(),
            budget: Some(CrawlBudget {
                max_pages,
                // Tight bounds: the crawl runs inline in a per-query research
                // call, so cap wall-clock, depth, and bytes to keep latency
                // bounded. All four fields are required by RustyRed.
                max_seconds: 12,
                max_depth: 1,
                max_bytes: 2 * 1024 * 1024,
            }),
        };
        let response = self.http.post(&url).json(&body).send().await?;
        if !response.status().is_success() {
            return Err(make_upstream_error(response, "/crawl").await);
        }
        Ok(response.json().await?)
    }

    /// `GET /search.json?q=`. Read the crawl substrate (the `Page` graph the
    /// crawler builds) for `query`. Unlike [`Client::fulltext_search`], this
    /// needs NO `(label, property)` designation: it searches crawled pages
    /// directly, so it surfaces freshly crawled content immediately. Root-
    /// level; `tenant` rides as a query param so the read is tenant-scoped
    /// (confirm RustyRed honors it; otherwise it reads the default substrate).
    pub async fn serp_search(
        &self,
        tenant_id: &str,
        query: &str,
    ) -> Result<SerpResponse, RustyRedError> {
        let url = format!("{}/search.json", self.config.base_url);
        let response = self
            .http
            .get(&url)
            .query(&[("q", query), ("tenant", tenant_id)])
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(make_upstream_error(response, "/search.json").await);
        }
        Ok(response.json().await?)
    }
}

/// `GET /health` response.
#[derive(Debug, Clone, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    #[serde(default)]
    pub version: Option<String>,
    /// Full payload, in case RustyRed adds fields between versions and the
    /// civic-atlas backend wants to surface diagnostic info to logs.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Body of the hybrid search request. Matches RustyRed's `HybridSearchBody`
/// in `crates/rustyred-server/src/router.rs`.
#[derive(Debug, Clone, Serialize)]
pub struct HybridSearchRequest {
    /// Optional node label filter. When `None`, the search runs across all
    /// labels under the tenant namespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Vector property to query against. Required.
    pub property: String,
    /// The query payload. RustyRed accepts either a string (which it
    /// embeds server-side) or a pre-embedded `[f32]` vector under the
    /// `query` key. We keep this as `Value` to stay neutral between the
    /// two modes; civic-atlas resolvers typically pass a string.
    pub query: Value,
    /// Top-k.
    pub k: usize,
    /// Optional graph seed node ids. The hybrid scorer pulls these into
    /// the result set as graph-proximate candidates.
    #[serde(default)]
    pub graph_seeds: Vec<String>,
    /// Max hops for graph expansion from the seeds.
    pub max_hops: u32,
    /// Optional alpha override. When `None`, the tenant's configured
    /// default applies (defaulting to `DEFAULT_HYBRID_ALPHA` on the
    /// server side).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f32>,
    /// Whether to weight graph distance by edge confidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_weighted_graph_distance: Option<bool>,
    /// Optional per-edge-type weights for the graph distance score.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_type_weights: Option<Value>,
}

/// Body of the hybrid search response. RustyRed returns
/// `{ ok: true, results: [...] }` so we project the results into typed
/// items and keep the rest in `extra` for forward compatibility.
#[derive(Debug, Clone, Deserialize)]
pub struct HybridSearchResponse {
    #[serde(default = "default_true")]
    pub ok: bool,
    #[serde(default)]
    pub results: Vec<HybridSearchItem>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

fn default_true() -> bool {
    true
}

/// A single hybrid search hit. The `properties` map and any auxiliary
/// fields RustyRed returns ride along on `extra` so callers can surface
/// new fields without a client-crate upgrade.
#[derive(Debug, Clone, Deserialize)]
pub struct HybridSearchItem {
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub score: Option<f32>,
    #[serde(default)]
    pub graph_distance: Option<f32>,
    #[serde(default)]
    pub vector_score: Option<f32>,
    #[serde(default)]
    pub properties: serde_json::Map<String, Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Body of the full-text search request. Matches RustyRed's
/// `FullTextSearchBody` in `crates/rustyred-server/src/router.rs`.
#[derive(Debug, Clone, Serialize)]
pub struct FullTextSearchRequest {
    /// Optional node label filter. When `None`, the designated default
    /// label applies on the server side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The designated full-text property to search. Required. Must match a
    /// `(label, property)` pair previously registered via
    /// `graph/fulltext/designate`.
    pub property: String,
    /// The query string. RustyRed tokenizes + matches this against the
    /// designated property's full-text index.
    pub query: String,
    /// Top-k.
    pub k: usize,
}

/// Body of the full-text search response. RustyRed returns
/// `{ ok, tenant, results: [{ node_id, score }] }`. Unlike the hybrid
/// response, full-text hits carry NO node payload, so [`FullTextHit`]
/// cannot expose a label/snippet/properties directly: callers hydrate via
/// [`Client::get_node`].
#[derive(Debug, Clone, Deserialize)]
pub struct FullTextSearchResponse {
    #[serde(default = "default_true")]
    pub ok: bool,
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub results: Vec<FullTextHit>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// A single full-text hit. RustyRed emits only `node_id` + `score`.
#[derive(Debug, Clone, Deserialize)]
pub struct FullTextHit {
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub score: Option<f32>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Body of the spatial bounding-box request. Matches RustyRed's
/// `SpatialBboxBody` in `crates/rustyred-server/src/router.rs`. All of
/// `label`, `lat_property`, and `lon_property` are required (the server
/// body struct declares them non-optional).
#[derive(Debug, Clone, Serialize)]
pub struct SpatialBboxRequest {
    pub label: String,
    pub lat_property: String,
    pub lon_property: String,
    pub min_lat: f64,
    pub min_lon: f64,
    pub max_lat: f64,
    pub max_lon: f64,
}

/// Body of the spatial bounding-box response. RustyRed returns
/// `{ ok, tenant, count, node_ids: [String] }`: an unranked node-id list
/// with no scores and no node payloads.
#[derive(Debug, Clone, Deserialize)]
pub struct SpatialBboxResponse {
    #[serde(default = "default_true")]
    pub ok: bool,
    #[serde(default)]
    pub count: usize,
    #[serde(default)]
    pub node_ids: Vec<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Response of `GET graph/nodes/:node_id`. RustyRed returns
/// `{ ok, node }` on a hit and a bare `404` on a miss. [`Client::get_node`]
/// maps the `404` to `{ ok: false, node: None }`.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeFetchResponse {
    #[serde(default = "default_true")]
    pub ok: bool,
    #[serde(default)]
    pub node: Option<NodeRecord>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// A node record as RustyRed serializes it (`graph_store::NodeRecord`).
/// We keep only the fields the civic-atlas resolver hydrates from
/// (`id`, `labels`, `properties`); the rest ride on `extra`.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeRecord {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub properties: serde_json::Map<String, Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Body of the crawl request (`POST /crawl`). Matches RustyRed's
/// `CrawlRouteBody`. `/crawl` is root-level so the tenant rides in the body.
#[derive(Debug, Clone, Serialize)]
pub struct CrawlRequest {
    pub tenant: String,
    pub seeds: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<CrawlBudget>,
}

/// Crawl budget. Matches RustyRed's `CrawlBudget`: ALL FOUR fields are required
/// server-side (no serde defaults), so omitting any one yields a `422`. When the
/// whole `budget` is omitted from the request body RustyRed applies its own
/// defaults (25 pages / 30s / depth 2 / 5 MB).
#[derive(Debug, Clone, Serialize)]
pub struct CrawlBudget {
    pub max_pages: usize,
    pub max_seconds: u64,
    pub max_depth: usize,
    pub max_bytes: usize,
}

/// Body of the crawl response. RustyRed returns
/// `{ ok, tenant, receipt, transaction, federation }`; the receipt fields ride
/// on `extra` for forward compatibility.
#[derive(Debug, Clone, Deserialize)]
pub struct CrawlResponse {
    #[serde(default = "default_true")]
    pub ok: bool,
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Body of the SERP response (`GET /search.json`). RustyRed returns
/// `{ ok, tenant, search: { hits, links, matched_count, kept_count, query } }`.
#[derive(Debug, Clone, Deserialize)]
pub struct SerpResponse {
    #[serde(default = "default_true")]
    pub ok: bool,
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub search: SerpSearch,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// The `search` block of a SERP response. `hits` are kept loose (`Value`) since
/// the crawl-substrate hit shape varies; callers map url/title/snippet keys
/// best-effort.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SerpSearch {
    #[serde(default)]
    pub hits: Vec<Value>,
    #[serde(default)]
    pub links: Vec<Value>,
    #[serde(default)]
    pub matched_count: usize,
    #[serde(default)]
    pub kept_count: usize,
    #[serde(default)]
    pub query: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

async fn make_upstream_error(response: reqwest::Response, path: &str) -> RustyRedError {
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<unable to read response body>".into());
    RustyRedError::Upstream {
        status,
        path: path.to_string(),
        body: body.chars().take(500).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_requires_url() {
        // Capture, clear, restore — keeps test hermetic if RUSTYRED_URL
        // happens to be set in the dev shell.
        let saved = std::env::var("RUSTYRED_URL").ok();
        std::env::remove_var("RUSTYRED_URL");
        let err = ClientConfig::from_env().expect_err("expected missing URL error");
        match err {
            RustyRedError::Config(msg) => assert!(msg.contains("RUSTYRED_URL")),
            other => panic!("unexpected error: {other:?}"),
        }
        if let Some(value) = saved {
            std::env::set_var("RUSTYRED_URL", value);
        }
    }

    #[test]
    fn hybrid_request_omits_empty_optionals() {
        let request = HybridSearchRequest {
            label: None,
            property: "embedding".into(),
            query: serde_json::json!("brick mills"),
            k: 5,
            graph_seeds: vec![],
            max_hops: 2,
            alpha: None,
            confidence_weighted_graph_distance: None,
            edge_type_weights: None,
        };
        let body = serde_json::to_string(&request).expect("serializable");
        assert!(!body.contains("label"));
        assert!(!body.contains("alpha"));
        assert!(body.contains("\"property\":\"embedding\""));
        assert!(body.contains("\"k\":5"));
    }

    #[test]
    fn fulltext_request_omits_label_when_none() {
        let request = FullTextSearchRequest {
            label: None,
            property: "name".into(),
            query: "carriage town".into(),
            k: 20,
        };
        let body = serde_json::to_string(&request).expect("serializable");
        assert!(!body.contains("label"));
        assert!(body.contains("\"property\":\"name\""));
        assert!(body.contains("\"query\":\"carriage town\""));
        assert!(body.contains("\"k\":20"));
    }

    #[test]
    fn spatial_bbox_request_serializes_bounds() {
        let request = SpatialBboxRequest {
            label: "Place".into(),
            lat_property: "lat".into(),
            lon_property: "lon".into(),
            min_lat: 42.9,
            min_lon: -83.8,
            max_lat: 43.1,
            max_lon: -83.6,
        };
        let body = serde_json::to_string(&request).expect("serializable");
        assert!(body.contains("\"label\":\"Place\""));
        assert!(body.contains("\"lat_property\":\"lat\""));
        assert!(body.contains("\"lon_property\":\"lon\""));
        assert!(body.contains("\"min_lat\":42.9"));
        assert!(body.contains("\"min_lon\":-83.8"));
        assert!(body.contains("\"max_lat\":43.1"));
        assert!(body.contains("\"max_lon\":-83.6"));
    }

    #[test]
    fn fulltext_response_parses_node_id_and_score() {
        let raw = serde_json::json!({
            "ok": true,
            "tenant": "flint",
            "results": [
                { "node_id": "node:1", "score": 0.83 },
                { "node_id": "node:2", "score": 0.41 },
            ],
        })
        .to_string();
        let parsed: FullTextSearchResponse =
            serde_json::from_str(&raw).expect("deserializable");
        assert!(parsed.ok);
        assert_eq!(parsed.tenant.as_deref(), Some("flint"));
        assert_eq!(parsed.results.len(), 2);
        assert_eq!(parsed.results[0].node_id.as_deref(), Some("node:1"));
        assert_eq!(parsed.results[0].score, Some(0.83));
    }
}
