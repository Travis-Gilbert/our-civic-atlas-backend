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
//! TR-B2b/B2c: the resolver prefers the tenant-scoped `traffic_segments` PostGIS
//! table and falls back to the honest in-code fixture when the DB is unavailable,
//! empty, or not migrated yet. The table seed now stores street-centerline-traced
//! geometry so the frontend animation follows roads instead of coarse corridor
//! chords. When the MDOT RIDE feed lands (TR-B3), writers update the same table
//! with `sourceStatus: LIVE`; no frontend change is needed.

use std::env;

use async_graphql::{Context, Enum, Json, Object, SimpleObject, ID};
use chrono::{Duration, Timelike, Utc};
use serde_json::{json, Value};
use sqlx::{postgres::PgRow, Postgres, Row, Transaction};
use tracing::warn;
use uuid::Uuid;

use crate::{tenant_db, AtlasState};

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
    #[graphql(name = "HISTORIC_AVERAGE")]
    HistoricAverage,
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
    #[graphql(name = "HISTORIC_AVERAGE")]
    HistoricAverage,
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
    /// Prefer the tenant-scoped `traffic_segments` table. If the table is empty
    /// or unavailable, return the honest fixture fallback; never report a LIVE
    /// source unless the row itself is marked live.
    async fn traffic_realtime(
        &self,
        ctx: &Context<'_>,
        network_id: ID,
    ) -> async_graphql::Result<TrafficRealtimeSnapshot> {
        let state = ctx
            .data::<AtlasState>()
            .map_err(|_| async_graphql::Error::new("AtlasState missing from GraphQL context"))?;

        match db_snapshot(state, &network_id.0).await {
            Ok(Some(snapshot)) => Ok(snapshot),
            Ok(None) => Ok(fixture_snapshot(&network_id.0)),
            Err(error) => {
                warn!(
                    ?error,
                    network_id = network_id.0.as_str(),
                    "trafficRealtime PostGIS read failed; using fixture fallback",
                );
                Ok(fixture_snapshot(&network_id.0))
            }
        }
    }
}

async fn db_snapshot(
    state: &AtlasState,
    network_id: &str,
) -> async_graphql::Result<Option<TrafficRealtimeSnapshot>> {
    let Some(pool) = state.db_pool() else {
        return Ok(None);
    };
    let tenant_slug = default_tenant();
    let generated_at = Utc::now();
    let generated_at_iso = generated_at.to_rfc3339();
    let expires_at = (generated_at + Duration::seconds(15)).to_rfc3339();

    let mut tx = pool.begin().await.map_err(graphql_db_error)?;
    let tenant_id = resolve_tenant_id(&mut tx, &tenant_slug)
        .await
        .map_err(graphql_db_error)?;
    tenant_db::set_transaction_tenant(&mut tx, &tenant_id.to_string())
        .await
        .map_err(graphql_db_error)?;

    let rows = sqlx::query(
        r#"
        SELECT
            segment_key,
            corridor_name,
            direction_label,
            ST_AsGeoJSON(geometry::geometry) AS geometry_geojson,
            estimate_basis,
            source_status,
            source_label,
            support_note,
            free_flow_speed_mph,
            base_speed_mph,
            base_volume_per_hour,
            confidence
        FROM traffic_segments
        WHERE tenant_id = $1 AND network_id = $2
        ORDER BY segment_key ASC
        "#,
    )
    .bind(tenant_id)
    .bind(network_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(graphql_db_error)?;

    tx.commit().await.map_err(graphql_db_error)?;

    if rows.is_empty() {
        return Ok(None);
    }

    let mut segments = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        segments.push(segment_from_row(
            row,
            index,
            &generated_at,
            &generated_at_iso,
            &expires_at,
        )?);
    }

    let status = if segments
        .iter()
        .any(|segment| matches!(segment.source_status, TrafficSourceStatus::Live))
    {
        TrafficFeedStatus::Live
    } else if segments
        .iter()
        .any(|segment| matches!(segment.source_status, TrafficSourceStatus::HistoricAverage))
    {
        TrafficFeedStatus::HistoricAverage
    } else {
        TrafficFeedStatus::FixtureFallback
    };
    let summary = summarize(&segments);

    Ok(Some(TrafficRealtimeSnapshot {
        feed_id: ID(format!("traffic:{network_id}:postgis")),
        source_label: match status {
            TrafficFeedStatus::Live => "PostGIS traffic_segments live feed".to_string(),
            TrafficFeedStatus::HistoricAverage => {
                "MDOT 2024 AADT official historic average".to_string()
            }
            TrafficFeedStatus::FixtureFallback => {
                "PostGIS traffic_segments fixture fallback".to_string()
            }
            TrafficFeedStatus::Unavailable => "Traffic source unavailable".to_string(),
        },
        source_url: Some(match status {
            TrafficFeedStatus::HistoricAverage => {
                "https://mdotgis.state.mi.us/arcgis/rest/services/DataAccess/MdotAadtCaadt2024/FeatureServer/1"
            }
            _ => "https://www.michigan.gov/mdot/travel/safety/efforts/its/its-data",
        }
        .to_string()),
        status,
        generated_at: generated_at_iso,
        refresh_interval_seconds: 15,
        summary,
        segments,
    }))
}

fn segment_from_row(
    row: &PgRow,
    index: usize,
    now: &chrono::DateTime<Utc>,
    observed_at: &str,
    expires_at: &str,
) -> async_graphql::Result<TrafficSegment> {
    let geometry_geojson: String = row.try_get("geometry_geojson").map_err(graphql_db_error)?;
    let geometry = serde_json::from_str::<Value>(&geometry_geojson).map_err(|error| {
        async_graphql::Error::new(format!(
            "traffic segment geometry is invalid GeoJSON: {error}"
        ))
    })?;
    let estimate_basis = estimate_basis_from_db(
        row.try_get::<String, _>("estimate_basis")
            .map_err(graphql_db_error)?
            .as_str(),
    );
    let source_status = source_status_from_db(
        row.try_get::<String, _>("source_status")
            .map_err(graphql_db_error)?
            .as_str(),
    );
    let free_flow_mph: f64 = row
        .try_get("free_flow_speed_mph")
        .map_err(graphql_db_error)?;
    let base_speed_mph: f64 = row.try_get("base_speed_mph").map_err(graphql_db_error)?;
    let base_volume_per_hour: f64 = row
        .try_get("base_volume_per_hour")
        .map_err(graphql_db_error)?;
    let (speed_mph, volume_per_hour, congestion_ratio) = shaped_flow(
        now,
        index,
        estimate_basis,
        base_speed_mph,
        free_flow_mph,
        base_volume_per_hour,
    );

    Ok(TrafficSegment {
        segment_id: ID(row
            .try_get::<String, _>("segment_key")
            .map_err(graphql_db_error)?),
        corridor_name: row
            .try_get::<String, _>("corridor_name")
            .map_err(graphql_db_error)?,
        direction_label: row
            .try_get::<String, _>("direction_label")
            .map_err(graphql_db_error)?,
        geometry: Json(geometry),
        estimate_basis,
        source_status,
        source_label: row
            .try_get::<String, _>("source_label")
            .map_err(graphql_db_error)?,
        support_note: row
            .try_get::<String, _>("support_note")
            .map_err(graphql_db_error)?,
        observed_at: observed_at.to_string(),
        expires_at: Some(expires_at.to_string()),
        speed_mph,
        free_flow_speed_mph: free_flow_mph,
        volume_per_hour,
        congestion_ratio,
        confidence: row.try_get("confidence").map_err(graphql_db_error)?,
    })
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
        live_feed_segments: count_source_status(segments, TrafficSourceStatus::Live),
        inferred_segments: count_basis(segments, TrafficEstimateBasis::HourlyPattern),
        congested_segments: segments
            .iter()
            .filter(|s| s.congestion_ratio >= 0.28)
            .count() as i32,
        average_speed_mph: round1(segments.iter().map(|s| s.speed_mph).sum::<f64>() / n),
        average_congestion_ratio: round2(
            segments.iter().map(|s| s.congestion_ratio).sum::<f64>() / n,
        ),
    }
}

fn count_basis(segments: &[TrafficSegment], basis: TrafficEstimateBasis) -> i32 {
    segments
        .iter()
        .filter(|s| s.estimate_basis == basis)
        .count() as i32
}

fn count_source_status(segments: &[TrafficSegment], status: TrafficSourceStatus) -> i32 {
    segments
        .iter()
        .filter(|s| s.source_status == status)
        .count() as i32
}

fn default_tenant() -> String {
    env::var("CIVIC_ATLAS_DEFAULT_TENANT").unwrap_or_else(|_| "flint".to_string())
}

async fn resolve_tenant_id(
    tx: &mut Transaction<'_, Postgres>,
    tenant_key: &str,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        SELECT id
        FROM tenants
        WHERE slug = $1 OR id::text = $1
        "#,
    )
    .bind(tenant_key)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(sqlx::Error::RowNotFound)
}

fn estimate_basis_from_db(raw: &str) -> TrafficEstimateBasis {
    match raw {
        "live_feed" => TrafficEstimateBasis::LiveFeed,
        "scenario_model" => TrafficEstimateBasis::ScenarioModel,
        _ => TrafficEstimateBasis::HourlyPattern,
    }
}

fn source_status_from_db(raw: &str) -> TrafficSourceStatus {
    match raw {
        "live" => TrafficSourceStatus::Live,
        "historic_average" => TrafficSourceStatus::HistoricAverage,
        "pending_live_source" => TrafficSourceStatus::PendingLiveSource,
        _ => TrafficSourceStatus::Fixture,
    }
}

fn shaped_flow(
    now: &chrono::DateTime<Utc>,
    index: usize,
    estimate_basis: TrafficEstimateBasis,
    base_speed_mph: f64,
    free_flow_mph: f64,
    base_volume_per_hour: f64,
) -> (f64, f64, f64) {
    let hour = now.hour();
    let is_peak = (7..=9).contains(&hour) || (16..=18).contains(&hour);
    let minute_bucket = now.timestamp() / 60;
    let phase = minute_bucket as f64 / 8.0 + index as f64;
    let wave = phase.sin() * 0.5 + 0.5;
    let peak_multiplier = if is_peak { 1.18 } else { 0.92 };
    let inferred_penalty = if matches!(estimate_basis, TrafficEstimateBasis::HourlyPattern) {
        0.88
    } else {
        1.0
    };
    let volume = (base_volume_per_hour * peak_multiplier * (0.88 + wave * 0.28)).round();
    let speed = round1(
        (base_speed_mph * inferred_penalty * (1.06 - wave * if is_peak { 0.22 } else { 0.12 }))
            .clamp(9.0, free_flow_mph),
    );
    let congestion_ratio = round2((1.0 - speed / free_flow_mph + wave * 0.08).clamp(0.0, 0.92));
    (speed, volume, congestion_ratio)
}

fn graphql_db_error(error: sqlx::Error) -> async_graphql::Error {
    async_graphql::Error::new(format!("trafficRealtime PostGIS read failed: {error}"))
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

/// The seed corridors. Real Flint centerline geometry, traced from the checked-in
/// OSM highway fixture where street-centerline names are present. These retire
/// once the road-network subgraph (TR-B2) and live feed (TR-B3) land.
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
                [-83.7464021, 42.9809101],
                [-83.7337611, 42.9847707],
                [-83.7246162, 42.9868365],
                [-83.7203617, 42.9879298],
                [-83.7115774, 42.9917899],
                [-83.7097826, 42.9928115],
                [-83.7085842, 42.9939147],
                [-83.7057572, 42.9974181],
                [-83.70366, 42.9992756],
                [-83.7016854, 43.0005116],
                [-83.6972937, 43.0022321],
                [-83.6926759, 43.0036699],
                [-83.6908789, 43.0044356],
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
                [-83.6960551, 43.0221557],
                [-83.6966648, 43.0228784],
                [-83.6976615, 43.0240574],
                [-83.6983962, 43.0249284],
                [-83.6997107, 43.0264868],
                [-83.7004594, 43.0273704],
                [-83.7014911, 43.0285902],
                [-83.7022743, 43.0295122],
                [-83.7034352, 43.0308898],
                [-83.7038272, 43.0313562],
                [-83.7041985, 43.031807],
                [-83.7043549, 43.0320809],
                [-83.7043882, 43.0332116],
                [-83.7044063, 43.0340262],
                [-83.7044248, 43.0349625],
                [-83.704449, 43.036245],
                [-83.7044727, 43.0374578],
                [-83.7044953, 43.0386226],
                [-83.7045093, 43.0393619],
                [-83.7045352, 43.0402207],
                [-83.7045701, 43.0415559],
                [-83.7045915, 43.0424688],
                [-83.7046304, 43.0438927],
                [-83.7046507, 43.0446898],
                [-83.704674, 43.045576],
                [-83.7046916, 43.0478026],
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
                [-83.7342573, 43.0069115],
                [-83.7303566, 43.006902],
                [-83.7261734, 43.0069149],
                [-83.7213885, 43.0069138],
                [-83.7138742, 43.0068762],
                [-83.7107771, 43.006882],
                [-83.7089746, 43.0068534],
                [-83.7060568, 43.0068448],
                [-83.7042546, 43.0068242],
                [-83.7014403, 43.006817],
                [-83.7007983, 43.0068306],
                [-83.7004091, 43.0069449],
                [-83.6984516, 43.0078281],
                [-83.6959012, 43.008967],
                [-83.6946226, 43.0095438],
                [-83.6939165, 43.0098546],
                [-83.6933183, 43.0100934],
                [-83.6917219, 43.0108083],
                [-83.6907386, 43.0112505],
                [-83.6892826, 43.0119088],
                [-83.6877535, 43.0126132],
                [-83.6865439, 43.0131659],
                [-83.6851737, 43.013789],
                [-83.6841188, 43.0142642],
                [-83.6832069, 43.0146783],
                [-83.6810213, 43.0156735],
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
                [-83.6776634, 43.0029166],
                [-83.6790299, 43.0043312],
                [-83.6804335, 43.0056004],
                [-83.6813844, 43.0064632],
                [-83.6828722, 43.0078536],
                [-83.6843682, 43.0092184],
                [-83.684928, 43.0097217],
                [-83.6862431, 43.0109235],
                [-83.6871807, 43.0117759],
                [-83.6878198, 43.0124862],
                [-83.6884268, 43.0132025],
                [-83.6898629, 43.0149033],
                [-83.6907806, 43.0159805],
                [-83.6921822, 43.0176378],
                [-83.6927125, 43.0182194],
                [-83.6934726, 43.0190733],
                [-83.6935673, 43.0192419],
                [-83.6935279, 43.0195188],
                [-83.6935289, 43.0206236],
                [-83.6935432, 43.022162],
                [-83.693562, 43.0236374],
                [-83.6935681, 43.0250629],
                [-83.6935794, 43.0259512],
                [-83.6936208, 43.0292531],
                [-83.6936475, 43.0317706],
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
                [-83.6531807, 42.9812669],
                [-83.6532406, 42.9825595],
                [-83.6533295, 42.9855535],
                [-83.6533981, 42.9876657],
                [-83.6534816, 42.9899025],
                [-83.6535668, 42.9918973],
                [-83.653628, 42.9933303],
                [-83.6536982, 42.9949724],
                [-83.6537896, 42.9971129],
                [-83.653878, 42.9991808],
                [-83.6539577, 43.0010472],
                [-83.6540292, 43.0027208],
                [-83.654084, 43.004096],
                [-83.6541506, 43.0062232],
                [-83.6542122, 43.0083424],
                [-83.6542784, 43.0102434],
                [-83.6544065, 43.0130846],
                [-83.6546297, 43.0180962],
                [-83.6547091, 43.0192589],
                [-83.6548003, 43.0206706],
                [-83.654966, 43.023347],
                [-83.6550924, 43.0253373],
                [-83.6552034, 43.0287159],
                [-83.6553425, 43.0328748],
                [-83.6554485, 43.0377341],
                [-83.6555913, 43.0412641],
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
                [-83.7796537, 42.9740091],
                [-83.7770726, 42.9746442],
                [-83.774029, 42.9753933],
                [-83.771734, 42.97595],
                [-83.7697825, 42.976702],
                [-83.7669905, 42.9777827],
                [-83.7650757, 42.9785381],
                [-83.7627646, 42.9794706],
                [-83.7615164, 42.9799623],
                [-83.7597673, 42.9806479],
                [-83.7568245, 42.9818008],
                [-83.7540275, 42.9828871],
                [-83.7515184, 42.9839225],
                [-83.7493913, 42.9848589],
                [-83.7470378, 42.9859227],
                [-83.7452637, 42.9867201],
                [-83.7443154, 42.9871381],
                [-83.7425777, 42.9879214],
                [-83.7414147, 42.988451],
                [-83.7397584, 42.9891851],
                [-83.7369472, 42.990452],
                [-83.7358817, 42.9909466],
                [-83.734475, 42.9915981],
                [-83.7336493, 42.9919785],
                [-83.7319133, 42.9927577],
                [-83.7293163, 42.9939289],
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
        assert_eq!(snap.summary.live_feed_segments, 0);
        assert_eq!(snap.summary.inferred_segments, 3);
    }

    #[test]
    fn fixture_geometry_is_centerline_rich() {
        let snap = fixture_snapshot("flint-downtown");
        for segment in snap.segments {
            let coordinates = segment
                .geometry
                .0
                .get("coordinates")
                .and_then(Value::as_array)
                .expect("fixture segment carries LineString coordinates");
            assert!(
                coordinates.len() > 4,
                "{:?} should use traced centerline geometry, not a coarse chord",
                segment.segment_id,
            );
        }
    }
}
