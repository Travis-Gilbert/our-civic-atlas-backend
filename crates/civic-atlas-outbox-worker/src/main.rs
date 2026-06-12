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

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use civic_atlas_reconstruction_engine::{
    attach_manifest_to_spec, reconstruction_part_records, reconstruction_spec_to_json,
    run_full_pipeline, PairformerCivicPriorModel, PipelineOutput, PostgisRepository,
    ReconstructionRequest, TheseusBatchEmbeddingProvider, ZeroEmbeddingProvider,
};
use civic_atlas_renderer::{
    select_tier, stamp_massing_texture_provenance, LocalDirAssetStore, PgRenderJobQueue,
    SceneFoundryRenderer, TierThresholds,
};
use civic_atlas_types::civic_atlas::v1::{
    ReconstructionSpec, ReconstructionSpecStatus, TenantContext, TimeSlice,
};
use clap::Parser;
use serde::Deserialize;
use serde_json::{json, Value};
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

    /// Maximum procedural reconstruction jobs claimed per poll.
    #[arg(long, env = "RECONSTRUCTION_JOB_BATCH_SIZE", default_value_t = 4)]
    reconstruction_job_batch_size: u32,

    /// Filesystem root the Scene Foundry renderer writes massing assets to.
    #[arg(
        long,
        env = "SCENE_FOUNDRY_ASSET_DIR",
        default_value = "data/scene-foundry-assets"
    )]
    scene_foundry_asset_dir: String,

    /// Public URL prefix minted into Scene Foundry asset URIs. Defaults to
    /// the civic-atlas-server static asset route.
    #[arg(
        long,
        env = "SCENE_FOUNDRY_PUBLIC_BASE_URL",
        default_value = "/assets/scene-foundry"
    )]
    scene_foundry_public_base_url: String,

    /// Ray Serve renderer endpoint in civic-atlas-ingest (POST /render).
    /// When empty, GPU refinement jobs stay pending instead of burning
    /// retry attempts against a renderer that is not deployed.
    #[arg(long, env = "SCENE_FOUNDRY_RENDER_URL", default_value = "")]
    scene_foundry_render_url: String,

    /// Seconds allowed for one GPU render dispatch round trip.
    #[arg(long, env = "SCENE_FOUNDRY_RENDER_TIMEOUT_SECS", default_value_t = 900)]
    scene_foundry_render_timeout_secs: u64,

    /// Maximum Scene Foundry render jobs claimed per poll.
    #[arg(long, env = "SCENE_FOUNDRY_RENDER_JOB_BATCH_SIZE", default_value_t = 2)]
    scene_foundry_render_job_batch_size: u32,
}

#[derive(Debug, Deserialize)]
struct BuildingPresencePayload {
    #[serde(rename = "specId")]
    spec_id: String,
    #[serde(rename = "specVersion", alias = "version", alias = "spec_version")]
    spec_version: i32,
    #[serde(rename = "buildingId")]
    building_id: String,
    #[serde(rename = "civicObjectId")]
    civic_object_id: String,
}

#[derive(Debug)]
struct ReconstructionJobRow {
    id: Uuid,
    tenant_id: Uuid,
    parcel_id: String,
    time_slice: TimeSlice,
    requested_by: String,
    auto_approve: bool,
    attempt_count: i32,
}

#[derive(Debug)]
struct ProcessedReconstruction {
    spec_id: String,
    spec_version: u32,
    stage_report: Value,
}

#[derive(Debug, Default, Deserialize)]
struct TimeSlicePayload {
    #[serde(default, alias = "atMs")]
    at_ms: Option<i64>,
    #[serde(default, alias = "startMs")]
    start_ms: Option<i64>,
    #[serde(default, alias = "endMs")]
    end_ms: Option<i64>,
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
    match claim_and_process_all(pool, args).await {
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

async fn claim_and_process_all(pool: &PgPool, args: &Args) -> Result<usize> {
    let reconstruction_jobs = claim_and_process_reconstruction_jobs(pool, args).await?;
    let render_jobs = claim_and_process_render_jobs(pool, args).await?;
    let projection_rows = claim_and_process(pool, args).await?;
    Ok(reconstruction_jobs + render_jobs + projection_rows)
}

async fn claim_and_process_reconstruction_jobs(pool: &PgPool, args: &Args) -> Result<usize> {
    let mut tx = pool.begin().await?;
    let rows = sqlx::query(
        r#"
        SELECT id, tenant_id, parcel_id, time_slice_jsonb, requested_by,
               auto_approve, attempt_count
        FROM reconstruction_jobs
        WHERE status = 'pending'
          AND (next_attempt_at IS NULL OR next_attempt_at <= now())
        ORDER BY created_at ASC
        FOR UPDATE SKIP LOCKED
        LIMIT $1
        "#,
    )
    .bind(args.reconstruction_job_batch_size as i64)
    .fetch_all(&mut *tx)
    .await?;

    if rows.is_empty() {
        tx.rollback().await.ok();
        return Ok(0);
    }

    let ids: Vec<Uuid> = rows.iter().map(|row| row.get::<Uuid, _>("id")).collect();
    sqlx::query(
        r#"
        UPDATE reconstruction_jobs
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
        let time_slice_json: Value = row
            .try_get::<Json<Value>, _>("time_slice_jsonb")
            .map(|json| json.0)
            .unwrap_or(Value::Null);
        let time_slice = decode_time_slice(time_slice_json);
        let job = ReconstructionJobRow {
            id: row.get("id"),
            tenant_id: row.get("tenant_id"),
            parcel_id: row.get("parcel_id"),
            time_slice,
            requested_by: row.get("requested_by"),
            auto_approve: row.get("auto_approve"),
            attempt_count: row.get("attempt_count"),
        };

        debug!(
            job_id = %job.id,
            tenant_id = %job.tenant_id,
            parcel_id = %job.parcel_id,
            auto_approve = job.auto_approve,
            "claimed procedural reconstruction job"
        );

        let outcome = process_reconstruction_job(pool, args, &job).await;
        if let Err(error) =
            mark_reconstruction_job_outcome(pool, job.id, job.attempt_count, outcome, args).await
        {
            error!(job_id = %job.id, %error, "failed to mark reconstruction job outcome");
        }
        handled += 1;
    }
    Ok(handled)
}

#[derive(Debug)]
struct RenderJobRow {
    id: Uuid,
    tenant_id: Uuid,
    spec_id: String,
    spec_version: i32,
    render_tier: String,
    job_kind: String,
    spec_json: Value,
    photo_sources: Value,
    attempt_count: i32,
}

/// One refined asset returned by the Ray Serve renderer app in
/// civic-atlas-ingest. The contract mirrors what
/// `civic_atlas_ingest.scene_foundry.render` already returns: a URI plus a
/// `sha256-<hex>` content hash, extended with the asset type and per-asset
/// provenance metadata.
#[derive(Debug, Clone, Deserialize)]
struct RenderedAsset {
    #[serde(rename = "assetId", default)]
    asset_id: Option<String>,
    #[serde(rename = "assetType")]
    asset_type: String,
    uri: String,
    #[serde(rename = "contentHash")]
    content_hash: String,
    #[serde(default)]
    metadata: Value,
}

#[derive(Debug, Deserialize)]
struct RenderResponse {
    status: String,
    #[serde(default)]
    assets: Vec<RenderedAsset>,
    #[serde(default)]
    error: Option<String>,
}

/// Claim and dispatch Scene Foundry GPU refinement jobs. When no renderer
/// endpoint is configured, rows stay pending untouched: refinement waits
/// for a deployed renderer instead of burning retry attempts.
async fn claim_and_process_render_jobs(pool: &PgPool, args: &Args) -> Result<usize> {
    if args.scene_foundry_render_url.trim().is_empty() {
        return Ok(0);
    }
    let mut tx = pool.begin().await?;
    let rows = sqlx::query(
        r#"
        SELECT id, tenant_id, spec_id, spec_version, render_tier, job_kind,
               spec_jsonb, photo_sources_jsonb, attempt_count
        FROM scene_foundry_render_jobs
        WHERE status = 'pending'
          AND (next_attempt_at IS NULL OR next_attempt_at <= now())
        ORDER BY created_at ASC
        FOR UPDATE SKIP LOCKED
        LIMIT $1
        "#,
    )
    .bind(args.scene_foundry_render_job_batch_size as i64)
    .fetch_all(&mut *tx)
    .await?;

    if rows.is_empty() {
        tx.rollback().await.ok();
        return Ok(0);
    }

    let ids: Vec<Uuid> = rows.iter().map(|row| row.get::<Uuid, _>("id")).collect();
    sqlx::query(
        r#"
        UPDATE scene_foundry_render_jobs
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
        let job = RenderJobRow {
            id: row.get("id"),
            tenant_id: row.get("tenant_id"),
            spec_id: row.get("spec_id"),
            spec_version: row.get("spec_version"),
            render_tier: row.get("render_tier"),
            job_kind: row.get("job_kind"),
            spec_json: row
                .try_get::<Json<Value>, _>("spec_jsonb")
                .map(|json| json.0)
                .unwrap_or(Value::Null),
            photo_sources: row
                .try_get::<Json<Value>, _>("photo_sources_jsonb")
                .map(|json| json.0)
                .unwrap_or_else(|_| json!([])),
            attempt_count: row.get("attempt_count"),
        };
        debug!(
            job_id = %job.id,
            spec_id = %job.spec_id,
            job_kind = %job.job_kind,
            "claimed scene foundry render job"
        );
        let outcome = dispatch_render_job(pool, args, &job).await;
        if let Err(error) =
            mark_render_job_outcome(pool, job.id, job.attempt_count, outcome, args).await
        {
            error!(job_id = %job.id, %error, "failed to mark render job outcome");
        }
        handled += 1;
    }
    Ok(handled)
}

async fn dispatch_render_job(
    pool: &PgPool,
    args: &Args,
    job: &RenderJobRow,
) -> Result<Vec<RenderedAsset>, ProjectionError> {
    let tenant_slug = resolve_tenant_slug(pool, job.tenant_id)
        .await
        .map_err(|error| ProjectionError::Transient(format!("tenant lookup: {error}")))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(args.scene_foundry_render_timeout_secs))
        .build()
        .map_err(|error| ProjectionError::Transient(format!("http client: {error}")))?;

    let payload = json!({
        "tenant": tenant_slug,
        "specId": job.spec_id,
        "specVersion": job.spec_version,
        "renderTier": job.render_tier,
        "jobKind": job.job_kind,
        "spec": job.spec_json,
        "photoSources": job.photo_sources,
    });

    let response = client
        .post(args.scene_foundry_render_url.trim_end_matches('/'))
        .json(&payload)
        .send()
        .await
        .map_err(|error| ProjectionError::Transient(format!("render dispatch: {error}")))?;

    let status = response.status();
    if status.is_client_error() {
        let body = response.text().await.unwrap_or_default();
        return Err(ProjectionError::Permanent(format!(
            "renderer rejected job ({status}): {body}"
        )));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ProjectionError::Transient(format!(
            "renderer error ({status}): {body}"
        )));
    }

    let render: RenderResponse = response
        .json()
        .await
        .map_err(|error| ProjectionError::Transient(format!("render response decode: {error}")))?;
    if render.status != "succeeded" {
        let message = render
            .error
            .unwrap_or_else(|| format!("renderer returned status {}", render.status));
        return Err(ProjectionError::Transient(message));
    }
    if render.assets.is_empty() {
        return Err(ProjectionError::Permanent(
            "renderer reported success with zero assets".to_string(),
        ));
    }
    for asset in &render.assets {
        if asset.content_hash.trim().is_empty() || asset.uri.trim().is_empty() {
            return Err(ProjectionError::Permanent(
                "renderer asset missing uri or content hash".to_string(),
            ));
        }
    }

    // Upsert refined assets. generated_assets is keyed (tenant_id, asset_id)
    // so refinement upgrades land idempotently next to the synchronous
    // massing assets.
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| ProjectionError::Transient(format!("tx begin: {error}")))?;
    set_tx_tenant(&mut tx, job.tenant_id)
        .await
        .map_err(|error| ProjectionError::Transient(format!("set tenant: {error}")))?;
    for (index, asset) in render.assets.iter().enumerate() {
        let asset_id = asset.asset_id.clone().unwrap_or_else(|| {
            format!(
                "scene-foundry:{}:v{}:{}:{}",
                job.spec_id, job.spec_version, job.job_kind, index
            )
        });
        sqlx::query(
            r#"
            INSERT INTO generated_assets (
              tenant_id, asset_id, spec_id, spec_version, asset_type, uri,
              content_hash, metadata_jsonb
            )
            VALUES ($1, $2, $3, $4, $5, $6, NULLIF($7, ''), $8)
            ON CONFLICT (tenant_id, asset_id) DO UPDATE
            SET asset_type = EXCLUDED.asset_type,
                uri = EXCLUDED.uri,
                content_hash = EXCLUDED.content_hash,
                metadata_jsonb = EXCLUDED.metadata_jsonb
            "#,
        )
        .bind(job.tenant_id)
        .bind(&asset_id)
        .bind(&job.spec_id)
        .bind(job.spec_version)
        .bind(&asset.asset_type)
        .bind(&asset.uri)
        .bind(&asset.content_hash)
        .bind(Json(asset.metadata.clone()))
        .execute(&mut *tx)
        .await
        .map_err(|error| ProjectionError::Transient(format!("asset upsert: {error}")))?;
    }
    tx.commit()
        .await
        .map_err(|error| ProjectionError::Transient(format!("tx commit: {error}")))?;

    Ok(render.assets)
}

async fn mark_render_job_outcome(
    pool: &PgPool,
    id: Uuid,
    attempt_count: i32,
    outcome: Result<Vec<RenderedAsset>, ProjectionError>,
    args: &Args,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    match outcome {
        Ok(assets) => {
            let result_assets = json!(assets
                .iter()
                .map(|asset| {
                    json!({
                        "assetType": asset.asset_type,
                        "uri": asset.uri,
                        "contentHash": asset.content_hash,
                        "metadata": asset.metadata,
                    })
                })
                .collect::<Vec<_>>());
            sqlx::query(
                r#"
                UPDATE scene_foundry_render_jobs
                SET status = 'succeeded',
                    attempt_count = attempt_count + 1,
                    result_assets_jsonb = $2,
                    last_error = NULL,
                    updated_at = now(),
                    next_attempt_at = NULL
                WHERE id = $1
                "#,
            )
            .bind(id)
            .bind(Json(result_assets))
            .execute(&mut *tx)
            .await?;
        }
        Err(ProjectionError::Permanent(msg)) => {
            sqlx::query(
                r#"
                UPDATE scene_foundry_render_jobs
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
                sqlx::query(
                    r#"
                    UPDATE scene_foundry_render_jobs
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
                let backoff = backoff_for_attempt(args.backoff_base_secs, next_attempt_count);
                sqlx::query(
                    r#"
                    UPDATE scene_foundry_render_jobs
                    SET status = 'pending',
                        attempt_count = $2,
                        last_error = $3,
                        updated_at = now(),
                        next_attempt_at = now() + make_interval(secs => $4)
                    WHERE id = $1
                    "#,
                )
                .bind(id)
                .bind(next_attempt_count)
                .bind(&msg)
                .bind(backoff as f64)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn process_reconstruction_job(
    pool: &PgPool,
    args: &Args,
    job: &ReconstructionJobRow,
) -> Result<ProcessedReconstruction, ProjectionError> {
    let tenant_slug = resolve_tenant_slug(pool, job.tenant_id)
        .await
        .map_err(|error| ProjectionError::Transient(format!("tenant lookup: {error}")))?;
    if args.theseus_bridge_url.trim().is_empty() {
        let provider = ZeroEmbeddingProvider::default();
        run_reconstruction_with_provider(pool, args, job, &tenant_slug, &provider).await
    } else {
        let provider = TheseusBatchEmbeddingProvider::new(args.theseus_bridge_url.clone());
        run_reconstruction_with_provider(pool, args, job, &tenant_slug, &provider).await
    }
}

async fn run_reconstruction_with_provider<E>(
    pool: &PgPool,
    args: &Args,
    job: &ReconstructionJobRow,
    tenant_slug: &str,
    embedding_provider: &E,
) -> Result<ProcessedReconstruction, ProjectionError>
where
    E: civic_atlas_reconstruction_engine::EmbeddingProvider,
{
    let request = ReconstructionRequest {
        tenant_context: TenantContext {
            tenant_id: tenant_slug.to_string(),
            atlas_node_id: format!("atlas:{tenant_slug}"),
            metadata: Default::default(),
        },
        parcel_id: job.parcel_id.clone(),
        time_slice: job.time_slice,
        requested_by: job.requested_by.clone(),
        auto_approve: job.auto_approve,
    };
    let repository = PostgisRepository::new(pool.clone());
    let model = PairformerCivicPriorModel::default();
    let store = Arc::new(LocalDirAssetStore::new(
        args.scene_foundry_asset_dir.clone(),
        args.scene_foundry_public_base_url.clone(),
    ));
    let generator = SceneFoundryRenderer::new(store)
        .with_job_queue(Arc::new(PgRenderJobQueue::new(pool.clone())));
    let output = run_full_pipeline(request, &repository, embedding_provider, &model, &generator)
        .await
        .map_err(|error| ProjectionError::Transient(format!("pipeline: {error}")))?;
    persist_reconstruction_output(pool, job, tenant_slug, output)
        .await
        .map_err(|error| ProjectionError::Transient(format!("persist: {error}")))
}

async fn persist_reconstruction_output(
    pool: &PgPool,
    job: &ReconstructionJobRow,
    tenant_slug: &str,
    output: PipelineOutput,
) -> Result<ProcessedReconstruction> {
    let mut spec = output.merged.spec.clone();
    attach_manifest_to_spec(&mut spec, &output.asset_manifest);
    // Persist the same texture provenance the renderer recorded for what it
    // actually produced: procedural PBR massing until a GPU appearance pass
    // upgrades it to archival_photo.
    let tier_decision = select_tier(&spec, &TierThresholds::default());
    stamp_massing_texture_provenance(&mut spec, tier_decision.tier);
    spec.tenant_context = Some(TenantContext {
        tenant_id: tenant_slug.to_string(),
        atlas_node_id: format!("atlas:{tenant_slug}"),
        metadata: Default::default(),
    });
    spec.status = if job.auto_approve {
        ReconstructionSpecStatus::Approved as i32
    } else {
        ReconstructionSpecStatus::InReview as i32
    };
    if job.auto_approve {
        spec.reviewed_by = "procedural-reconstruction-engine".to_string();
    }
    if spec.created_by.trim().is_empty() {
        spec.created_by = job.requested_by.clone();
    }

    let spec_json = reconstruction_spec_to_json(&spec);
    let building_id = optional_uuid(&spec.building_id)?;
    let mut tx = pool.begin().await?;
    set_tx_tenant(&mut tx, job.tenant_id).await?;
    let parcel_id = resolve_optional_parcel_uuid(&mut tx, job.tenant_id, &spec.parcel_id).await?;
    let result = sqlx::query(
        r#"
        INSERT INTO reconstruction_specs (
          tenant_id, spec_id, version, status, building_id, parcel_id,
          civic_object_id, block_id, title, supersedes_spec_id, spec_jsonb,
          created_by, reviewed_by, approved_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, NULLIF($8, ''), $9, NULLIF($10, ''),
                $11, $12, NULLIF($13, ''), CASE WHEN $4 = 'approved' THEN now() ELSE NULL END, now())
        ON CONFLICT (tenant_id, spec_id, version) DO UPDATE
        SET status = EXCLUDED.status,
            building_id = EXCLUDED.building_id,
            parcel_id = EXCLUDED.parcel_id,
            civic_object_id = EXCLUDED.civic_object_id,
            block_id = EXCLUDED.block_id,
            title = EXCLUDED.title,
            supersedes_spec_id = EXCLUDED.supersedes_spec_id,
            spec_jsonb = EXCLUDED.spec_jsonb,
            created_by = EXCLUDED.created_by,
            reviewed_by = EXCLUDED.reviewed_by,
            approved_at = EXCLUDED.approved_at,
            updated_at = now()
        WHERE reconstruction_specs.status <> 'approved'
        "#,
    )
    .bind(job.tenant_id)
    .bind(&spec.spec_id)
    .bind(spec.spec_version as i32)
    .bind(spec_status_sql(spec.status))
    .bind(building_id)
    .bind(parcel_id)
    .bind(&spec.civic_object_id)
    .bind(&spec.block_id)
    .bind(&spec.title)
    .bind(&spec.supersedes_spec_id)
    .bind(Json(&spec_json))
    .bind(&spec.created_by)
    .bind(&spec.reviewed_by)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(anyhow!("approved reconstruction specs are immutable"));
    }

    for asset in &spec.assets {
        sqlx::query(
            r#"
            INSERT INTO generated_assets (
              tenant_id, asset_id, spec_id, spec_version, asset_type, uri,
              content_hash, metadata_jsonb
            )
            VALUES ($1, $2, $3, $4, $5, $6, NULLIF($7, ''), $8)
            ON CONFLICT (tenant_id, asset_id) DO UPDATE
            SET asset_type = EXCLUDED.asset_type,
                uri = EXCLUDED.uri,
                content_hash = EXCLUDED.content_hash,
                metadata_jsonb = EXCLUDED.metadata_jsonb
            "#,
        )
        .bind(job.tenant_id)
        .bind(&asset.asset_id)
        .bind(&asset.spec_id)
        .bind(asset.spec_version as i32)
        .bind(&asset.asset_type)
        .bind(&asset.uri)
        .bind(&asset.content_hash)
        .bind(Json(json!(asset.metadata)))
        .execute(&mut *tx)
        .await?;
    }

    if job.auto_approve {
        let building_id = building_id
            .ok_or_else(|| anyhow!("auto-approved reconstruction requires a UUID building_id"))?;
        for part in reconstruction_part_records(&spec) {
            sqlx::query(
                r#"
                INSERT INTO building_parts (
                  tenant_id, building_id, part_key, part_type, payload_jsonb,
                  confidence, source_ids, updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, now())
                ON CONFLICT (tenant_id, building_id, part_key) DO UPDATE
                SET part_type = EXCLUDED.part_type,
                    payload_jsonb = EXCLUDED.payload_jsonb,
                    confidence = EXCLUDED.confidence,
                    source_ids = EXCLUDED.source_ids,
                    updated_at = now()
                "#,
            )
            .bind(job.tenant_id)
            .bind(building_id)
            .bind(&part.key)
            .bind(&part.part_type)
            .bind(Json(&part.payload))
            .bind(part.confidence)
            .bind(&part.source_ids)
            .execute(&mut *tx)
            .await?;
        }
        enqueue_building_presence_projection(&mut tx, job.tenant_id, &spec).await?;
    }

    tx.commit().await?;

    let stage_report = json!({
        "evidence": {
            "directCount": output.evidence.direct.len(),
            "adjacentCount": output.evidence.adjacent.len(),
            "hasTemporalPredecessor": output.evidence.temporal_predecessor.is_some(),
            "hasTemporalSuccessor": output.evidence.temporal_successor.is_some()
        },
        "directExtraction": {
            "populatedFields": output.direct.populated_fields
        },
        "blockSubgraph": {
            "nodeCount": output.block_subgraph.nodes.len(),
            "edgeCount": output.block_subgraph.edges.len(),
            "focusNode": output.block_subgraph.focus_node
        },
        "embeddings": {
            "model": output.embedded_subgraph.embedding_model,
            "modelVersion": output.embedded_subgraph.embedding_model_version,
            "missingCount": output.embedded_subgraph.nodes.iter().filter(|node| node.missing_embedding).count()
        },
        "prior": {
            "modelVersion": output.prior.model_version,
            "edgeConfidences": output.prior.edge_confidences
        },
        "merge": {
            "conflicts": output.merged.conflicts
        },
        "assets": output.asset_manifest
    });

    Ok(ProcessedReconstruction {
        spec_id: spec.spec_id,
        spec_version: spec.spec_version,
        stage_report,
    })
}

async fn enqueue_building_presence_projection(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    spec: &ReconstructionSpec,
) -> Result<()> {
    let idempotency_key = format!(
        "procedural-reconstruction:building-presence:{tenant_id}:{}:{}",
        spec.spec_id, spec.spec_version
    );
    let payload = json!({
        "projectionKind": "BuildingPresence",
        "specId": spec.spec_id,
        "specVersion": spec.spec_version,
        "buildingId": spec.building_id,
        "civicObjectId": spec.civic_object_id,
    });
    sqlx::query(
        r#"
        INSERT INTO reconstruction_projection_outbox (
          tenant_id, spec_id, spec_version, projection_kind, idempotency_key,
          payload_jsonb, status
        )
        VALUES ($1, $2, $3, 'BuildingPresence', $4, $5, 'pending')
        ON CONFLICT (tenant_id, idempotency_key) DO NOTHING
        "#,
    )
    .bind(tenant_id)
    .bind(&spec.spec_id)
    .bind(spec.spec_version as i32)
    .bind(idempotency_key)
    .bind(Json(payload))
    .execute(&mut **tx)
    .await?;
    Ok(())
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
        "BuildingPresence" | "rustyred_building_presence" => {
            project_building_presence(args, payload).await
        }
        other => Err(ProjectionError::Permanent(format!(
            "unknown projection_kind: {other}"
        ))),
    }
}

async fn project_building_presence(args: &Args, payload: &Value) -> Result<(), ProjectionError> {
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
                let backoff_seconds =
                    backoff_for_attempt(args.backoff_base_secs, next_attempt_count);
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

async fn mark_reconstruction_job_outcome(
    pool: &PgPool,
    id: Uuid,
    attempt_count: i32,
    outcome: Result<ProcessedReconstruction, ProjectionError>,
    args: &Args,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    match outcome {
        Ok(processed) => {
            sqlx::query(
                r#"
                UPDATE reconstruction_jobs
                SET status = 'succeeded',
                    attempt_count = attempt_count + 1,
                    resulting_spec_id = $2,
                    resulting_spec_version = $3,
                    stage_report_jsonb = $4,
                    last_error = NULL,
                    updated_at = now(),
                    next_attempt_at = NULL
                WHERE id = $1
                "#,
            )
            .bind(id)
            .bind(&processed.spec_id)
            .bind(processed.spec_version as i32)
            .bind(Json(processed.stage_report))
            .execute(&mut *tx)
            .await?;
        }
        Err(ProjectionError::Permanent(msg)) => {
            sqlx::query(
                r#"
                UPDATE reconstruction_jobs
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
                sqlx::query(
                    r#"
                    UPDATE reconstruction_jobs
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
                let backoff_seconds =
                    backoff_for_attempt(args.backoff_base_secs, next_attempt_count);
                sqlx::query(
                    r#"
                    UPDATE reconstruction_jobs
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

async fn resolve_tenant_slug(pool: &PgPool, tenant_id: Uuid) -> Result<String> {
    let slug: Option<String> = sqlx::query_scalar("SELECT slug FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(pool)
        .await?;
    slug.ok_or_else(|| anyhow!("tenant not found: {tenant_id}"))
}

fn decode_time_slice(value: Value) -> TimeSlice {
    let decoded: TimeSlicePayload = serde_json::from_value(value).unwrap_or_default();
    TimeSlice {
        at_ms: decoded.at_ms,
        start_ms: decoded.start_ms,
        end_ms: decoded.end_ms,
    }
}

fn optional_uuid(value: &str) -> Result<Option<Uuid>> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    value
        .parse()
        .map(Some)
        .with_context(|| format!("invalid UUID value: {value}"))
}

async fn resolve_optional_parcel_uuid(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    parcel_ref: &str,
) -> Result<Option<Uuid>> {
    let parcel_ref = parcel_ref.trim();
    if parcel_ref.is_empty() {
        return Ok(None);
    }
    let parsed_uuid = parcel_ref.parse::<Uuid>().ok();
    let parcel_id: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id
        FROM parcels
        WHERE tenant_id = $1
          AND (($2::uuid IS NOT NULL AND id = $2) OR parcel_key = $3)
        LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .bind(parsed_uuid)
    .bind(parcel_ref)
    .fetch_optional(&mut **tx)
    .await?;
    parcel_id
        .map(Some)
        .ok_or_else(|| anyhow!("parcel not found for reconstruction parcel_ref: {parcel_ref}"))
}

async fn set_tx_tenant(tx: &mut Transaction<'_, Postgres>, tenant_id: Uuid) -> Result<()> {
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn spec_status_sql(status: i32) -> &'static str {
    match ReconstructionSpecStatus::try_from(status).ok() {
        Some(ReconstructionSpecStatus::InReview) => "in_review",
        Some(ReconstructionSpecStatus::Approved) => "approved",
        Some(ReconstructionSpecStatus::Superseded) => "superseded",
        Some(ReconstructionSpecStatus::Rejected) => "rejected",
        _ => "draft",
    }
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
    use serde_json::json;

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

    #[test]
    fn building_presence_payload_accepts_usd_spec_version_alias() {
        let payload: BuildingPresencePayload = serde_json::from_value(json!({
            "specId": "spec:ct-001",
            "specVersion": 7,
            "buildingId": "building-001",
            "civicObjectId": "building:ct-001"
        }))
        .expect("specVersion payload decodes");

        assert_eq!(payload.spec_version, 7);
    }

    #[test]
    fn building_presence_payload_still_accepts_legacy_version() {
        let payload: BuildingPresencePayload = serde_json::from_value(json!({
            "specId": "spec:ct-001",
            "version": 3,
            "buildingId": "building-001",
            "civicObjectId": "building:ct-001"
        }))
        .expect("legacy version payload decodes");

        assert_eq!(payload.spec_version, 3);
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
