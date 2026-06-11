//! Axum-native GraphQL surface.
//!
//! Replaces the Node sidecar at `apps/graphql-server` as the GraphQL
//! boundary the Civic Atlas frontend talks to. The frontend posts
//! GraphQL operations to `/graphql` on this Axum process, and resolvers
//! call the existing internal services (ReconstructionGrpcService,
//! CivicAtlasGrpcService, etc.) directly without crossing a network hop.
//!
//! Phase: vertical slice. The first commit ships only enough surface
//! to prove the endpoint works end-to-end. Subsequent commits port the
//! sidecar's remaining resolvers across (reconstructionDossier,
//! civicResearch, places, events, scenarios, dossierFor, ...).
//!
//! Architecture rationale lives in the project CLAUDE.md: the frontend
//! talks one boundary (GraphQL), tenant-scoped service credentials live
//! on Axum (never in the frontend or a Node sidecar), and capability
//! growth happens as new schema fields whose resolvers run here.

pub mod civic_research;
pub mod event_planner;
pub mod layer;
pub mod query;
pub mod reconstruction;
pub mod search;
pub mod traffic;

use async_graphql::{
    EmptySubscription, MergedObject, Request as GraphQLRequest, Response as GraphQLResponse, Schema,
};
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::env;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::warn;

use crate::{event_planner_auth, AtlasState};
use civic_research::MutationRoot as CivicResearchMutationRoot;
use event_planner::{EventPlannerMutation, PlannerActor};
use query::QueryRoot;

const PLANNER_SESSION_COOKIE: &str = "porchfest_planner_session";

#[derive(MergedObject, Default)]
pub struct RootMutation(CivicResearchMutationRoot, EventPlannerMutation);

/// The composed GraphQL schema served from this process.
///
/// EmptySubscription is a placeholder until streaming surfaces port
/// across (engine job progress, civic research as it resolves).
pub type CivicAtlasSchema = Schema<QueryRoot, RootMutation, EmptySubscription>;

/// Construct the GraphQL schema with the AtlasState available to every
/// resolver via async-graphql's typed context.
pub fn build_schema(state: AtlasState) -> CivicAtlasSchema {
    Schema::build(
        QueryRoot::default(),
        RootMutation::default(),
        EmptySubscription,
    )
    .data(state)
    .finish()
}

/// Mount the GraphQL endpoint on a Router under `/graphql`. async-graphql-axum
/// 7.x exposes the schema as a tower Service, so we use `route_service`
/// rather than a stateful handler. The schema clones cheaply because its
/// inner Arc is cheap to clone per request.
///
/// CorsLayer is applied so the browser-side Civic Atlas frontend at
/// http://localhost:3000 (dev) or its production origin can post
/// GraphQL operations directly. Configure allowed origins via the
/// CIVIC_ATLAS_GRAPHQL_ALLOWED_ORIGINS env var (comma-separated list);
/// the default permits any origin in dev mode for ergonomic local
/// iteration. Production deployments MUST set the explicit allowlist.
///
/// GraphiQL playground is not mounted here: enabling it requires the
/// `graphiql` feature which pulls handlebars as a heavyweight dep. For
/// ad-hoc exploration use curl, the urql codegen output, or a desktop
/// GraphQL client pointed at /graphql.
pub fn graphql_router(state: AtlasState) -> Router {
    let schema = build_schema(state.clone());
    let http_state = GraphqlHttpState {
        schema,
        state: state.clone(),
    };

    Router::new()
        .route("/graphql", post(graphql_handler))
        .route("/auth/claim", get(auth_claim_get).post(auth_claim_post))
        .route(
            "/auth/sign-out",
            get(auth_sign_out_get).post(auth_sign_out_post),
        )
        .with_state(http_state)
        .layer(cors_layer())
}

fn cors_layer() -> CorsLayer {
    let allowed = env::var("CIVIC_ATLAS_GRAPHQL_ALLOWED_ORIGINS").ok();
    let mut layer = CorsLayer::new()
        .allow_credentials(true)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            HeaderName::from_static("x-requested-with"),
        ]);
    match allowed {
        Some(list) if !list.trim().is_empty() => {
            let origins: Vec<HeaderValue> = list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .filter_map(|s| HeaderValue::from_str(s).ok())
                .collect();
            if !origins.is_empty() {
                layer = layer.allow_origin(origins);
            }
        }
        _ => {
            // Dev/default fallback mirrors the request origin so credentialed
            // browser calls work without using an invalid wildcard CORS header.
            // Production should set CIVIC_ATLAS_GRAPHQL_ALLOWED_ORIGINS.
            layer = layer.allow_origin(AllowOrigin::mirror_request());
        }
    }
    layer
}

#[derive(Clone)]
struct GraphqlHttpState {
    schema: CivicAtlasSchema,
    state: AtlasState,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthClaimQuery {
    token: String,
    return_to: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthClaimBody {
    token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthSignOutQuery {
    return_to: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthClaimResponse {
    success: bool,
    user_id: String,
    display_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthErrorResponse {
    error: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthSignOutResponse {
    signed_out: bool,
}

async fn graphql_handler(
    State(http_state): State<GraphqlHttpState>,
    headers: HeaderMap,
    Json(request): Json<GraphQLRequest>,
) -> Json<GraphQLResponse> {
    let mut request = request;
    if let Some(actor) = resolve_planner_actor(&http_state.state, &headers).await {
        request = request.data(actor);
    }
    Json(http_state.schema.execute(request).await)
}

async fn auth_claim_get(
    State(http_state): State<GraphqlHttpState>,
    Query(query): Query<AuthClaimQuery>,
) -> Response {
    match claim_planner_session(&http_state.state, &query.token).await {
        Ok(claimed) => {
            let mut response = match safe_return_to(query.return_to.as_deref()) {
                Some(return_to) => Redirect::temporary(&return_to).into_response(),
                None => Json(AuthClaimResponse {
                    success: true,
                    user_id: claimed.user_id,
                    display_name: claimed.display_name,
                })
                .into_response(),
            };
            set_session_cookie_header(&mut response, &claimed.session_token, 60 * 60 * 24 * 30);
            response
        }
        Err(response) => response,
    }
}

async fn auth_claim_post(
    State(http_state): State<GraphqlHttpState>,
    Json(body): Json<AuthClaimBody>,
) -> Response {
    match claim_planner_session(&http_state.state, &body.token).await {
        Ok(claimed) => {
            let mut response = Json(AuthClaimResponse {
                success: true,
                user_id: claimed.user_id,
                display_name: claimed.display_name,
            })
            .into_response();
            set_session_cookie_header(&mut response, &claimed.session_token, 60 * 60 * 24 * 30);
            response
        }
        Err(response) => response,
    }
}

async fn auth_sign_out_get(
    State(http_state): State<GraphqlHttpState>,
    headers: HeaderMap,
    Query(query): Query<AuthSignOutQuery>,
) -> Response {
    revoke_planner_session(&http_state.state, &headers).await;
    let mut response = match safe_return_to(query.return_to.as_deref()) {
        Some(return_to) => Redirect::temporary(&return_to).into_response(),
        None => Json(AuthSignOutResponse { signed_out: true }).into_response(),
    };
    set_session_cookie_header(&mut response, "", 0);
    response
}

async fn auth_sign_out_post(
    State(http_state): State<GraphqlHttpState>,
    headers: HeaderMap,
) -> Response {
    revoke_planner_session(&http_state.state, &headers).await;
    let mut response = Json(AuthSignOutResponse { signed_out: true }).into_response();
    set_session_cookie_header(&mut response, "", 0);
    response
}

struct ClaimedPlannerSession {
    user_id: String,
    display_name: String,
    session_token: String,
}

async fn claim_planner_session(
    state: &AtlasState,
    token: &str,
) -> Result<ClaimedPlannerSession, Response> {
    let Some(pool) = state.db_pool() else {
        return Err(auth_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "DATABASE_URL is required for planner auth",
        ));
    };
    if token.trim().is_empty() {
        return Err(auth_error(StatusCode::BAD_REQUEST, "token is required"));
    }
    let claimed = event_planner_auth::claim_invite(pool, token)
        .await
        .map_err(|status| auth_error(StatusCode::INTERNAL_SERVER_ERROR, status.message()))?;

    let Some(claimed) = claimed else {
        return Err(auth_error(
            StatusCode::UNAUTHORIZED,
            "magic link is invalid, expired, or already used",
        ));
    };

    Ok(ClaimedPlannerSession {
        user_id: claimed.user_id.to_string(),
        display_name: claimed.display_name,
        session_token: claimed.session_token,
    })
}

async fn resolve_planner_actor(state: &AtlasState, headers: &HeaderMap) -> Option<PlannerActor> {
    let token = parse_cookie(headers, PLANNER_SESSION_COOKIE)?;
    let pool = state.db_pool()?;
    match event_planner_auth::resolve_session(pool, &token).await {
        Ok(Some(session)) => Some(PlannerActor {
            user_id: session.user_id.to_string(),
        }),
        Ok(None) => None,
        Err(status) => {
            warn!(
                message = status.message(),
                "planner session resolution failed"
            );
            None
        }
    }
}

async fn revoke_planner_session(state: &AtlasState, headers: &HeaderMap) {
    let Some(token) = parse_cookie(headers, PLANNER_SESSION_COOKIE) else {
        return;
    };
    let Some(pool) = state.db_pool() else {
        return;
    };
    if let Err(status) = event_planner_auth::revoke_session(pool, &token).await {
        warn!(message = status.message(), "planner session revoke failed");
    }
}

fn parse_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in header.split(';') {
        let Some((candidate_name, value)) = part.trim().split_once('=') else {
            continue;
        };
        if candidate_name.trim() == name {
            return Some(value.to_string());
        }
    }
    None
}

fn set_session_cookie_header(response: &mut Response, value: &str, max_age_seconds: i64) {
    let cookie = format!(
        "{PLANNER_SESSION_COOKIE}={value}; Path=/; HttpOnly; Secure; SameSite=None; Max-Age={max_age_seconds}"
    );
    if let Ok(header_value) = HeaderValue::from_str(&cookie) {
        response
            .headers_mut()
            .insert(header::SET_COOKIE, header_value);
    }
}

fn auth_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(AuthErrorResponse {
            error: message.to_string(),
        }),
    )
        .into_response()
}

fn safe_return_to(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if return_to_allowed(value) {
        Some(value.to_string())
    } else {
        None
    }
}

fn return_to_allowed(value: &str) -> bool {
    if value.starts_with("http://localhost:") || value.starts_with("http://127.0.0.1:") {
        return true;
    }

    let configured = env::var("CIVIC_ATLAS_AUTH_RETURN_ORIGINS").ok();
    let allowed_origins: Vec<String> = configured
        .as_deref()
        .unwrap_or(
            "https://flint.ourcivicatlas.org,\
             https://porchfest.ourcivicatlas.org,\
             https://ourcivicatlas.org,\
             https://www.ourcivicatlas.org",
        )
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| origin.trim_end_matches('/').to_string())
        .collect();

    allowed_origins
        .iter()
        .any(|origin| value == origin || value.starts_with(&format!("{origin}/")))
}

impl Default for CivicResearchMutationRoot {
    fn default() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_builds_with_event_planner_fields() {
        let state = AtlasState::from_env().expect("fixture AtlasState builds");
        let sdl = build_schema(state).sdl();

        assert!(sdl.contains("eventLayers"));
        assert!(sdl.contains("eventApplications"));
        assert!(sdl.contains("submitEventApplication"));
        assert!(sdl.contains("submittedAtMs"));
        assert!(sdl.contains("placements"));
        assert!(sdl.contains("createPlacement"));
        assert!(sdl.contains("updatePlacement"));
    }

    #[test]
    fn schema_builds_with_traffic_fields() {
        let state = AtlasState::from_env().expect("fixture AtlasState builds");
        let sdl = build_schema(state).sdl();

        assert!(sdl.contains("trafficRealtime"));
        assert!(sdl.contains("TrafficRealtimeSnapshot"));
        assert!(sdl.contains("estimateBasis"));
        assert!(sdl.contains("FIXTURE_FALLBACK"));
    }

    #[test]
    fn schema_builds_with_layer_contract_fields() {
        let state = AtlasState::from_env().expect("fixture AtlasState builds");
        let sdl = build_schema(state).sdl();

        assert!(sdl.contains("type Layer"));
        assert!(sdl.contains("type LayerView"));
        assert!(sdl.contains("type LayerRecipe"));
        assert!(sdl.contains("enum LayerKind"));
        assert!(sdl.contains("layers("));
        assert!(sdl.contains("layerView("));
    }

    #[tokio::test]
    async fn traffic_layer_view_matches_traffic_snapshot_count() {
        let state = AtlasState::from_env().expect("fixture AtlasState builds");
        let schema = build_schema(state);
        let response = schema
            .execute(
                r#"
                {
                  trafficRealtime(networkId: "flint-downtown") {
                    status
                    summary { segmentCount }
                  }
                  layerView(layerId: "layer:traffic:flint-downtown") {
                    status
                    summary { recordCount }
                    records { id confidence visibility reviewStatus }
                  }
                }
                "#,
            )
            .await;
        assert!(
            response.errors.is_empty(),
            "unexpected GraphQL errors: {:?}",
            response.errors
        );
        let data = response.data.into_json().expect("GraphQL data to JSON");
        let traffic_count = data["trafficRealtime"]["summary"]["segmentCount"]
            .as_i64()
            .expect("traffic segmentCount");
        let layer_count = data["layerView"]["summary"]["recordCount"]
            .as_i64()
            .expect("layer recordCount");

        assert_eq!(data["trafficRealtime"]["status"], "FIXTURE_FALLBACK");
        assert_eq!(data["layerView"]["status"], "FIXTURE");
        assert_eq!(traffic_count, layer_count);
        assert_eq!(
            data["layerView"]["records"]
                .as_array()
                .expect("layer records")
                .len() as i64,
            traffic_count
        );
    }
}
