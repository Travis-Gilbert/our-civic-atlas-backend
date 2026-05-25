//! GraphQL types and resolvers for the civicResearch mutation.
//!
//! Mirrors the sidecar's `civicResearch(input: CivicResearchInput!)`
//! mutation (apps/graphql-server/src/schema.ts:1351) and runs in-process:
//! resolver constructs a CivicAtlasGrpcService request and dispatches
//! through the existing `civic_research` handler in lib.rs, which
//! connects to the Theseus harness via the bridge URL.
//!
//! Honest semantics: when THESEUS_BRIDGE_URL is unset the underlying
//! gRPC handler returns Status::unavailable. That surfaces here as a
//! GraphQL error and propagates to the frontend, which renders it as
//! an honest "research currently unavailable" state.
//!
//! `parse_search_results` is a faithful Rust port of the sidecar's
//! parseSearchResults function (schema.ts:230). It coerces the
//! orchestrator's JSON payload into the typed SearchResults shape,
//! including the derived signal/source blending the sidecar does for
//! priorKnowledge + newEvidence + gapClosures.

use async_graphql::{Context, InputObject, Object, SimpleObject};
use civic_atlas_types::civic_atlas::v1::{
    civic_atlas_service_server::CivicAtlasService, CivicResearchRequest, TenantContext,
};
use serde_json::Value;
use tonic::Request;

use crate::graphql::reconstruction::{HistoricalReconstruction, ReconstructionFootprint};
use crate::graphql::search::{
    Place, PlaceRef, SearchResults, Signal, Source, SpatialEvent, TimeRange,
};
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
    /// Evidence the orchestrator returned, parsed from the typed
    /// places / signals / events / historicalReconstructions / sources
    /// arrays plus derived signals/sources from priorKnowledge +
    /// newEvidence + gapClosures.
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

// ---------------------------------------------------------------------------
// JSON coercion helpers. Mirror the closures in sidecar parseSearchResults.
// ---------------------------------------------------------------------------

fn obj(value: &Value) -> &serde_json::Map<String, Value> {
    static EMPTY: std::sync::OnceLock<serde_json::Map<String, Value>> = std::sync::OnceLock::new();
    value
        .as_object()
        .unwrap_or_else(|| EMPTY.get_or_init(serde_json::Map::new))
}

fn arr<'a>(parsed: &'a Value, key: &str) -> &'a [Value] {
    static EMPTY: &[Value] = &[];
    parsed
        .get(key)
        .and_then(|v| v.as_array().map(|a| a.as_slice()))
        .unwrap_or(EMPTY)
}

fn nullable_text(value: Option<&Value>) -> Option<String> {
    match value.and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => Some(s.to_string()),
        _ => None,
    }
}

/// Returns the first non-empty string from a list of candidate JSON
/// keys (camelCase + snake_case fallbacks). Mirrors `item.foo ?? item.bar`.
fn first_str(item: &serde_json::Map<String, Value>, keys: &[&str], fallback: &str) -> String {
    for key in keys {
        if let Some(value) = item.get(*key) {
            if let Some(s) = value.as_str() {
                if !s.trim().is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    fallback.to_string()
}

fn first_nullable_str(item: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = item.get(*key) {
            if let Some(s) = value.as_str() {
                if !s.trim().is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

fn number(value: Option<&Value>, fallback: f64) -> f64 {
    value
        .and_then(|v| v.as_f64())
        .filter(|n| n.is_finite())
        .unwrap_or(fallback)
}

fn first_number(item: &serde_json::Map<String, Value>, keys: &[&str], fallback: f64) -> f64 {
    for key in keys {
        if let Some(value) = item.get(*key) {
            if let Some(n) = value.as_f64() {
                if n.is_finite() {
                    return n;
                }
            }
        }
    }
    fallback
}

fn first_nullable_number(item: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(value) = item.get(*key) {
            if let Some(n) = value.as_f64() {
                if n.is_finite() {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn coordinate_pair(value: Option<&Value>) -> Option<[f64; 2]> {
    let arr = value?.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    let lng = arr[0].as_f64()?;
    let lat = arr[1].as_f64()?;
    if lng.is_finite() && lat.is_finite() {
        Some([lng, lat])
    } else {
        None
    }
}

fn footprint_from(value: Option<&Value>) -> ReconstructionFootprint {
    let m = value.map(obj);
    let width = m
        .map(|map| first_number(map, &["widthMeters", "width_m"], 0.0))
        .unwrap_or(0.0);
    let depth = m
        .map(|map| first_number(map, &["depthMeters", "depth_m"], 0.0))
        .unwrap_or(0.0);
    ReconstructionFootprint {
        width_meters: width,
        depth_meters: depth,
    }
}

fn place_ref_from(value: Option<&Value>) -> Option<PlaceRef> {
    let item = value?.as_object()?;
    let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() || name.is_empty() {
        None
    } else {
        Some(PlaceRef {
            id: id.to_string(),
            name: name.to_string(),
        })
    }
}

fn time_range_from(value: Option<&Value>) -> Option<TimeRange> {
    let item = value?.as_object()?;
    Some(TimeRange {
        start: nullable_text(item.get("start")),
        end: nullable_text(item.get("end")),
        label: nullable_text(item.get("label")),
    })
}

// ---------------------------------------------------------------------------
// parseSearchResults port
// ---------------------------------------------------------------------------

/// Port of sidecar parseSearchResults. Coerces the orchestrator JSON
/// into the typed SearchResults shape, deriving extra signals/sources
/// from priorKnowledge + newEvidence + gapClosures so the same atelier
/// surface renders identical content whether traffic goes through the
/// sidecar or this Axum-native path.
pub fn parse_search_results(results_json: &str, query: &str) -> SearchResults {
    let parsed: Value = if results_json.trim().is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(results_json).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
    };
    let parsed_obj = parsed.as_object();

    let places: Vec<Place> = arr(&parsed, "places")
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let item = obj(value);
            Place {
                id: first_str(item, &["id"], &format!("place:{idx}")),
                name: first_str(item, &["name", "label"], "Unknown place"),
                place_type: first_str(item, &["placeType", "kind"], "place"),
                centroid: coordinate_pair(item.get("centroid")).map(async_graphql::Json),
                confidence: first_number(item, &["confidence"], 0.0),
                temporal_status: first_str(item, &["temporalStatus"], "unknown"),
            }
        })
        .collect();

    let typed_signals: Vec<Signal> = arr(&parsed, "signals")
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let item = obj(value);
            Signal {
                id: first_str(item, &["id"], &format!("signal:{idx}")),
                signal_kind: first_str(item, &["signalKind", "kind"], "civic_research"),
                title: first_str(item, &["title", "label"], "Research signal"),
                summary: first_str(item, &["summary", "snippet"], ""),
                published_at: first_nullable_str(item, &["publishedAt"]),
                relative_time_label: first_nullable_str(item, &["relativeTimeLabel"]),
                confidence: first_number(item, &["confidence"], 0.0),
                place: place_ref_from(item.get("place")),
            }
        })
        .collect();

    let typed_events: Vec<SpatialEvent> = arr(&parsed, "events")
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let item = obj(value);
            SpatialEvent {
                id: first_str(item, &["id"], &format!("event:{idx}")),
                title: first_str(item, &["title", "label"], "Research event"),
                summary: first_str(item, &["summary", "snippet"], ""),
                occurred_at: first_nullable_str(item, &["occurredAt"]),
                confidence: first_number(item, &["confidence"], 0.0),
                place: place_ref_from(item.get("place")),
            }
        })
        .collect();

    let typed_reconstructions: Vec<HistoricalReconstruction> = arr(&parsed, "historicalReconstructions")
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let item = obj(value);
            HistoricalReconstruction {
                id: first_str(item, &["id"], &format!("reconstruction:{idx}")),
                civic_object_id: first_str(
                    item,
                    &["civicObjectId", "civic_object_id"],
                    &format!("reconstruction:{idx}"),
                ),
                name: first_str(item, &["name", "label"], "Research reconstruction"),
                description: first_str(item, &["description", "snippet"], ""),
                position: coordinate_pair(item.get("position")).map(async_graphql::Json),
                footprint: footprint_from(item.get("footprint")),
                height_meters: first_number(item, &["heightMeters", "height_m"], 0.0),
                bearing_degrees: first_number(item, &["bearingDegrees", "bearing_deg"], 0.0),
                confidence: first_number(item, &["confidence"], 0.0),
                facade_confidence: first_nullable_number(item, &["facadeConfidence", "facade_confidence"]),
                roof_confidence: first_nullable_number(item, &["roofConfidence", "roof_confidence"]),
                ground_floor_confidence: first_nullable_number(
                    item,
                    &["groundFloorConfidence", "ground_floor_confidence"],
                ),
                roof_form: first_nullable_str(item, &["roofForm", "roof_form"]),
                time_start: first_nullable_str(item, &["timeStart"]),
                time_end: first_nullable_str(item, &["timeEnd"]),
                geometry_url: first_nullable_str(item, &["geometryUrl", "geometry_url"]),
                geometry_format: first_nullable_str(item, &["geometryFormat", "geometry_format"]),
                foundry_asset_url: first_nullable_str(
                    item,
                    &["foundryAssetUrl", "foundry_asset_url"],
                ),
                sources: Vec::new(),
            }
        })
        .collect();

    let typed_sources: Vec<Source> = arr(&parsed, "sources")
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let item = obj(value);
            Source {
                id: first_str(item, &["id"], &format!("source:{idx}")),
                name: first_str(item, &["name", "url", "source"], "Research source"),
                homepage_url: first_nullable_str(item, &["homepageUrl", "homepage_url", "url"]),
                source_type: first_str(item, &["sourceType", "source"], "research"),
                public_use_terms: first_nullable_str(item, &["publicUseTerms", "public_use_terms"]),
                trust_tier: first_str(item, &["trustTier"], "reviewable"),
                last_checked: first_nullable_str(item, &["lastChecked", "last_checked"]),
                known_limits: string_array(item.get("knownLimits").or_else(|| item.get("known_limits"))),
                contains_personal_data: item
                    .get("containsPersonalData")
                    .or_else(|| item.get("contains_personal_data"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            }
        })
        .collect();

    // Derive extra signals and sources from priorKnowledge + newEvidence
    // (orchestrator-returned search results not yet bucketed into the
    // typed lists above) and from gapClosures (which surface as signals
    // describing where the orchestrator could not yet find evidence).
    let mut search_evidence: Vec<(String, Signal, String, String)> = Vec::new();
    for (idx, value) in arr(&parsed, "priorKnowledge")
        .iter()
        .chain(arr(&parsed, "newEvidence").iter())
        .enumerate()
    {
        let item = obj(value);
        let id = first_str(item, &["resultId", "id"], &format!("research:{idx}"));
        let signal = Signal {
            id: id.clone(),
            signal_kind: first_str(item, &["kind"], "civic_research"),
            title: first_str(item, &["label", "title"], "Research result"),
            summary: first_str(item, &["snippet", "summary"], ""),
            published_at: None,
            relative_time_label: None,
            confidence: first_number(
                item,
                &["confidence"],
                first_number(item, &["relevanceScore"], 0.0),
            ),
            place: None,
        };
        let source = first_str(item, &["source"], "");
        let url = first_str(item, &["url"], "");
        search_evidence.push((id, signal, source, url));
    }

    let gap_signals: Vec<Signal> = arr(&parsed, "gapClosures")
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let item = obj(value);
            let gap_id = first_str(item, &["gapId", "id"], &format!("gap:{idx}"));
            let description = first_str(item, &["description", "summary"], "");
            let closed = item
                .get("closed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let is_config_missing = gap_id.contains("rustyred_unconfigured")
                || description
                    .to_ascii_lowercase()
                    .contains("rustyred_unconfigured");
            let title = if is_config_missing {
                "Research sources are not connected yet".to_string()
            } else if closed {
                "Research gap closed".to_string()
            } else {
                "Research needs more source data".to_string()
            };
            let summary = if !description.is_empty() {
                description
            } else if closed {
                "Civic research closed a source gap.".to_string()
            } else {
                "Civic research could not expand this query yet.".to_string()
            };
            Signal {
                id: format!("gap:{gap_id}"),
                signal_kind: if closed {
                    "gap_closure".to_string()
                } else {
                    "research_status".to_string()
                },
                title,
                summary,
                published_at: None,
                relative_time_label: None,
                confidence: 0.0,
                place: None,
            }
        })
        .collect();

    let evidence_sources: Vec<Source> = search_evidence
        .iter()
        .filter(|(_, _, source, url)| !source.is_empty() || !url.is_empty())
        .enumerate()
        .map(|(idx, (id, _signal, source, url))| {
            let fallback_id = format!("research-source:{}", if id.is_empty() { idx.to_string() } else { id.clone() });
            Source {
                id: fallback_id,
                name: if !url.is_empty() {
                    url.clone()
                } else if !source.is_empty() {
                    source.clone()
                } else {
                    "Research source".to_string()
                },
                homepage_url: if !url.is_empty() { Some(url.clone()) } else { None },
                source_type: if !source.is_empty() {
                    source.clone()
                } else {
                    "research".to_string()
                },
                public_use_terms: None,
                trust_tier: "reviewable".to_string(),
                last_checked: None,
                known_limits: Vec::new(),
                contains_personal_data: false,
            }
        })
        .collect();

    let mut signals: Vec<Signal> = typed_signals;
    signals.extend(gap_signals);
    signals.extend(search_evidence.into_iter().map(|(_, signal, _, _)| signal));

    let mut sources: Vec<Source> = typed_sources;
    sources.extend(evidence_sources);

    let derived_total = (places.len() + signals.len() + typed_events.len() + typed_reconstructions.len()) as i64;
    let total_result_count = parsed_obj
        .and_then(|o| o.get("totalResultCount").or_else(|| o.get("totalReturned")))
        .and_then(|v| v.as_i64())
        .unwrap_or(derived_total);

    let query_string = parsed_obj
        .and_then(|o| o.get("query"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| query.to_string());

    SearchResults {
        query: query_string,
        total_result_count: total_result_count as i32,
        reranked: parsed_obj
            .and_then(|o| o.get("reranked"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        accepted_confidence_floor: number(
            parsed_obj.and_then(|o| o.get("acceptedConfidenceFloor")),
            0.0,
        ),
        inferred_time_range: time_range_from(parsed_obj.and_then(|o| o.get("inferredTimeRange"))),
        places,
        signals,
        events: typed_events,
        historical_reconstructions: typed_reconstructions,
        sources,
    }
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

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

    let results = parse_search_results(&response.results_json, &query);

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
    /// orchestrator run id, the skill that ran, and the typed
    /// SearchResults shape (parsed from the orchestrator's results_json
    /// payload, with derived signals/sources from priorKnowledge +
    /// newEvidence + gapClosures matching the sidecar's parse).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_json_returns_default_shape_with_query() {
        let results = parse_search_results("", "carriage town storefront");
        assert_eq!(results.query, "carriage town storefront");
        assert_eq!(results.total_result_count, 0);
        assert!(!results.reranked);
        assert_eq!(results.places.len(), 0);
        assert_eq!(results.signals.len(), 0);
        assert_eq!(results.events.len(), 0);
        assert_eq!(results.historical_reconstructions.len(), 0);
        assert_eq!(results.sources.len(), 0);
    }

    #[test]
    fn parses_typed_places_with_centroid_and_confidence() {
        let json = r#"{
            "places": [
                { "id": "place:1", "name": "Carriage Town", "placeType": "neighborhood",
                  "centroid": [-83.6995, 43.0125], "confidence": 0.82,
                  "temporalStatus": "historical" }
            ]
        }"#;
        let results = parse_search_results(json, "carriage town");
        assert_eq!(results.places.len(), 1);
        let place = &results.places[0];
        assert_eq!(place.id, "place:1");
        assert_eq!(place.name, "Carriage Town");
        assert_eq!(place.place_type, "neighborhood");
        assert!(place.centroid.is_some());
        assert!((place.confidence - 0.82).abs() < 1e-9);
        assert_eq!(place.temporal_status, "historical");
    }

    #[test]
    fn derives_signals_from_prior_knowledge_and_new_evidence() {
        let json = r#"{
            "priorKnowledge": [
                { "id": "pk:1", "label": "Sanborn 1899 sheet S18", "snippet": "Storefront on E Kearsley",
                  "confidence": 0.7, "source": "loc.gov", "url": "https://loc.gov/..." }
            ],
            "newEvidence": [
                { "id": "ne:1", "label": "Sloan archive photo", "snippet": "Circa 1925",
                  "confidence": 0.65, "source": "sloan", "url": "https://sloan.org/..." }
            ]
        }"#;
        let results = parse_search_results(json, "");
        assert_eq!(results.signals.len(), 2);
        assert!(results.sources.iter().any(|s| s.id.starts_with("research-source:")));
    }

    #[test]
    fn gap_closures_with_rustyred_unconfigured_render_config_signal() {
        let json = r#"{
            "gapClosures": [
                { "gapId": "rustyred_unconfigured.providers",
                  "description": "RUSTYRED_PROVIDERS not set",
                  "closed": false }
            ]
        }"#;
        let results = parse_search_results(json, "");
        assert_eq!(results.signals.len(), 1);
        let signal = &results.signals[0];
        assert_eq!(signal.signal_kind, "research_status");
        assert_eq!(signal.title, "Research sources are not connected yet");
    }
}
