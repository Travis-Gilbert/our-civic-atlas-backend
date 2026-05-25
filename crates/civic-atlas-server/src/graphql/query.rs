//! GraphQL Query root for the Axum-native surface.
//!
//! Vertical slice: ships a `version` field so we can verify the endpoint
//! is reachable and the schema is served correctly. Subsequent commits
//! port `reconstructionDossier`, `historicalReconstructions`, `places`,
//! `events`, `scenarios`, `dossierFor`, etc. from the sidecar.

use async_graphql::Object;

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    /// Identifies which GraphQL implementation answered. Used by the
    /// frontend and operations to confirm traffic is hitting Axum-native
    /// GraphQL rather than the legacy Node sidecar during the migration.
    async fn version(&self) -> &'static str {
        "axum-native-graphql-v0.1"
    }
}
