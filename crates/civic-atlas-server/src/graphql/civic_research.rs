//! GraphQL types and resolvers for the civicResearch mutation.
//!
//! Mirrors the sidecar's `civicResearch(input: CivicResearchInput!)`
//! mutation (apps/graphql-server/src/schema.ts:1351). Runs in-process:
//! resolver constructs a CivicAtlasGrpcService request and dispatches
//! through the existing `civic_research` handler in lib.rs, which then
//! connects to the Theseus harness via the bridge URL and returns the
//! orchestrator's run id + skill name + results JSON.
//!
//! Honest semantics: when THESEUS_BRIDGE_URL is unset (e.g., local dev
//! without theseus running), the underlying gRPC handler returns
//! Status::unavailable. That surfaces here as a GraphQL error and
//! propagates to the frontend, which renders it as an honest
//! "research currently unavailable" state. The system functions
//! without theseus configured: empty research results are valid.
//!
//! Results parsing: this commit returns SearchResults with empty
//! element arrays and the scalar fields zeroed. The orchestrator's
//! results_json is preserved in the run identifier so the frontend
//! can still chain to /provenance/<runId> for inspection. A follow-up
//! commit parses results_json into the typed Place/Signal/Event/
//! Source/HistoricalReconstruction arrays.

use async_graphql::{Context, InputObject, Object, SimpleObject};
use civic_atlas_types::civic_atlas::v1::{
    civic_atlas_service_server::CivicAtlasService, CivicResearchRequest, TenantContext,
};
use tonic::Request;

use crate::graphql::search::SearchResults;
use crate::{AtlasState, CivicAtlasGrpcService};

#[derive(InputObject)]
pub struct CivicResearchInput {
    /// Free-text query passed verbatim to the Theseus orchestrator.
    pub query: String,
    /// Optional orchestrator knobs (top_k, min_confidence, source_pair,
    /// max_rounds, etc.). Forwarded as JSON because the orchestrator
    /// vocabulary evolves faster than the GraphQL schema.
    pub budget: Option<async_graphql::Json<serde_json::Value>>,
    /// Optional scope hints (era, bbox, place_ids, source_ids,
    /// min_confidence, providers). Same JSON-passthrough rationale.
    pub scope: Option<async_graphql::Json<serde_json::Value>>,
    /// Optional session id for cross-call caching and follow-up.
    pub session_id: Option<String>,
    /// Optional folio id when the call is part of a curated workspace.
    pub folio_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct CivicResearchPayload {
    /// Orchestrator trace identifier; correlates with /provenance/<runId>
    /// for replay and compare.
    pub run_id: String,
    /// Orchestrator mode that ran (e.g. "civic_atlas", "deep", "ask").
    pub skill: String,
    /// Evidence the orchestrator returned. Element arrays default to
    /// empty until the results_json parser lands in a follow-up commit.
    pub results: SearchResults,
}

fn default_tenant() -> String {
    std::env::var("CIVIC_ATLAS_DEFAULT_TENANT").unwrap_or_else(|_| "flint".to_string())
}

fn json_to_string(value: Option<async_graphql::Json<serde_json::Value>>) -> String {
    match value {
        Some(async_graphql::Json(v)) => v.to_string(),
        None => String::new(),
    }
}

/// Resolver for the civicResearch mutation. Validates input, dispatches
/// through CivicAtlasGrpcService::civic_research in-process, maps the
/// CivicResearchResponse back into the GraphQL payload shape.
pub async fn resolve_civic_research(
    state: &AtlasState,
    input: CivicResearchInput,
) -> async_graphql::Result<CivicResearchPayload> {
    let query = input.query.trim().to_string();
    if query.is_empty() {
        return Err(async_graphql::Error::new(
            "civicResearch: `query` must be a non-empty string.",
        ));
    }

    let tenant_id = default_tenant();
    let service = CivicAtlasGrpcService::new(state.clone());

    let mut request = Request::new(CivicResearchRequest {
        tenant_context: Some(TenantContext {
            tenant_id: tenant_id.clone(),
            atlas_node_id: format!("atlas:{tenant_id}"),
            metadata: Default::default(),
        }),
        query: query.clone(),
        budget_json: json_to_string(input.budget),
        scope_json: json_to_string(input.scope),
        session_id: input.session_id.unwrap_or_default(),
        folio_id: input.folio_id.unwrap_or_default(),
    });
    request.metadata_mut().insert(
        "x-atlas-tenant",
        tenant_id
            .parse()
            .map_err(|err| async_graphql::Error::new(format!("invalid tenant id: {err}")))?,
    );

    let response = service
        .civic_research(request)
        .await
        .map_err(|status| {
            async_graphql::Error::new(format!(
                "civicResearch failed: {} ({})",
                status.message(),
                status.code()
            ))
        })?
        .into_inner();

    let results = SearchResults {
        query,
        ..Default::default()
    };

    Ok(CivicResearchPayload {
        run_id: response.run_id,
        skill: response.skill,
        results,
    })
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    /// Queue a civic research run via the Theseus harness. Returns the
    /// orchestrator run id, the skill that ran, and the SearchResults
    /// shape (currently empty arrays; full parsing lands in a follow-up).
    async fn civic_research(
        &self,
        ctx: &Context<'_>,
        input: CivicResearchInput,
    ) -> async_graphql::Result<CivicResearchPayload> {
        let state = ctx
            .data::<AtlasState>()
            .map_err(|_| async_graphql::Error::new("AtlasState missing from GraphQL context"))?;
        resolve_civic_research(state, input).await
    }
}
