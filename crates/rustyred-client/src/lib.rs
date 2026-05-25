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
//! - Hybrid vector + graph search (HNSW + graph proximity blend) for
//!   civic-atlas search surfaces, including the `civicResearch` GraphQL
//!   mutation.
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
        let mut auth = HeaderValue::from_str(&bearer).map_err(|err| {
            RustyRedError::Config(format!("Invalid bearer token: {err}"))
        })?;
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
}
