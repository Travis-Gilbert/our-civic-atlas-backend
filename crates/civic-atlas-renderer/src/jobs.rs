//! GPU refinement job dispatch.
//!
//! The synchronous render inside `generate_assets` must return promptly, so
//! the model-inference stages (facade parsing, inpainting, PyTorch3D
//! fitting, splatting, MASt3R/VGGT, Open3D meshing, Blender archetypes) run
//! asynchronously: the renderer enqueues a row in
//! `scene_foundry_render_jobs`, the outbox worker claims it and calls the
//! Ray Serve renderer app in `civic-atlas-ingest`, and the returned assets
//! upsert into `generated_assets` out-of-band.
//!
//! The job payload is self-contained: the merged spec JSON plus the photo
//! sources (id, uri, title), so the GPU lane never needs to re-derive
//! evidence.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;
use sqlx::PgPool;

use civic_atlas_reconstruction_engine::reconstruction_spec_to_json;
use civic_atlas_types::civic_atlas::v1::ReconstructionSpec;

use crate::tier::TierDecision;

/// Outcome of an enqueue attempt, recorded into the manifest metadata so
/// the refinement state is honest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    Queued,
    AlreadyQueued,
}

impl EnqueueOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            EnqueueOutcome::Queued => "queued",
            EnqueueOutcome::AlreadyQueued => "already_queued",
        }
    }
}

#[async_trait]
pub trait RenderJobQueue: Send + Sync {
    async fn enqueue(
        &self,
        spec: &ReconstructionSpec,
        decision: &TierDecision,
    ) -> Result<EnqueueOutcome>;
}

/// Postgres-backed queue writing `scene_foundry_render_jobs` rows
/// (migration 0024). Tenant scoping follows the repo invariant: the row
/// carries the tenant uuid resolved from the spec's tenant slug.
#[derive(Clone)]
pub struct PgRenderJobQueue {
    pool: PgPool,
}

impl PgRenderJobQueue {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RenderJobQueue for PgRenderJobQueue {
    async fn enqueue(
        &self,
        spec: &ReconstructionSpec,
        decision: &TierDecision,
    ) -> Result<EnqueueOutcome> {
        let tenant_slug = spec
            .tenant_context
            .as_ref()
            .map(|tenant| tenant.tenant_id.as_str())
            .filter(|slug| !slug.is_empty())
            .unwrap_or("flint");
        let spec_json = reconstruction_spec_to_json(spec);
        let photo_sources = json!(decision
            .photo_sources
            .iter()
            .map(|source| {
                json!({
                    "sourceId": source.source_id,
                    "uri": source.uri,
                    "title": source.title,
                    "capturedAtMs": source.captured_at_ms,
                })
            })
            .collect::<Vec<_>>());

        let result = sqlx::query(
            r#"
            INSERT INTO scene_foundry_render_jobs (
              tenant_id, spec_id, spec_version, render_tier, job_kind,
              spec_jsonb, photo_sources_jsonb
            )
            SELECT t.id, $2, $3, $4, $5, $6, $7
            FROM tenants t
            WHERE t.slug = $1
            ON CONFLICT (tenant_id, spec_id, spec_version, job_kind)
              DO NOTHING
            "#,
        )
        .bind(tenant_slug)
        .bind(&spec.spec_id)
        .bind(spec.spec_version as i32)
        .bind(decision.tier.as_str())
        .bind(decision.tier.refinement_kind())
        .bind(sqlx::types::Json(&spec_json))
        .bind(sqlx::types::Json(&photo_sources))
        .execute(&self.pool)
        .await
        .context("enqueue scene foundry render job")?;

        if result.rows_affected() > 0 {
            Ok(EnqueueOutcome::Queued)
        } else {
            Ok(EnqueueOutcome::AlreadyQueued)
        }
    }
}
