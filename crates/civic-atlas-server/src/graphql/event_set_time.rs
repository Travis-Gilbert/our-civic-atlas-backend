//! GraphQL resolver for the durable run-of-show set-time projection.
//!
//! The civic planning store (Yjs) is the system of record for organizer-set set
//! times. This surface is the one-way projection of the PARSED window (minutes
//! from the festival start) into Postgres (`event_set_time_projections`,
//! migration 0027), so the festival schedule survives as queryable durable data
//! for reporting and cross-system reads, not only as CRDT free text. The planner
//! pushes the schedule via `projectEventSetTimes`; nothing writes back to the
//! CRDT.
//!
//! Direct-sqlx adapter mirroring `graphql/event_email.rs` (the freshest
//! projection pattern). The small db helpers are duplicated locally to keep the
//! module self-contained; extract them to a shared `graphql/db.rs` if a third
//! consumer appears.

use async_graphql::{Context, InputObject, Object, SimpleObject};
use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, types::time::OffsetDateTime, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::AtlasState;

#[derive(SimpleObject, Clone)]
pub struct EventSetTimeProjection {
    pub id: String,
    pub event_layer_id: String,
    /// Civic object provenance key (the planning store sourceId).
    pub source_key: String,
    pub act_name: String,
    /// Original free text the organizer entered, e.g. "14:00-14:45".
    pub set_time_raw: Option<String>,
    /// Parsed window, minutes from the festival start (the run-of-show cursor).
    pub start_minute: i32,
    pub end_minute: i32,
    pub projected_at: Option<String>,
    pub version: i32,
}

#[derive(InputObject)]
pub struct EventSetTimeProjectionInput {
    pub source_key: String,
    pub act_name: String,
    pub set_time_raw: Option<String>,
    pub start_minute: i32,
    pub end_minute: i32,
}

#[derive(InputObject)]
pub struct ProjectEventSetTimesInput {
    pub event_slug: String,
    pub projections: Vec<EventSetTimeProjectionInput>,
    /// Full-snapshot semantics: when true, projections for the layer that are
    /// absent from this batch are removed. Default false (upsert only).
    pub prune_missing: Option<bool>,
}

#[derive(SimpleObject, Clone)]
pub struct ProjectEventSetTimesResult {
    pub projections: Vec<EventSetTimeProjection>,
    pub upserted: i32,
    pub pruned: i32,
}

#[derive(Default)]
pub struct EventSetTimeQuery;

#[Object]
impl EventSetTimeQuery {
    /// The durable set-time schedule projected for an event layer, ordered by
    /// start time. Empty until the planner has pushed at least once.
    async fn event_set_time_projections(
        &self,
        ctx: &Context<'_>,
        event_slug: String,
    ) -> async_graphql::Result<Vec<EventSetTimeProjection>> {
        let pool = pool(ctx)?;
        let mut tx = pool.begin().await.map_err(graphql_db_error)?;
        let tenant_id = resolve_tenant_id(&mut tx, &default_tenant_slug()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let event_layer_id = resolve_event_layer_id(&mut tx, tenant_id, &event_slug).await?;
        let projections = fetch_layer_projections(&mut tx, tenant_id, event_layer_id).await?;
        tx.commit().await.map_err(graphql_db_error)?;
        Ok(projections)
    }
}

#[derive(Default)]
pub struct EventSetTimeMutation;

#[Object]
impl EventSetTimeMutation {
    /// One-way push of the run-of-show schedule from the planning CRDT into the
    /// durable Postgres projection. Upserts each act by `source_key`; with
    /// `pruneMissing` it also removes acts no longer in the snapshot. Returns
    /// the full projected schedule for the layer.
    async fn project_event_set_times(
        &self,
        ctx: &Context<'_>,
        input: ProjectEventSetTimesInput,
    ) -> async_graphql::Result<ProjectEventSetTimesResult> {
        let pool = pool(ctx)?;
        let mut tx = pool.begin().await.map_err(graphql_db_error)?;
        let tenant_id = resolve_tenant_id(&mut tx, &default_tenant_slug()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let event_layer_id = resolve_event_layer_id(&mut tx, tenant_id, &input.event_slug).await?;

        let mut upserted = 0i32;
        let mut kept_keys: Vec<String> = Vec::with_capacity(input.projections.len());
        for projection in &input.projections {
            let source_key = projection.source_key.trim();
            if source_key.is_empty() {
                continue;
            }
            // The CHECK constraint rejects an inverted window; skip defensively
            // so one malformed act never fails the whole snapshot.
            if projection.end_minute < projection.start_minute {
                continue;
            }
            sqlx::query(
                r#"
                INSERT INTO event_set_time_projections (
                    tenant_id,
                    event_layer_id,
                    source_key,
                    act_name,
                    set_time_raw,
                    start_minute,
                    end_minute,
                    projected_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, now())
                ON CONFLICT (tenant_id, event_layer_id, source_key)
                DO UPDATE SET
                    act_name = EXCLUDED.act_name,
                    set_time_raw = EXCLUDED.set_time_raw,
                    start_minute = EXCLUDED.start_minute,
                    end_minute = EXCLUDED.end_minute,
                    projected_at = now()
                "#,
            )
            .bind(tenant_id)
            .bind(event_layer_id)
            .bind(source_key)
            .bind(projection.act_name.trim())
            .bind(clean_optional(projection.set_time_raw.as_deref()))
            .bind(projection.start_minute)
            .bind(projection.end_minute)
            .execute(&mut *tx)
            .await
            .map_err(graphql_db_error)?;
            upserted += 1;
            kept_keys.push(source_key.to_string());
        }

        let mut pruned = 0i32;
        if input.prune_missing.unwrap_or(false) {
            let result = sqlx::query(
                r#"
                DELETE FROM event_set_time_projections
                WHERE tenant_id = $1
                  AND event_layer_id = $2
                  AND NOT (source_key = ANY($3))
                "#,
            )
            .bind(tenant_id)
            .bind(event_layer_id)
            .bind(&kept_keys)
            .execute(&mut *tx)
            .await
            .map_err(graphql_db_error)?;
            pruned = i32::try_from(result.rows_affected()).unwrap_or(i32::MAX);
        }

        let projections = fetch_layer_projections(&mut tx, tenant_id, event_layer_id).await?;
        tx.commit().await.map_err(graphql_db_error)?;

        Ok(ProjectEventSetTimesResult {
            projections,
            upserted,
            pruned,
        })
    }
}

async fn fetch_layer_projections(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    event_layer_id: Uuid,
) -> async_graphql::Result<Vec<EventSetTimeProjection>> {
    let rows = sqlx::query(
        r#"
        SELECT id,
               event_layer_id,
               source_key,
               act_name,
               set_time_raw,
               start_minute,
               end_minute,
               projected_at,
               version
        FROM event_set_time_projections
        WHERE tenant_id = $1
          AND event_layer_id = $2
        ORDER BY start_minute, source_key
        "#,
    )
    .bind(tenant_id)
    .bind(event_layer_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(graphql_db_error)?;
    Ok(rows.iter().map(projection_from_row).collect())
}

fn projection_from_row(row: &PgRow) -> EventSetTimeProjection {
    EventSetTimeProjection {
        id: row.get::<Uuid, _>("id").to_string(),
        event_layer_id: row.get::<Uuid, _>("event_layer_id").to_string(),
        source_key: row.get("source_key"),
        act_name: row.get("act_name"),
        set_time_raw: row
            .try_get::<Option<String>, _>("set_time_raw")
            .ok()
            .flatten(),
        start_minute: row.get::<i32, _>("start_minute"),
        end_minute: row.get::<i32, _>("end_minute"),
        projected_at: ts_iso(row, "projected_at"),
        version: version_i32(row.try_get::<i64, _>("version").unwrap_or(1)),
    }
}

/* ------------------------------------------------------------------ */
/*  Local db helpers (mirror graphql/event_email.rs).                  */
/* ------------------------------------------------------------------ */

fn pool(ctx: &Context<'_>) -> async_graphql::Result<sqlx::PgPool> {
    let state = ctx
        .data::<AtlasState>()
        .map_err(|_| async_graphql::Error::new("AtlasState missing from GraphQL context"))?;
    state.db_pool().cloned().ok_or_else(|| {
        async_graphql::Error::new("DATABASE_URL is required for set-time projection")
    })
}

async fn resolve_tenant_id(
    tx: &mut Transaction<'_, Postgres>,
    tenant_slug: &str,
) -> async_graphql::Result<Uuid> {
    let row = sqlx::query("SELECT id FROM tenants WHERE slug = $1")
        .bind(tenant_slug)
        .fetch_optional(&mut **tx)
        .await
        .map_err(graphql_db_error)?;
    row.and_then(|row| row.try_get::<Uuid, _>("id").ok())
        .ok_or_else(|| async_graphql::Error::new(format!("unknown tenant: {tenant_slug}")))
}

async fn set_transaction_tenant(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
) -> async_graphql::Result<()> {
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(graphql_db_error)?;
    Ok(())
}

async fn resolve_event_layer_id(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    slug: &str,
) -> async_graphql::Result<Uuid> {
    let trimmed = slug.trim();
    if trimmed.is_empty() {
        return Err(async_graphql::Error::new("eventSlug is required"));
    }
    let row = sqlx::query(
        r#"
        SELECT id
        FROM event_layers
        WHERE tenant_id = $1
          AND slug = $2
        "#,
    )
    .bind(tenant_id)
    .bind(trimmed)
    .fetch_optional(&mut **tx)
    .await
    .map_err(graphql_db_error)?;
    row.and_then(|row| row.try_get::<Uuid, _>("id").ok())
        .ok_or_else(|| async_graphql::Error::new(format!("event layer not found: {trimmed}")))
}

fn default_tenant_slug() -> String {
    std::env::var("CIVIC_ATLAS_DEFAULT_TENANT").unwrap_or_else(|_| "flint".to_string())
}

fn clean_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn ts_iso(row: &PgRow, column: &str) -> Option<String> {
    if let Ok(Some(ts)) = row.try_get::<Option<OffsetDateTime>, _>(column) {
        return offset_to_iso(ts);
    }
    if let Ok(ts) = row.try_get::<OffsetDateTime, _>(column) {
        return offset_to_iso(ts);
    }
    None
}

fn offset_to_iso(ts: OffsetDateTime) -> Option<String> {
    let millis = ts.unix_timestamp() * 1_000 + i64::from(ts.millisecond());
    DateTime::<Utc>::from_timestamp_millis(millis).map(|dt| dt.to_rfc3339())
}

fn version_i32(version: i64) -> i32 {
    i32::try_from(version).unwrap_or(i32::MAX)
}

fn graphql_db_error(error: sqlx::Error) -> async_graphql::Error {
    async_graphql::Error::new(format!("event set-time projection failed: {error}"))
}
