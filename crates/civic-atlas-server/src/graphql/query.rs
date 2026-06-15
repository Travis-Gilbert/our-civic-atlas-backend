//! GraphQL Query root for the Axum-native surface.
//!
//! Composed via async-graphql's MergedObject so each resolver family
//! (reconstruction, places, scenarios, events, dossier, ...) lives in
//! its own module and is merged into the public Query root here.
//!
//! Ships in this commit:
//!   - `version` — sanity field confirming Axum-native GraphQL answered
//!   - `reconstructionDossier(reconstructionId)` — atelier one-shot payload
//!
//! Next commits port: civicResearch (Mutation root), historicalReconstructions
//! list, places, events, scenarios, dossierFor.

use async_graphql::{MergedObject, Object};

use crate::graphql::event_email::EventEmailQuery;
use crate::graphql::event_planner::EventPlannerQuery;
use crate::graphql::event_set_time::EventSetTimeQuery;
use crate::graphql::google_workspace::GoogleWorkspaceQuery;
use crate::graphql::layer::LayerQuery;
use crate::graphql::reconstruction::ReconstructionQuery;
use crate::graphql::traffic::TrafficQuery;

pub struct CoreQuery;

#[Object]
impl CoreQuery {
    /// Identifies which GraphQL implementation answered. Used by the
    /// frontend and operations to confirm traffic is hitting Axum-native
    /// GraphQL rather than the legacy Node sidecar during the migration.
    async fn version(&self) -> &'static str {
        "axum-native-graphql-v0.1"
    }
}

#[derive(MergedObject, Default)]
pub struct QueryRoot(
    CoreQuery,
    ReconstructionQuery,
    EventPlannerQuery,
    EventEmailQuery,
    EventSetTimeQuery,
    GoogleWorkspaceQuery,
    TrafficQuery,
    LayerQuery,
);

impl Default for CoreQuery {
    fn default() -> Self {
        Self
    }
}

impl Default for ReconstructionQuery {
    fn default() -> Self {
        Self
    }
}
