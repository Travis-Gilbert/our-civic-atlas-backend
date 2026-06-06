//! GraphQL types and resolver for realtime traffic flow.
//!
//! Implements schema Extension 8 (`flint-graphql-schema-v1.graphql`) on the
//! Axum-native GraphQL surface: `trafficRealtime(networkId)` returns a
//! per-segment flow snapshot the Civic Atlas frontend renders as animated flow.
//!
//! Cross-repo coordination lives in the frontend repo at
//! `docs/plans/traffic-domain-realtime/` (Open-Flint-Atlas-main-release): GraphQL
//! is the canonical public read seam; the frontend's REST shim is a dev fallback.
//!
//! TR-B1 (this slice): an honest, fixture-backed resolver. No live feed is wired
//! (TR-B3) and no road-network table exists yet (TR-B2), so every segment is
//! marked `sourceStatus: FIXTURE` and the snapshot `status: FIXTURE_FALLBACK`.
//! The engine never reports a LIVE source it does not have. The frontend renders
//! this identically to its dev fallback, but now over the canonical GraphQL seam
//! (source "graphql"). When the MDOT RIDE feed lands (TR-B3), this resolver
//! returns measured segments with `sourceStatus: LIVE` and `status: LIVE`, and no
//! frontend change is needed.

use async_graphql::{Context, Enum, Json, Object, SimpleObject, ID};
use chrono::Utc;
use serde_json::{json, Value};

use crate::AtlasState;

/// How a segment's flow numbers were derived.
#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum TrafficEstimateBasis {
    #[graphql(name = "LIVE_FEED")]
    LiveFeed,
    #[graphql(name = "HOURLY_PATTERN")]
    HourlyPattern,
    #[graphql(name = "SCENARIO_MODEL")]
    ScenarioModel,
}

/// Whether a segment's number is backed by an actually-live source right now.
#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum TrafficSourceStatus {
    #[graphql(name = "LIVE")]
    Live,
    #[graphql(name = "FIXTURE")]
    Fixture,
    #[graphql(name = "PENDING_LIVE_SOURCE")]
    PendingLiveSource,
}

/// Whole-snapshot feed status. FIXTURE_FALLBACK and UNAVAILABLE are never live.
#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum TrafficFeedStatus {
    #[graphql(name = "LIVE")]
    Live,
    #[graphql(name = "FIXTURE_FALLBACK")]
    FixtureFallback,
    #[graphql(name = "UNAVAILABLE")]
    Unavailable,
}

/// One road segment carrying flow. `geometry` is the LineString the animation
/// tweens particles along; `volumePerHour` drives particle density and
/// `speedMph` drives tween duration.
#[derive(SimpleObject, Clone)]
pub struct TrafficSegment {
    pub segment_id: ID,
    pub corridor_name: String,
    pub direction_label: String,
    pub geometry: Json<Value>,
    pub estimate_basis: TrafficEstimateBasis,
    pub source_status: TrafficSourceStatus,
    pub source_label: String,
    pub support_note: String,
    pub observed_at: String,
    pub expires_at: Option<String>,
    pub speed_mph: f64,
    pub free_flow_speed_mph: f64,
    pub volume_per_hour: f64,
    pub congestion_ratio: f64,
    pub confidence: f64,
}

/// Roll-up the panel renders above the segment list.
#[derive(SimpleObject, Clone)]
pub struct TrafficRealtimeSummary {
    pub segment_count: i32,
    pub live_feed_segments: i32,
    pub inferred_segments: i32,
    pub congested_segments: i32,
    pub average_speed_mph: f64,
    pub average_congestion_ratio: f64,
}

/// One-shot realtime traffic payload for a road-network area at a moment.
#[derive(SimpleObject, Clone)]
pub struct TrafficRealtimeSnapshot {
    pub feed_id: ID,
    pub source_label: String,
    pub source_url: Option<String>,
    pub status: TrafficFeedStatus,
    pub generated_at: String,
    pub refresh_interval_seconds: i32,
    pub summary: TrafficRealtimeSummary,
    pub segments: Vec<TrafficSegment>,
}

#[derive(Default)]
pub struct TrafficQuery;

#[Object]
impl TrafficQuery {
    /// Returns the current realtime traffic snapshot for a road-network area.
    /// `networkId` selects the area (e.g. "flint-downtown"). Single round-trip
    /// so the surface can drive the animation without N+1 fetches.
    ///
    /// TR-B1: honest fixture fallback until the live MDOT RIDE feed (TR-B3) and
    /// the road-network subgraph (TR-B2) land. The engine never reports a LIVE
    /// source it does not have.
    async fn traffic_realtime(
        &self,
        ctx: &Context<'_>,
        network_id: ID,
    ) -> async_graphql::Result<TrafficRealtimeSnapshot> {
        // Presence guard, and the seam where the DB-backed path (TR-B2) attaches.
        ctx.data::<AtlasState>()
            .map_err(|_| async_graphql::Error::new("AtlasState missing from GraphQL context"))?;
        Ok(fixture_snapshot(&network_id.0))
    }
}

/// Build the honest fixture snapshot for a network. Public so an integration
/// test or the future DB-backed path can reuse the shaping.
pub fn fixture_snapshot(network_id: &str) -> TrafficRealtimeSnapshot {
    let now = Utc::now().to_rfc3339();
    let segments = seed_flint_segments(&now);
    let summary = summarize(&segments);
    TrafficRealtimeSnapshot {
        feed_id: ID(format!("traffic:{network_id}:realtime-seed")),
        source_label: "MDOT RIDE connection target; backend fixture fallback".to_string(),
        source_url: Some(
            "https://www.michigan.gov/mdot/travel/safety/efforts/its/its-data".to_string(),
        ),
        status: TrafficFeedStatus::FixtureFallback,
        generated_at: now,
        refresh_interval_seconds: 15,
        summary,
        segments,
    }
}

fn summarize(segments: &[TrafficSegment]) -> TrafficRealtimeSummary {
    let n = segments.len().max(1) as f64;
    TrafficRealtimeSummary {
        segment_count: segments.len() as i32,
        live_feed_segments: count_basis(segments, TrafficEstimateBasis::LiveFeed),
        inferred_segments: count_basis(segments, TrafficEstimateBasis::HourlyPattern),
        congested_segments: segments.iter().filter(|s| s.congestion_ratio >= 0.28).count() as i32,
        average_speed_mph: round1(segments.iter().map(|s| s.speed_mph).sum::<f64>() / n),
        average_congestion_ratio: round2(
            segments.iter().map(|s| s.congestion_ratio).sum::<f64>() / n,
        ),
    }
}

fn count_basis(segments: &[TrafficSegment], basis: TrafficEstimateBasis) -> i32 {
    segments.iter().filter(|s| s.estimate_basis == basis).count() as i32
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

struct SeedCorridor {
    id: &'static str,
    corridor: &'static str,
    direction: &'static str,
    basis: TrafficEstimateBasis,
    speed_mph: f64,
    free_flow_mph: f64,
    volume_per_hour: f64,
    confidence: f64,
    support_note: &'static str,
    coordinates: &'static [[f64; 2]],
}

fn seed_flint_segments(now: &str) -> Vec<TrafficSegment> {
    seed_corridors()
        .into_iter()
        .map(|c| {
            let congestion_ratio = (1.0 - c.speed_mph / c.free_flow_mph).clamp(0.0, 1.0);
            let source_label = match c.basis {
                TrafficEstimateBasis::LiveFeed => "MDOT RIDE target, fixture mirror",
                TrafficEstimateBasis::HourlyPattern => "Hourly pattern seed",
                TrafficEstimateBasis::ScenarioModel => "Scenario model seed",
            };
            TrafficSegment {
                segment_id: ID(c.id.to_string()),
                corridor_name: c.corridor.to_string(),
                direction_label: c.direction.to_string(),
                geometry: Json(json!({
                    "type": "LineString",
                    "coordinates": c.coordinates,
                })),
                estimate_basis: c.basis,
                source_status: TrafficSourceStatus::Fixture,
                source_label: source_label.to_string(),
                support_note: c.support_note.to_string(),
                observed_at: now.to_string(),
                expires_at: None,
                speed_mph: c.speed_mph,
                free_flow_speed_mph: c.free_flow_mph,
                volume_per_hour: c.volume_per_hour,
                congestion_ratio: round2(congestion_ratio),
                confidence: c.confidence,
            }
        })
        .collect()
}

/// The seed corridors. Real Flint geometry, mirroring the frontend dev fixture
/// (`src/data/open-flint-atlas/fixtures/traffic/realtime-flint.json`). These
/// retire once the road-network subgraph (TR-B2) and live feed (TR-B3) land.
fn seed_corridors() -> Vec<SeedCorridor> {
    vec![
        SeedCorridor {
            id: "traffic:flint:i-69:west",
            corridor: "I-69 west approach",
            direction: "Eastbound / west-side approach",
            basis: TrafficEstimateBasis::LiveFeed,
            speed_mph: 56.0,
            free_flow_mph: 65.0,
            volume_per_hour: 1280.0,
            confidence: 0.82,
            support_note: "Fixture segment shaped to the public RIDE handoff contract until the authenticated live feed is wired.",
            coordinates: &[
                [-83.7514, 42.9989],
                [-83.7347, 42.9992],
                [-83.7116, 42.9995],
                [-83.6904, 42.9991],
            ],
        },
        SeedCorridor {
            id: "traffic:flint:i-475:spine",
            corridor: "I-475 city spine",
            direction: "North / south trunkline",
            basis: TrafficEstimateBasis::LiveFeed,
            speed_mph: 49.0,
            free_flow_mph: 60.0,
            volume_per_hour: 1640.0,
            confidence: 0.84,
            support_note: "Represents the first trunkline corridor a live loop-detector feed should map onto.",
            coordinates: &[
                [-83.7047, 43.0476],
                [-83.7041, 43.0315],
                [-83.7016, 43.0122],
                [-83.6991, 42.9918],
            ],
        },
        SeedCorridor {
            id: "traffic:flint:court:midtown",
            corridor: "Court Street / M-21",
            direction: "Downtown east-west corridor",
            basis: TrafficEstimateBasis::HourlyPattern,
            speed_mph: 27.0,
            free_flow_mph: 35.0,
            volume_per_hour: 760.0,
            confidence: 0.62,
            support_note: "Local arterial sample inferred from an hourly traffic pattern until current counts are available.",
            coordinates: &[
                [-83.7346, 43.012],
                [-83.7168, 43.0121],
                [-83.6978, 43.0124],
                [-83.6798, 43.0125],
            ],
        },
        SeedCorridor {
            id: "traffic:flint:saginaw:downtown",
            corridor: "Saginaw Street downtown",
            direction: "Downtown north-south street",
            basis: TrafficEstimateBasis::HourlyPattern,
            speed_mph: 18.0,
            free_flow_mph: 28.0,
            volume_per_hour: 540.0,
            confidence: 0.58,
            support_note: "Downtown street sample, useful for proving density-and-speed rendering before signal timing feeds are connected.",
            coordinates: &[
                [-83.6938, 43.0308],
                [-83.694, 43.0218],
                [-83.6933, 43.0142],
                [-83.6925, 43.0046],
            ],
        },
        SeedCorridor {
            id: "traffic:flint:dort:east",
            corridor: "Dort Highway",
            direction: "East-side north-south corridor",
            basis: TrafficEstimateBasis::LiveFeed,
            speed_mph: 42.0,
            free_flow_mph: 50.0,
            volume_per_hour: 1120.0,
            confidence: 0.78,
            support_note: "High-volume east-side corridor sample for live-feed segment mapping.",
            coordinates: &[
                [-83.6558, 43.0412],
                [-83.6557, 43.0228],
                [-83.6552, 43.0004],
                [-83.6547, 42.981],
            ],
        },
        SeedCorridor {
            id: "traffic:flint:miller:southwest",
            corridor: "Miller Road",
            direction: "Southwest commercial corridor",
            basis: TrafficEstimateBasis::HourlyPattern,
            speed_mph: 31.0,
            free_flow_mph: 40.0,
            volume_per_hour: 880.0,
            confidence: 0.6,
            support_note: "Commercial-corridor sample for inferred demand and later count calibration.",
            coordinates: &[
                [-83.7835, 42.9901],
                [-83.7612, 42.9904],
                [-83.7418, 42.9906],
                [-83.7228, 42.9908],
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_snapshot_is_honest() {
        let snap = fixture_snapshot("flint-downtown");
        assert_eq!(snap.segments.len(), 6);
        assert!(matches!(snap.status, TrafficFeedStatus::FixtureFallback));
        // Calibration discipline: never present non-live flow as a live source.
        assert!(snap
            .segments
            .iter()
            .all(|s| !matches!(s.source_status, TrafficSourceStatus::Live)));
        // Every segment carries provenance + a confidence in [0, 1].
        assert!(snap
            .segments
            .iter()
            .all(|s| s.confidence >= 0.0 && s.confidence <= 1.0));
        assert_eq!(snap.summary.segment_count, 6);
        assert_eq!(snap.summary.live_feed_segments, 3);
        assert_eq!(snap.summary.inferred_segments, 3);
    }
}
