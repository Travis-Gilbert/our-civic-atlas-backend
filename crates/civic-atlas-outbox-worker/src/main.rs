//! civic-atlas-outbox-worker
//!
//! Drains `reconstruction_projection_outbox` and projects approved
//! ReconstructionSpec rows to the external RustyRed knowledge graph
//! and (optionally) downstream renderers.
//!
//! Each pending row is claimed under `SELECT ... FOR UPDATE SKIP LOCKED`
//! so multiple worker replicas can run safely. After the projection
//! succeeds the row is marked `succeeded`. On failure the row is marked
//! `failed` with `last_error`, attempt_count incremented, and
//! `next_attempt_at` set to a backoff timestamp so a subsequent
//! poll picks it up later.
//!
//! Per migration 0002, `projection_kind` is currently `"BuildingPresence"`.
//! Future projection kinds (e.g. "SceneFoundryRender") plug in through
//! `dispatch_projection`.

#![allow(clippy::result_large_err)]

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, types::Json, PgPool, Postgres, Row, Transaction};
use tokio::signal;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "civic-atlas-outbox-worker", version, about)]
struct Args {
    /// PostgreSQL DSN. Falls back to DATABASE_URL env var.
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    /// Theseus bridge URL (for RustyRed projection). Optional; when
    /// empty, BuildingPresence projection logs the work and marks
    /// succeeded without calling Theseus.
    #[arg(long, env = "THESEUS_BRIDGE_URL", default_value = "")]
    theseus_bridge_url: String,

    /// Seconds between polls when the outbox is empty.
    #[arg(long, env = "OUTBOX_POLL_INTERVAL_SECS", default_value_t = 2)]
    poll_interval_secs: u64,

    /// Maximum rows claimed per poll.
    #[arg(long, env = "OUTBOX_BATCH_SIZE", default_value_t = 16)]
    batch_size: u32,

    /// Max retry attempts before a row is parked at status='failed'
    /// without rescheduling.
    #[arg(long, env = "OUTBOX_MAX_ATTEMPTS", default_value_t = 8)]
    max_attempts: i32,

    /// Base backoff seconds. Effective backoff is base * 2^(attempt - 1)
    /// capped at 1 hour. Exponential.
    #[arg(long, env = "OUTBOX_BACKOFF_BASE_SECS", default_value_t = 15)]
    backoff_base_secs: i64,
}

#[derive(Debug, Deserialize)]
struct BuildingPresencePayload {
    #[serde(rename = "specId")]
    spec_id: String,
    #[serde(rename = "version")]
    spec_version: i32,
    #[serde(rename = "buildingId")]
    building_id: String,
    #[serde(rename = "civicObjectId")]
    civic_object_id: String,
}

#[derive(Debug)]
enum ProjectionError {
    /// Transient failure: row stays pending, next_attempt_at gets pushed
    /// out. attempt_count is incremented.
    Transient(String),
    /// Permanent failure: row moves to status='failed' immediately.
    Permanent(String),
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "civic_atlas_outbox_worker=info,sqlx=warn".into()),
        )
        .init();

    let args = Args::parse();

    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&args.database_url)
        .await
        .context("connecting to DATABASE_URL")?;

    info!(
        poll_interval_secs = args.poll_interval_secs,
        batch_size = args.batch_size,
        "civic-atlas-outbox-worker starting"
    );

    let shutdown = signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("ctrl_c received, draining and exiting");
                break;
            }
            _ = run_one_poll(&pool, &args) => {
                // run_one_poll always returns Ok(()); errors are logged
                // inside.
            }
        }
    }

    Ok(())
}

async fn run_one_poll(pool: &PgPool, args: &Args) -> () {
    match claim_and_process(pool, args).await {
        Ok(rows_handled) => {
            if rows_handled == 0 {
                debug!("outbox empty; sleeping {}s", args.poll_interval_secs);
                sleep(Duration::from_secs(args.poll_interval_secs)).await;
            } else {
                debug!(rows_handled, "processed batch");
                // Immediately loop again when we found work, so a backlog
                // drains as fast as the worker can.
            }
        }
        Err(error) => {
            error!(%error, "poll cycle failed; backing off 5s");
            sleep(Duration::from_secs(5)).await;
        }
    }
}

async fn claim_and_process(pool: &PgPool, args: &Args) -> Result<usize> {
    let mut tx = pool.begin().await?;
    // SELECT FOR UPDATE SKIP LOCKED is the standard "claim N pending
    // rows safely under concurrency" pattern. The same pattern is used
    // by Sidekiq/Resque-like queues and by the standard pg_skip_locked
    // recipe. Each running worker only sees rows nobody else has
    // claimed.
    let rows = sqlx::query(
        r#"
        SELECT id, tenant_id, spec_id, spec_version, projection_kind,
               idempotency_key, payload_jsonb, attempt_count
        FROM reconstruction_projection_outbox
        WHERE status = 'pending'
          AND (next_attempt_at IS NULL OR next_attempt_at <= now())
        ORDER BY created_at ASC
        FOR UPDATE SKIP LOCKED
        LIMIT $1
        "#,
    )
    .bind(args.batch_size as i64)
    .fetch_all(&mut *tx)
    .await?;

    if rows.is_empty() {
        tx.rollback().await.ok();
        return Ok(0);
    }

    // Mark all as running while still holding the lock.
    let ids: Vec<Uuid> = rows.iter().map(|r| r.get::<Uuid, _>("id")).collect();
    sqlx::query(
        r#"
        UPDATE reconstruction_projection_outbox
        SET status = 'running', updated_at = now()
        WHERE id = ANY($1)
        "#,
    )
    .bind(&ids[..])
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let mut handled = 0_usize;
    for row in rows {
        let id: Uuid = row.get("id");
        let tenant_id: Uuid = row.get("tenant_id");
        let spec_id: String = row.get("spec_id");
        let spec_version: i32 = row.get("spec_version");
        let projection_kind: String = row.get("projection_kind");
        let attempt_count: i32 = row.get("attempt_count");
        let payload: Value = row
            .try_get::<Json<Value>, _>("payload_jsonb")
            .map(|j| j.0)
            .unwrap_or(Value::Null);

        debug!(
            outbox_id = %id, %tenant_id, %spec_id, spec_version,
            %projection_kind, attempt_count, "claimed outbox row"
        );

        let outcome = dispatch_projection(args, &projection_kind, &payload).await;

        if let Err(error) = mark_outcome(pool, id, attempt_count, outcome, args).await {
            error!(outbox_id = %id, %error, "failed to mark outbox outcome");
        }
        handled += 1;
    }
    Ok(handled)
}

async fn dispatch_projection(
    args: &Args,
    projection_kind: &str,
    payload: &Value,
) -> Result<(), ProjectionError> {
    match projection_kind {
        "BuildingPresence" => project_building_presence(args, payload).await,
        other => Err(ProjectionError::Permanent(format!(
            "unknown projection_kind: {other}"
        ))),
    }
}

async fn project_building_presence(
    args: &Args,
    payload: &Value,
) -> Result<(), ProjectionError> {
    let parsed: BuildingPresencePayload = serde_json::from_value(payload.clone())
        .map_err(|e| ProjectionError::Permanent(format!("payload decode: {e}")))?;

    info!(
        spec_id = %parsed.spec_id,
        spec_version = parsed.spec_version,
        building_id = %parsed.building_id,
        "projecting BuildingPresence"
    );

    // When the Theseus bridge URL is unset, log + return ok. This is
    // the default in dev: the outbox row is drained but no external
    // call happens. Phase 4 gate verification in PostGIS can still
    // observe the status transition pending -> succeeded.
    if args.theseus_bridge_url.trim().is_empty() {
        warn!(
            "THESEUS_BRIDGE_URL not set; logging projection and marking succeeded \
             without calling RustyRed"
        );
        return Ok(());
    }

    // TODO(phase-4): wire to theseus-client / RustyRed. The bridge
    // does not yet expose a "ProjectReconstruction" RPC. When it does,
    // open a channel here and call it. Until then, treat configured
    // URLs as a marker that the operator wants a real call but we
    // can only stub it.
    let _ = parsed.civic_object_id; // suppress unused
    Err(ProjectionError::Transient(
        "theseus bridge ProjectReconstruction RPC is not yet defined; \
         will retry once contract lands"
            .to_string(),
    ))
}

async fn mark_outcome(
    pool: &PgPool,
    id: Uuid,
    attempt_count: i32,
    outcome: Result<(), ProjectionError>,
    args: &Args,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    match outcome {
        Ok(()) => {
            sqlx::query(
                r#"
                UPDATE reconstruction_projection_outbox
                SET status = 'succeeded',
                    attempt_count = attempt_count + 1,
                    last_error = NULL,
                    updated_at = now(),
                    next_attempt_at = NULL
                WHERE id = $1
                "#,
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        Err(ProjectionError::Permanent(msg)) => {
            warn!(outbox_id = %id, %msg, "permanent projection failure");
            sqlx::query(
                r#"
                UPDATE reconstruction_projection_outbox
                SET status = 'failed',
                    attempt_count = attempt_count + 1,
                    last_error = $2,
                    updated_at = now(),
                    next_attempt_at = NULL
                WHERE id = $1
                "#,
            )
            .bind(id)
            .bind(&msg)
            .execute(&mut *tx)
            .await?;
        }
        Err(ProjectionError::Transient(msg)) => {
            let next_attempt_count = attempt_count + 1;
            if next_attempt_count >= args.max_attempts {
                warn!(
                    outbox_id = %id,
                    %msg,
                    attempts = next_attempt_count,
                    "max attempts reached; parking row at failed"
                );
                sqlx::query(
                    r#"
                    UPDATE reconstruction_projection_outbox
                    SET status = 'failed',
                        attempt_count = $2,
                        last_error = $3,
                        updated_at = now(),
                        next_attempt_at = NULL
                    WHERE id = $1
                    "#,
                )
                .bind(id)
                .bind(next_attempt_count)
                .bind(&msg)
                .execute(&mut *tx)
                .await?;
            } else {
                let backoff_seconds = backoff_for_attempt(args.backoff_base_secs, next_attempt_count);
                debug!(
                    outbox_id = %id,
                    %msg,
                    attempts = next_attempt_count,
                    backoff_seconds,
                    "transient projection failure; rescheduling"
                );
                sqlx::query(
                    r#"
                    UPDATE reconstruction_projection_outbox
                    SET status = 'pending',
                        attempt_count = $2,
                        last_error = $3,
                        updated_at = now(),
                        next_attempt_at = now() + ($4::bigint || ' seconds')::interval
                    WHERE id = $1
                    "#,
                )
                .bind(id)
                .bind(next_attempt_count)
                .bind(&msg)
                .bind(backoff_seconds)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

fn backoff_for_attempt(base_secs: i64, attempt: i32) -> i64 {
    // Exponential backoff with a 1-hour cap. attempt=1 -> base, 2 ->
    // base*2, ... capped at 3600s.
    let exponent = (attempt - 1).max(0) as u32;
    let raw = base_secs.saturating_mul(2_i64.saturating_pow(exponent));
    raw.min(3600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        let base = 15;
        assert_eq!(backoff_for_attempt(base, 1), 15);
        assert_eq!(backoff_for_attempt(base, 2), 30);
        assert_eq!(backoff_for_attempt(base, 3), 60);
        // attempt 9 -> 15 * 256 = 3840 -> capped at 3600
        assert_eq!(backoff_for_attempt(base, 9), 3600);
    }

    #[test]
    fn backoff_handles_attempt_zero() {
        assert_eq!(backoff_for_attempt(15, 0), 15);
    }
}

// Unused but kept so `theseus_client` stays in the dep graph; the real
// projection call will use it once the RPC lands.
#[allow(dead_code)]
fn _ensure_theseus_client_linked() {
    let _ = std::any::type_name::<theseus_client::TheseusClient>();
}

// Same for tonic transaction handle so future PR doesn't need to
// re-add the type-only use.
#[allow(dead_code)]
fn _tx_type_only<'a>(_: Transaction<'a, Postgres>) {}
