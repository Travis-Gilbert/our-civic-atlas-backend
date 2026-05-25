//! GraphQL types for the search results surface (SearchResults and its
//! element types: Place, Signal, SpatialEvent, Source, TimeRange).
//!
//! Used by the `civicResearch` mutation (graphql/civic_research.rs) and
//! eventually by `placesNear`, `sourcesFor`, and the other read paths
//! that surface mixed evidence sets.
//!
//! Field coverage in this module matches what the public frontend
//! queries fetch (src/lib/api/graphql/queries/civic-research.graphql).
//! These are deliberately POJO-style types: server-side defaults are
//! safe (id and name fall back to empty strings) so empty arrays of
//! these types never trigger field-resolver errors.

use async_graphql::SimpleObject;

#[derive(SimpleObject, Default, Clone)]
pub struct TimeRange {
    /// ISO 8601 timestamp marking the start of the inferred range.
    pub start: Option<String>,
    /// ISO 8601 timestamp marking the end of the inferred range.
    pub end: Option<String>,
    /// Human-readable label (e.g., "1920s").
    pub label: Option<String>,
}

#[derive(SimpleObject, Default, Clone)]
pub struct PlaceRef {
    pub id: String,
    pub name: String,
}

#[derive(SimpleObject, Default, Clone)]
pub struct Place {
    pub id: String,
    pub name: String,
    pub place_type: String,
    /// [longitude, latitude] tuple serialized as a JSON array.
    pub centroid: Option<async_graphql::Json<[f64; 2]>>,
    pub confidence: f64,
    pub temporal_status: String,
}

#[derive(SimpleObject, Default, Clone)]
pub struct Signal {
    pub id: String,
    pub signal_kind: String,
    pub title: String,
    pub summary: String,
    pub published_at: Option<String>,
    pub relative_time_label: Option<String>,
    pub confidence: f64,
    pub place: Option<PlaceRef>,
}

#[derive(SimpleObject, Default, Clone)]
pub struct SpatialEvent {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub occurred_at: Option<String>,
    pub confidence: f64,
    pub place: Option<PlaceRef>,
}

#[derive(SimpleObject, Default, Clone)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub homepage_url: Option<String>,
    pub source_type: String,
    pub public_use_terms: Option<String>,
    pub trust_tier: String,
    pub last_checked: Option<String>,
    pub known_limits: Vec<String>,
    pub contains_personal_data: bool,
}

#[derive(SimpleObject, Default, Clone)]
pub struct SearchResults {
    pub query: String,
    pub total_result_count: i32,
    pub reranked: bool,
    pub accepted_confidence_floor: f64,
    pub inferred_time_range: Option<TimeRange>,
    pub places: Vec<Place>,
    pub signals: Vec<Signal>,
    pub events: Vec<SpatialEvent>,
    pub historical_reconstructions: Vec<crate::graphql::reconstruction::HistoricalReconstruction>,
    pub sources: Vec<Source>,
}
