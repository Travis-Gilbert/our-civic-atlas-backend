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
use chrono::{DateTime, Utc};
use civic_atlas_types::civic_atlas::v1::{
    civic_atlas_service_server::CivicAtlasService, CivicResearchRequest, PersistArtifactRequest,
    TenantContext,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
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

#[derive(InputObject)]
pub struct ResearchArtifactPromotionInput {
    /// Optional stable artifact key. When omitted the resolver derives one
    /// from run/source/title so repeat promotions upsert the same artifact.
    pub artifact_key: Option<String>,
    /// Harness run id returned by civicResearch.
    pub run_id: Option<String>,
    /// Source/result id selected from civicResearch results.
    pub source_id: Option<String>,
    /// Source family for reconstruction filtering (directory, archival_photo,
    /// map, text, etc.).
    pub source_type: String,
    /// Resident-readable source title.
    pub title: String,
    /// Canonical source URL when one exists.
    pub uri: Option<String>,
    /// Short citation or holding note.
    pub citation: Option<String>,
    /// Capture/publication time for the source itself.
    pub captured_at: Option<DateTime<Utc>>,
    /// JSON object persisted on artifacts.payload_jsonb.
    pub payload: Option<async_graphql::Json<serde_json::Value>>,
    /// Which reconstruction claim(s) this source helps: footprint,
    /// facade, ground_floor_use, date, contradiction, or other.
    pub source_use_tags: Option<Vec<String>>,
    /// Short human review note describing why this source was saved.
    pub source_use_note: Option<String>,
    /// Review state for the saved source. Defaults to
    /// accepted_for_reconstruction, which means durable input for the
    /// reconstruction engine, not public claim publication.
    pub review_state: Option<String>,
    /// UUID or parcel_key. Prefer parcel_key for resident research results.
    pub parcel_ref: Option<String>,
    /// Optional building UUID for already-resolved backend objects.
    pub building_id: Option<String>,
    /// Optional building-part UUID for already-resolved backend objects.
    pub building_part_id: Option<String>,
    /// Anchor family; defaults to `research`.
    pub anchor_kind: Option<String>,
    /// Optional WKT geometry anchor.
    pub anchor_geometry_wkt: Option<String>,
    /// Optional start of the source's applicability window.
    pub anchor_time_start: Option<DateTime<Utc>>,
    /// Optional end of the source's applicability window.
    pub anchor_time_end: Option<DateTime<Utc>>,
    /// JSON object persisted on artifact_anchors.payload_jsonb.
    pub anchor_payload: Option<async_graphql::Json<serde_json::Value>>,
}

#[derive(SimpleObject)]
pub struct ResearchArtifactPromotionPayload {
    pub artifact_id: String,
    pub artifact_key: String,
    pub status: String,
}

const DEFAULT_PROMOTION_REVIEW_STATE: &str = "accepted_for_reconstruction";
const ALLOWED_PROMOTION_REVIEW_STATES: &[&str] = &[
    "accepted_for_reconstruction",
    "pending_review",
    "rejected_for_reconstruction",
];
const ALLOWED_SOURCE_USE_TAGS: &[&str] = &[
    "footprint",
    "facade",
    "ground_floor_use",
    "date",
    "contradiction",
    "other",
];

fn default_tenant() -> String {
    std::env::var("CIVIC_ATLAS_DEFAULT_TENANT").unwrap_or_else(|_| "flint".to_string())
}

fn json_to_string(value: Option<async_graphql::Json<serde_json::Value>>) -> String {
    match value {
        Some(async_graphql::Json(v)) => v.to_string(),
        None => String::new(),
    }
}

fn required_trimmed(value: &str, field_name: &str) -> async_graphql::Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(async_graphql::Error::new(format!(
            "promoteResearchArtifact: `{field_name}` must be a non-empty string.",
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn optional_trimmed_string(value: &Option<String>) -> String {
    optional_trimmed(value.as_deref()).unwrap_or_default()
}

fn normalized_token(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| {
            if ch == '-' || ch == ' ' {
                '_'
            } else {
                ch.to_ascii_lowercase()
            }
        })
        .collect()
}

fn validate_allowed_token(
    field_name: &str,
    raw: &str,
    allowed: &[&str],
) -> async_graphql::Result<String> {
    let normalized = normalized_token(raw);
    if normalized.is_empty() {
        return Err(async_graphql::Error::new(format!(
            "promoteResearchArtifact: `{field_name}` must not contain an empty value.",
        )));
    }
    if allowed.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(async_graphql::Error::new(format!(
            "promoteResearchArtifact: `{field_name}` value `{raw}` is not supported. Allowed values: {}.",
            allowed.join(", ")
        )))
    }
}

fn normalized_review_state(
    input: &ResearchArtifactPromotionInput,
) -> async_graphql::Result<String> {
    match optional_trimmed(input.review_state.as_deref()) {
        Some(value) => {
            validate_allowed_token("reviewState", &value, ALLOWED_PROMOTION_REVIEW_STATES)
        }
        None => Ok(DEFAULT_PROMOTION_REVIEW_STATE.to_string()),
    }
}

fn normalized_source_use_tags(
    input: &ResearchArtifactPromotionInput,
) -> async_graphql::Result<Vec<String>> {
    let mut normalized_tags = Vec::new();
    for raw in input.source_use_tags.as_deref().unwrap_or_default() {
        let tag = validate_allowed_token("sourceUseTags", raw, ALLOWED_SOURCE_USE_TAGS)?;
        if !normalized_tags.iter().any(|seen| seen == &tag) {
            normalized_tags.push(tag);
        }
    }
    Ok(normalized_tags)
}

fn has_artifact_anchor(input: &ResearchArtifactPromotionInput) -> bool {
    optional_trimmed(input.parcel_ref.as_deref()).is_some()
        || optional_trimmed(input.building_id.as_deref()).is_some()
        || optional_trimmed(input.building_part_id.as_deref()).is_some()
        || optional_trimmed(input.anchor_geometry_wkt.as_deref()).is_some()
}

fn key_piece(value: Option<&str>) -> String {
    let piece: String = value
        .unwrap_or("source")
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else if ch == ':' || ch == '-' || ch == '_' {
                Some('-')
            } else {
                None
            }
        })
        .take(48)
        .collect();
    let trimmed = piece.trim_matches('-');
    if trimmed.is_empty() {
        "source".to_string()
    } else {
        trimmed.to_string()
    }
}

fn artifact_key_for(
    input: &ResearchArtifactPromotionInput,
    source_type: &str,
    title: &str,
) -> String {
    if let Some(key) = optional_trimmed(input.artifact_key.as_deref()) {
        return key;
    }

    let mut hasher = Sha256::new();
    hasher.update(optional_trimmed(input.run_id.as_deref()).unwrap_or_default());
    hasher.update("|");
    hasher.update(optional_trimmed(input.source_id.as_deref()).unwrap_or_default());
    hasher.update("|");
    hasher.update(optional_trimmed(input.uri.as_deref()).unwrap_or_default());
    hasher.update("|");
    hasher.update(source_type);
    hasher.update("|");
    hasher.update(title);
    let digest = format!("{:x}", hasher.finalize());
    format!(
        "research:{}:{}",
        key_piece(input.source_id.as_deref().or(input.run_id.as_deref())),
        &digest[..16],
    )
}

fn json_object_map(
    value: &Option<async_graphql::Json<serde_json::Value>>,
    field_name: &str,
) -> async_graphql::Result<Map<String, Value>> {
    match value {
        Some(async_graphql::Json(value)) => value.as_object().cloned().ok_or_else(|| {
            async_graphql::Error::new(format!(
                "promoteResearchArtifact: `{field_name}` must be a JSON object.",
            ))
        }),
        None => Ok(Map::new()),
    }
}

fn insert_optional_metadata(map: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if map.contains_key(key) {
        return;
    }
    if let Some(value) = optional_trimmed(value) {
        map.insert(key.to_string(), Value::String(value));
    }
}

fn metadata_object_mut<'a>(
    map: &'a mut Map<String, Value>,
    field_name: &str,
) -> async_graphql::Result<&'a mut Map<String, Value>> {
    map.entry("metadata".to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| {
            async_graphql::Error::new(format!(
                "promoteResearchArtifact: `{field_name}.metadata` must be a JSON object.",
            ))
        })
}

fn promotion_json_object(
    value: &Option<async_graphql::Json<serde_json::Value>>,
    field_name: &str,
    input: &ResearchArtifactPromotionInput,
    artifact_key: &str,
) -> async_graphql::Result<String> {
    let mut map = json_object_map(value, field_name)?;
    let review_state = normalized_review_state(input)?;
    let source_use_tags = normalized_source_use_tags(input)?;
    let source_use_note = optional_trimmed(input.source_use_note.as_deref());

    map.entry("promotionKind".to_string())
        .or_insert_with(|| Value::String("civicResearch".to_string()));
    map.entry("artifactKey".to_string())
        .or_insert_with(|| Value::String(artifact_key.to_string()));
    map.entry("reviewState".to_string())
        .or_insert_with(|| Value::String(review_state.clone()));
    if !source_use_tags.is_empty() && !map.contains_key("sourceUseTags") {
        map.insert(
            "sourceUseTags".to_string(),
            Value::Array(
                source_use_tags
                    .iter()
                    .map(|tag| Value::String(tag.clone()))
                    .collect(),
            ),
        );
    }
    if let Some(note) = source_use_note.as_deref() {
        insert_optional_metadata(&mut map, "sourceUseNote", Some(note));
    }
    insert_optional_metadata(&mut map, "runId", input.run_id.as_deref());
    insert_optional_metadata(&mut map, "sourceId", input.source_id.as_deref());

    let metadata = metadata_object_mut(&mut map, field_name)?;
    insert_optional_metadata(metadata, "reviewState", Some(&review_state));
    if !source_use_tags.is_empty() {
        insert_optional_metadata(metadata, "sourceUseTags", Some(&source_use_tags.join(",")));
    }
    if let Some(note) = source_use_note.as_deref() {
        insert_optional_metadata(metadata, "sourceUseNote", Some(note));
    }
    insert_optional_metadata(metadata, "runId", input.run_id.as_deref());
    insert_optional_metadata(metadata, "sourceId", input.source_id.as_deref());

    Ok(Value::Object(map).to_string())
}

fn millis(value: Option<DateTime<Utc>>) -> Option<i64> {
    value.map(|dt| dt.timestamp_millis())
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

    let typed_reconstructions: Vec<HistoricalReconstruction> =
        arr(&parsed, "historicalReconstructions")
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
                    facade_confidence: first_nullable_number(
                        item,
                        &["facadeConfidence", "facade_confidence"],
                    ),
                    roof_confidence: first_nullable_number(
                        item,
                        &["roofConfidence", "roof_confidence"],
                    ),
                    ground_floor_confidence: first_nullable_number(
                        item,
                        &["groundFloorConfidence", "ground_floor_confidence"],
                    ),
                    roof_form: first_nullable_str(item, &["roofForm", "roof_form"]),
                    time_start: first_nullable_str(item, &["timeStart"]),
                    time_end: first_nullable_str(item, &["timeEnd"]),
                    geometry_url: first_nullable_str(item, &["geometryUrl", "geometry_url"]),
                    geometry_format: first_nullable_str(
                        item,
                        &["geometryFormat", "geometry_format"],
                    ),
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
                known_limits: string_array(
                    item.get("knownLimits").or_else(|| item.get("known_limits")),
                ),
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
            let fallback_id = format!(
                "research-source:{}",
                if id.is_empty() {
                    idx.to_string()
                } else {
                    id.clone()
                }
            );
            Source {
                id: fallback_id,
                name: if !url.is_empty() {
                    url.clone()
                } else if !source.is_empty() {
                    source.clone()
                } else {
                    "Research source".to_string()
                },
                homepage_url: if !url.is_empty() {
                    Some(url.clone())
                } else {
                    None
                },
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

    let derived_total =
        (places.len() + signals.len() + typed_events.len() + typed_reconstructions.len()) as i64;
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

pub async fn resolve_promote_research_artifact(
    state: &AtlasState,
    input: ResearchArtifactPromotionInput,
) -> async_graphql::Result<ResearchArtifactPromotionPayload> {
    let source_type = required_trimmed(&input.source_type, "sourceType")?;
    let title = required_trimmed(&input.title, "title")?;
    if !has_artifact_anchor(&input) {
        return Err(async_graphql::Error::new(
            "promoteResearchArtifact: provide parcelRef, buildingId, buildingPartId, or anchorGeometryWkt.",
        ));
    }

    let artifact_key = artifact_key_for(&input, &source_type, &title);
    let payload_json = promotion_json_object(&input.payload, "payload", &input, &artifact_key)?;
    let anchor_payload_json = promotion_json_object(
        &input.anchor_payload,
        "anchorPayload",
        &input,
        &artifact_key,
    )?;

    let tenant_id = default_tenant();
    let service = CivicAtlasGrpcService::new(state.clone());

    let mut request = Request::new(PersistArtifactRequest {
        tenant_context: Some(TenantContext {
            tenant_id: tenant_id.clone(),
            atlas_node_id: format!("atlas:{tenant_id}"),
            metadata: Default::default(),
        }),
        artifact_key: artifact_key.clone(),
        source_type,
        title,
        uri: optional_trimmed_string(&input.uri),
        citation: optional_trimmed_string(&input.citation),
        captured_at_ms: millis(input.captured_at),
        payload_json,
        parcel_ref: optional_trimmed_string(&input.parcel_ref),
        building_id: optional_trimmed_string(&input.building_id),
        building_part_id: optional_trimmed_string(&input.building_part_id),
        anchor_kind: optional_trimmed(input.anchor_kind.as_deref())
            .unwrap_or_else(|| "research".to_string()),
        anchor_geometry_wkt: optional_trimmed_string(&input.anchor_geometry_wkt),
        anchor_time_start_ms: millis(input.anchor_time_start),
        anchor_time_end_ms: millis(input.anchor_time_end),
        anchor_payload_json,
    });
    request.metadata_mut().insert(
        "x-atlas-tenant",
        tenant_id
            .parse()
            .map_err(|err| async_graphql::Error::new(format!("invalid tenant id: {err}")))?,
    );

    let response = service
        .persist_artifact(request)
        .await
        .map_err(|status| {
            async_graphql::Error::new(format!(
                "promoteResearchArtifact failed: {} ({})",
                status.message(),
                status.code()
            ))
        })?
        .into_inner();

    Ok(ResearchArtifactPromotionPayload {
        artifact_id: response.artifact_id,
        artifact_key,
        status: response.status,
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

    /// Promote a selected civicResearch source into the tenant-scoped
    /// artifact tables so future reconstruction runs can consume it as
    /// source-backed evidence.
    async fn promote_research_artifact(
        &self,
        ctx: &Context<'_>,
        input: ResearchArtifactPromotionInput,
    ) -> async_graphql::Result<ResearchArtifactPromotionPayload> {
        let state = ctx
            .data::<AtlasState>()
            .map_err(|_| async_graphql::Error::new("AtlasState missing from GraphQL context"))?;
        resolve_promote_research_artifact(state, input).await
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
        assert!(results
            .sources
            .iter()
            .any(|s| s.id.starts_with("research-source:")));
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

    fn promotion_input() -> ResearchArtifactPromotionInput {
        ResearchArtifactPromotionInput {
            artifact_key: None,
            run_id: Some("run:carriage-town".to_string()),
            source_id: Some("directory:1925:storefront".to_string()),
            source_type: "directory".to_string(),
            title: "1925 city directory storefront row".to_string(),
            uri: Some("https://example.org/directory".to_string()),
            citation: Some("Flint city directory, 1925".to_string()),
            captured_at: None,
            payload: None,
            source_use_tags: None,
            source_use_note: None,
            review_state: None,
            parcel_ref: Some("carriage-town:3".to_string()),
            building_id: None,
            building_part_id: None,
            anchor_kind: None,
            anchor_geometry_wkt: None,
            anchor_time_start: None,
            anchor_time_end: None,
            anchor_payload: None,
        }
    }

    #[test]
    fn generated_artifact_key_is_stable_for_same_research_source() {
        let input = promotion_input();
        let first = artifact_key_for(&input, "directory", "1925 city directory storefront row");
        let second = artifact_key_for(&input, "directory", "1925 city directory storefront row");
        assert_eq!(first, second);
        assert!(first.starts_with("research:directory-1925-storefront:"));
    }

    #[test]
    fn explicit_artifact_key_wins() {
        let mut input = promotion_input();
        input.artifact_key = Some("artifact:manual-directory-1925".to_string());
        assert_eq!(
            artifact_key_for(&input, "directory", "1925 city directory storefront row"),
            "artifact:manual-directory-1925"
        );
    }

    #[test]
    fn promotion_payload_requires_json_object() {
        let mut input = promotion_input();
        input.payload = Some(async_graphql::Json(serde_json::json!(["not", "object"])));
        let error =
            promotion_json_object(&input.payload, "payload", &input, "artifact:1").unwrap_err();
        assert!(error.message.contains("must be a JSON object"));
    }

    #[test]
    fn promotion_payload_preserves_claims_and_adds_research_metadata() {
        let mut input = promotion_input();
        input.payload = Some(async_graphql::Json(serde_json::json!({
            "claim": "storefront active in 1925"
        })));
        let raw = promotion_json_object(&input.payload, "payload", &input, "artifact:1").unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["claim"], "storefront active in 1925");
        assert_eq!(value["promotionKind"], "civicResearch");
        assert_eq!(value["runId"], "run:carriage-town");
        assert_eq!(value["sourceId"], "directory:1925:storefront");
        assert_eq!(value["reviewState"], "accepted_for_reconstruction");
        assert_eq!(
            value["metadata"]["reviewState"],
            "accepted_for_reconstruction"
        );
        assert_eq!(value["metadata"]["runId"], "run:carriage-town");
    }

    #[test]
    fn promotion_anchor_accepts_parcel_ref() {
        assert!(has_artifact_anchor(&promotion_input()));
    }

    #[test]
    fn promotion_payload_persists_source_use_and_review_metadata() {
        let mut input = promotion_input();
        input.source_use_tags = Some(vec![
            "facade".to_string(),
            "ground-floor-use".to_string(),
            "date".to_string(),
            "date".to_string(),
        ]);
        input.source_use_note = Some("Directory row supports storefront use.".to_string());
        input.review_state = Some("accepted-for-reconstruction".to_string());

        let raw = promotion_json_object(&input.payload, "payload", &input, "artifact:1").unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(value["reviewState"], "accepted_for_reconstruction");
        assert_eq!(
            value["sourceUseTags"],
            serde_json::json!(["facade", "ground_floor_use", "date"])
        );
        assert_eq!(
            value["sourceUseNote"],
            "Directory row supports storefront use."
        );
        assert_eq!(
            value["metadata"]["sourceUseTags"],
            "facade,ground_floor_use,date"
        );
        assert_eq!(
            value["metadata"]["sourceUseNote"],
            "Directory row supports storefront use."
        );
    }

    #[test]
    fn promotion_payload_rejects_unknown_source_use_tags() {
        let mut input = promotion_input();
        input.source_use_tags = Some(vec!["vibes".to_string()]);

        let error =
            promotion_json_object(&input.payload, "payload", &input, "artifact:1").unwrap_err();
        assert!(error.message.contains("sourceUseTags"));
        assert!(error.message.contains("vibes"));
    }
}
