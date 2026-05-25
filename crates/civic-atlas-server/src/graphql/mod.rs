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
pub mod query;
pub mod reconstruction;
pub mod search;

use async_graphql::{EmptySubscription, Schema};
use async_graphql_axum::GraphQL;
use axum::{http::HeaderValue, Router};
use std::env;
use tower_http::cors::{Any, CorsLayer};

use crate::AtlasState;
use civic_research::MutationRoot;
use query::QueryRoot;

/// The composed GraphQL schema served from this process.
///
/// EmptySubscription is a placeholder until streaming surfaces port
/// across (engine job progress, civic research as it resolves).
pub type CivicAtlasSchema = Schema<QueryRoot, MutationRoot, EmptySubscription>;

/// Construct the GraphQL schema with the AtlasState available to every
/// resolver via async-graphql's typed context.
pub fn build_schema(state: AtlasState) -> CivicAtlasSchema {
    Schema::build(QueryRoot::default(), MutationRoot, EmptySubscription)
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
    let schema = build_schema(state);
    Router::new()
        .route_service("/graphql", GraphQL::new(schema))
        .layer(cors_layer())
}

fn cors_layer() -> CorsLayer {
    let allowed = env::var("CIVIC_ATLAS_GRAPHQL_ALLOWED_ORIGINS").ok();
    let mut layer = CorsLayer::new()
        .allow_methods([axum::http::Method::POST, axum::http::Method::OPTIONS])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
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
            // Dev default: permissive. Production MUST set
            // CIVIC_ATLAS_GRAPHQL_ALLOWED_ORIGINS to lock this down.
            layer = layer.allow_origin(Any);
        }
    }
    layer
}
