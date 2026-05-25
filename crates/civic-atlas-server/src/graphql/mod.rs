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

pub mod query;
pub mod reconstruction;

use async_graphql::{EmptyMutation, EmptySubscription, Schema};
use async_graphql_axum::GraphQL;
use axum::Router;

use crate::AtlasState;
use query::QueryRoot;

/// The composed GraphQL schema served from this process.
///
/// EmptyMutation and EmptySubscription are placeholders until civicResearch
/// (and other write paths) port across. Replace them when the first
/// mutation resolver lands.
pub type CivicAtlasSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

/// Construct the GraphQL schema with the AtlasState available to every
/// resolver via async-graphql's typed context.
pub fn build_schema(state: AtlasState) -> CivicAtlasSchema {
    Schema::build(QueryRoot::default(), EmptyMutation, EmptySubscription)
        .data(state)
        .finish()
}

/// Mount the GraphQL endpoint on a Router under `/graphql`. async-graphql-axum
/// 7.x exposes the schema as a tower Service, so we use `route_service`
/// rather than a stateful handler. The schema clones cheaply because its
/// inner Arc is cheap to clone per request.
///
/// GraphiQL playground is not mounted here: enabling it requires the
/// `graphiql` feature which pulls handlebars as a heavyweight dep. For
/// ad-hoc exploration use curl, the urql codegen output, or a desktop
/// GraphQL client pointed at /graphql.
pub fn graphql_router(state: AtlasState) -> Router {
    let schema = build_schema(state);
    Router::new().route_service("/graphql", GraphQL::new(schema))
}
