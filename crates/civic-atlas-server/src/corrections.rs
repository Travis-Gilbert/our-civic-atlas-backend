//! Phase 4 community correction loop service.
//!
//! Implements `CorrectionService` from `proto/civic_atlas/v1/corrections.proto`.
//! Backed by the polymorphic `corrections` table from migration 0002 plus
//! the Phase 4 extensions in migration 0003 (`changelog_entries`,
//! `correction_rate_limits`, and the accepted-immutability trigger).

#![allow(clippy::result_large_err)]

use civic_atlas_types::civic_atlas::v1::correction_service_server::CorrectionService;
use civic_atlas_types::civic_atlas::v1::{
    ApproveCorrectionRequest, ApproveCorrectionResponse, ChangelogEntry,
    CommunityCorrectionPayload, CorrectionKind, CorrectionStatus, CorrectionSubmission,
    CorrectionTargetType, ExportCorrectionsTrainingDataRequest,
    ExportCorrectionsTrainingDataResponse, ListChangelogEntriesRequest,
    ListChangelogEntriesResponse, ListCorrectionsForBuildingRequest,
    ListCorrectionsForBuildingResponse, ListPendingCorrectionsRequest,
    ListPendingCorrectionsResponse, RejectCorrectionRequest, RejectCorrectionResponse,
    SubmitCorrectionRequest, SubmitCorrectionResponse, TenantContext, TrainingExample,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgRow, types::Json, PgPool, Postgres, Row, Transaction};
use tenant_resolver::require_tenant_context;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::AtlasState;

/// Phase 4 spec ceiling: 10 anonymous submissions per IP per hour.
const ANONYMOUS_HOURLY_LIMIT: i32 = 10;

/// Hour bucket helper shared by submit + rate-limit lookup.
fn hour_bucket_for_now() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    secs / 3600
}

#[derive(Clone)]
pub struct CorrectionGrpcService {
    state: AtlasState,
}

impl CorrectionGrpcService {
    pub fn new(state: AtlasState) -> Self {
        Self { state }
    }

    fn pool(&self) -> Result<&PgPool, Status> {
        self.state.db_pool().ok_or_else(|| {
            Status::unavailable("DATABASE_URL is required for CorrectionService")
        })
    }
}

#[tonic::async_trait]
impl CorrectionService for CorrectionGrpcService {
    async fn submit_correction(
        &self,
        request: Request<SubmitCorrectionRequest>,
    ) -> Result<Response<SubmitCorrectionResponse>, Status> {
        let request = request.into_inner();
        let submission = request
            .submission
            .ok_or_else(|| Status::invalid_argument("submission is required"))?;
        let tenant = require_tenant_context(submission.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;

        if submission.target_id.trim().is_empty() {
            return Err(Status::invalid_argument("target_id is required"));
        }
        let target_id_uuid = Uuid::parse_str(submission.target_id.trim())
            .map_err(|_| Status::invalid_argument("target_id must be a UUID"))?;

        let correction_key = if submission.correction_key.trim().is_empty() {
            Uuid::new_v4().to_string()
        } else {
            submission.correction_key.trim().to_string()
        };

        let target_type_str = correction_target_type_to_sql(submission.target_type)?;
        let kind_str = correction_kind_to_sql(submission.kind)?;
        let status_str = "open"; // submissions always start open
        let is_anonymous = submission.submitted_by.trim().is_empty();
        let submitter_ip_hash = if submission.submitter_ip_hash.trim().is_empty() {
            None
        } else {
            Some(submission.submitter_ip_hash.trim().to_string())
        };

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;

        // Rate-limit check: anonymous submissions only.
        if is_anonymous {
            let ip_hash = submitter_ip_hash.as_deref().ok_or_else(|| {
                Status::invalid_argument(
                    "anonymous submissions require submitter_ip_hash for rate limiting",
                )
            })?;
            let bucket = hour_bucket_for_now();
            let count = upsert_rate_limit(&mut tx, tenant_id, ip_hash, bucket).await?;
            if count > ANONYMOUS_HOURLY_LIMIT {
                return Err(Status::resource_exhausted(format!(
                    "anonymous submission rate limit exceeded ({} per hour)",
                    ANONYMOUS_HOURLY_LIMIT
                )));
            }
        }

        let payload_jsonb = community_payload_to_json(submission.payload.as_ref());

        let row = sqlx::query(
            r#"
            INSERT INTO corrections (
                tenant_id, correction_key, target_type, target_id,
                correction_type, status, payload_jsonb,
                submitted_by, submitter_ip_hash
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id, correction_key, status, target_type, target_id,
                      correction_type, submitted_by, submitter_ip_hash,
                      payload_jsonb, reviewed_by, reviewed_at, created_at,
                      moderator_notes, accepted_part_selectors,
                      resulting_spec_id, resulting_spec_version
            "#,
        )
        .bind(tenant_id)
        .bind(&correction_key)
        .bind(target_type_str)
        .bind(target_id_uuid)
        .bind(kind_str)
        .bind(status_str)
        .bind(Json(&payload_jsonb))
        .bind(if submission.submitted_by.is_empty() {
            ""
        } else {
            submission.submitted_by.as_str()
        })
        .bind(submitter_ip_hash.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        let mut stored = submission_from_row(&row, tenant.as_str())?;
        // Preserve tenant_context shape from request for the response.
        stored.tenant_context = Some(TenantContext {
            tenant_id: tenant.as_str().to_string(),
            atlas_node_id: submission
                .tenant_context
                .as_ref()
                .map(|tc| tc.atlas_node_id.clone())
                .unwrap_or_default(),
            metadata: submission
                .tenant_context
                .as_ref()
                .map(|tc| tc.metadata.clone())
                .unwrap_or_default(),
        });

        Ok(Response::new(SubmitCorrectionResponse {
            submission: Some(stored),
        }))
    }

    async fn list_corrections_for_building(
        &self,
        request: Request<ListCorrectionsForBuildingRequest>,
    ) -> Result<Response<ListCorrectionsForBuildingResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.building_id.trim().is_empty() {
            return Err(Status::invalid_argument("building_id is required"));
        }
        let building_uuid = Uuid::parse_str(request.building_id.trim())
            .map_err(|_| Status::invalid_argument("building_id must be a UUID"))?;

        let limit = request.page_size.clamp(1, 200) as i64;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;

        // Match corrections whose target is the building itself OR any
        // building_part of the building. The polymorphic target makes
        // this a two-arm OR.
        let rows = sqlx::query(
            r#"
            SELECT id, correction_key, status, target_type, target_id,
                   correction_type, submitted_by, submitter_ip_hash,
                   payload_jsonb, reviewed_by, reviewed_at, created_at,
                   moderator_notes, accepted_part_selectors,
                   resulting_spec_id, resulting_spec_version
            FROM corrections
            WHERE tenant_id = $1
              AND (
                  (target_type = 'building' AND target_id = $2)
                  OR (target_type = 'building_part' AND target_id IN (
                      SELECT id FROM building_parts
                      WHERE tenant_id = $1 AND building_id = $2
                  ))
              )
              AND ($3::text IS NULL OR status = $3::text)
            ORDER BY created_at DESC
            LIMIT $4
            "#,
        )
        .bind(tenant_id)
        .bind(building_uuid)
        .bind(maybe_status_filter(request.status_filter))
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_status)?;
        tx.commit().await.map_err(db_status)?;

        let submissions: Vec<CorrectionSubmission> = rows
            .iter()
            .map(|row| submission_from_row(row, tenant.as_str()))
            .collect::<Result<_, _>>()?;

        Ok(Response::new(ListCorrectionsForBuildingResponse {
            submissions,
            next_page_token: String::new(),
        }))
    }

    async fn list_pending_corrections(
        &self,
        request: Request<ListPendingCorrectionsRequest>,
    ) -> Result<Response<ListPendingCorrectionsResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let limit = request.page_size.clamp(1, 200) as i64;

        let kind_filter = if request.kind_filter == CorrectionKind::Unspecified as i32 {
            None
        } else {
            Some(correction_kind_to_sql(request.kind_filter)?)
        };

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;

        let rows = sqlx::query(
            r#"
            SELECT id, correction_key, status, target_type, target_id,
                   correction_type, submitted_by, submitter_ip_hash,
                   payload_jsonb, reviewed_by, reviewed_at, created_at,
                   moderator_notes, accepted_part_selectors,
                   resulting_spec_id, resulting_spec_version
            FROM corrections
            WHERE tenant_id = $1
              AND status = 'open'
              AND ($2::text IS NULL OR correction_type = $2::text)
            ORDER BY created_at ASC
            LIMIT $3
            "#,
        )
        .bind(tenant_id)
        .bind(kind_filter)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_status)?;
        tx.commit().await.map_err(db_status)?;

        let submissions: Vec<CorrectionSubmission> = rows
            .iter()
            .map(|row| submission_from_row(row, tenant.as_str()))
            .collect::<Result<_, _>>()?;

        Ok(Response::new(ListPendingCorrectionsResponse {
            submissions,
            next_page_token: String::new(),
        }))
    }

    async fn approve_correction(
        &self,
        request: Request<ApproveCorrectionRequest>,
    ) -> Result<Response<ApproveCorrectionResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.correction_id.trim().is_empty() {
            return Err(Status::invalid_argument("correction_id is required"));
        }
        let correction_uuid = Uuid::parse_str(request.correction_id.trim())
            .map_err(|_| Status::invalid_argument("correction_id must be a UUID"))?;
        if request.approved_by.trim().is_empty() {
            return Err(Status::invalid_argument("approved_by is required"));
        }

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;

        // Lock the row for update so a concurrent approval can't race.
        let row = sqlx::query(
            r#"
            SELECT id, correction_key, status, target_type, target_id,
                   correction_type, submitted_by, submitter_ip_hash,
                   payload_jsonb, reviewed_by, reviewed_at, created_at,
                   moderator_notes, accepted_part_selectors,
                   resulting_spec_id, resulting_spec_version
            FROM corrections
            WHERE tenant_id = $1 AND id = $2
            FOR UPDATE
            "#,
        )
        .bind(tenant_id)
        .bind(correction_uuid)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?
        .ok_or_else(|| Status::not_found("correction not found"))?;

        let current_status: String = row.get("status");
        if current_status != "open" {
            return Err(Status::failed_precondition(format!(
                "correction status must be 'open' to approve; got {current_status}"
            )));
        }

        // For community part corrections, merge the proposed parts onto the
        // current approved spec and write a new spec version.
        let correction_type: String = row.get("correction_type");
        let target_type: String = row.get("target_type");

        // TODO(phase-4): implement the per-part merge against the targeted
        // reconstruction spec. The minimal implementation:
        //
        //   1. Resolve the target reconstruction spec (from target_id if
        //      target_type='reconstruction_spec', or via building_parts->
        //      reconstruction_specs join if target_type='building_part').
        //   2. Load the current approved spec_jsonb.
        //   3. For each accepted_part_selector in request.accepted_part_selectors
        //      (empty list = accept all), patch the corresponding part field
        //      from payload_jsonb.part_changes onto the current spec.
        //   4. Insert a new reconstruction_specs row with version+1 and
        //      status='approved' (since this is a moderator-driven approval,
        //      not a draft -> review flow).
        //   5. Enqueue a reconstruction_projection_outbox row pointing at the
        //      new spec version.
        //
        // Until that lands, we mark the correction accepted but leave
        // resulting_spec_id NULL. The outbox worker (Codex incomplete A) will
        // pick up any explicit reconstruction_specs approvals separately.
        let _ = (correction_type, target_type);

        // Update the correction row to accepted.
        let updated_row = sqlx::query(
            r#"
            UPDATE corrections
            SET status = 'accepted',
                reviewed_by = $1,
                reviewed_at = now(),
                moderator_notes = $2,
                accepted_part_selectors = $3::text[]
            WHERE tenant_id = $4 AND id = $5
            RETURNING id, correction_key, status, target_type, target_id,
                      correction_type, submitted_by, submitter_ip_hash,
                      payload_jsonb, reviewed_by, reviewed_at, created_at,
                      moderator_notes, accepted_part_selectors,
                      resulting_spec_id, resulting_spec_version
            "#,
        )
        .bind(request.approved_by.trim())
        .bind(request.moderator_notes.trim())
        .bind(&request.accepted_part_selectors[..])
        .bind(tenant_id)
        .bind(correction_uuid)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_status)?;

        let public_title = synthesize_changelog_title(&updated_row);
        let public_summary = synthesize_changelog_summary(&updated_row);

        // Create the public changelog entry (idempotent via UNIQUE on
        // (tenant_id, correction_id)).
        let entry_row = sqlx::query(
            r#"
            INSERT INTO changelog_entries (
                tenant_id, correction_id, public_title, public_summary
            ) VALUES ($1, $2, $3, $4)
            ON CONFLICT (tenant_id, correction_id) DO UPDATE
              SET public_title = EXCLUDED.public_title,
                  public_summary = EXCLUDED.public_summary
            RETURNING id, correction_id, public_title, public_summary,
                      resulting_spec_id, resulting_spec_version, published_at
            "#,
        )
        .bind(tenant_id)
        .bind(correction_uuid)
        .bind(&public_title)
        .bind(&public_summary)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        let mut submission = submission_from_row(&updated_row, tenant.as_str())?;
        // Preserve the request's TenantContext shape.
        submission.tenant_context = Some(TenantContext {
            tenant_id: tenant.as_str().to_string(),
            atlas_node_id: request
                .tenant_context
                .as_ref()
                .map(|tc| tc.atlas_node_id.clone())
                .unwrap_or_default(),
            metadata: request
                .tenant_context
                .as_ref()
                .map(|tc| tc.metadata.clone())
                .unwrap_or_default(),
        });

        let changelog = changelog_entry_from_row(&entry_row, tenant.as_str())?;

        Ok(Response::new(ApproveCorrectionResponse {
            submission: Some(submission),
            changelog_entry: Some(changelog),
        }))
    }

    async fn reject_correction(
        &self,
        request: Request<RejectCorrectionRequest>,
    ) -> Result<Response<RejectCorrectionResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.correction_id.trim().is_empty() {
            return Err(Status::invalid_argument("correction_id is required"));
        }
        let correction_uuid = Uuid::parse_str(request.correction_id.trim())
            .map_err(|_| Status::invalid_argument("correction_id must be a UUID"))?;
        if request.rejected_by.trim().is_empty() {
            return Err(Status::invalid_argument("rejected_by is required"));
        }

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;

        let row = sqlx::query(
            r#"
            UPDATE corrections
            SET status = 'rejected',
                reviewed_by = $1,
                reviewed_at = now(),
                moderator_notes = $2
            WHERE tenant_id = $3 AND id = $4 AND status = 'open'
            RETURNING id, correction_key, status, target_type, target_id,
                      correction_type, submitted_by, submitter_ip_hash,
                      payload_jsonb, reviewed_by, reviewed_at, created_at,
                      moderator_notes, accepted_part_selectors,
                      resulting_spec_id, resulting_spec_version
            "#,
        )
        .bind(request.rejected_by.trim())
        .bind(request.moderator_notes.trim())
        .bind(tenant_id)
        .bind(correction_uuid)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?
        .ok_or_else(|| {
            Status::failed_precondition("correction not found or not in 'open' status")
        })?;
        tx.commit().await.map_err(db_status)?;

        let mut submission = submission_from_row(&row, tenant.as_str())?;
        submission.tenant_context = Some(TenantContext {
            tenant_id: tenant.as_str().to_string(),
            atlas_node_id: request
                .tenant_context
                .as_ref()
                .map(|tc| tc.atlas_node_id.clone())
                .unwrap_or_default(),
            metadata: request
                .tenant_context
                .as_ref()
                .map(|tc| tc.metadata.clone())
                .unwrap_or_default(),
        });

        Ok(Response::new(RejectCorrectionResponse {
            submission: Some(submission),
        }))
    }

    async fn list_changelog_entries(
        &self,
        request: Request<ListChangelogEntriesRequest>,
    ) -> Result<Response<ListChangelogEntriesResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let limit = request.page_size.clamp(1, 200) as i64;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;

        let rows = sqlx::query(
            r#"
            SELECT id, correction_id, public_title, public_summary,
                   resulting_spec_id, resulting_spec_version, published_at
            FROM changelog_entries
            WHERE tenant_id = $1
            ORDER BY published_at DESC
            LIMIT $2
            "#,
        )
        .bind(tenant_id)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_status)?;
        tx.commit().await.map_err(db_status)?;

        let entries: Vec<ChangelogEntry> = rows
            .iter()
            .map(|row| changelog_entry_from_row(row, tenant.as_str()))
            .collect::<Result<_, _>>()?;

        Ok(Response::new(ListChangelogEntriesResponse {
            entries,
            next_page_token: String::new(),
        }))
    }

    async fn export_corrections_training_data(
        &self,
        request: Request<ExportCorrectionsTrainingDataRequest>,
    ) -> Result<Response<ExportCorrectionsTrainingDataResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let limit = request.page_size.clamp(1, 1000) as i64;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;

        // Both 'accepted' and 'rejected' are valid training labels.
        // 'open' and 'superseded' are excluded.
        let rows = sqlx::query(
            r#"
            SELECT id, correction_key, status, target_type, target_id,
                   correction_type, submitted_by, submitter_ip_hash,
                   payload_jsonb, reviewed_by, reviewed_at, created_at,
                   moderator_notes, accepted_part_selectors,
                   resulting_spec_id, resulting_spec_version
            FROM corrections
            WHERE tenant_id = $1
              AND status IN ('accepted', 'rejected')
              AND ($2 = 0 OR extract(epoch FROM created_at) * 1000 >= $2)
            ORDER BY created_at ASC
            LIMIT $3
            "#,
        )
        .bind(tenant_id)
        .bind(request.since_ms)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_status)?;
        tx.commit().await.map_err(db_status)?;

        let examples: Vec<TrainingExample> = rows
            .iter()
            .map(|row| -> Result<TrainingExample, Status> {
                let status: String = row.get("status");
                let positive = status == "accepted";
                let submission = submission_from_row(row, tenant.as_str())?;
                Ok(TrainingExample {
                    submission: Some(submission),
                    positive_label: positive,
                })
            })
            .collect::<Result<_, _>>()?;

        Ok(Response::new(ExportCorrectionsTrainingDataResponse {
            examples,
            next_page_token: String::new(),
        }))
    }
}

// --- helpers ----------------------------------------------------------

fn db_status(error: sqlx::Error) -> Status {
    Status::internal(format!("database error: {error}"))
}

async fn resolve_tenant_id(
    tx: &mut Transaction<'_, Postgres>,
    tenant_slug: &str,
) -> Result<Uuid, Status> {
    let row = sqlx::query("SELECT id FROM tenants WHERE slug = $1")
        .bind(tenant_slug)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_status)?;
    row.and_then(|row| row.try_get::<Uuid, _>("id").ok())
        .ok_or_else(|| Status::unauthenticated(format!("unknown tenant: {tenant_slug}")))
}

async fn set_transaction_tenant_uuid(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
) -> Result<(), Status> {
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_status)?;
    Ok(())
}

async fn upsert_rate_limit(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    submitter_ip_hash: &str,
    hour_bucket: i64,
) -> Result<i32, Status> {
    let row = sqlx::query(
        r#"
        INSERT INTO correction_rate_limits (tenant_id, submitter_ip_hash, hour_bucket)
        VALUES ($1, $2, $3)
        ON CONFLICT (tenant_id, submitter_ip_hash, hour_bucket) DO UPDATE
          SET submission_count = correction_rate_limits.submission_count + 1,
              updated_at = now()
        RETURNING submission_count
        "#,
    )
    .bind(tenant_id)
    .bind(submitter_ip_hash)
    .bind(hour_bucket)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_status)?;
    Ok(row.get::<i32, _>("submission_count"))
}

fn community_payload_to_json(payload: Option<&CommunityCorrectionPayload>) -> Value {
    let Some(p) = payload else {
        return json!({});
    };
    // Per-part bodies (Mass/Facade/Roof/Ornament/GroundFloor) are proto
    // messages without serde::Serialize derives. To round-trip them
    // through payload_jsonb without adding serde derives globally, we
    // base64-encode the prost-encoded bytes. The Phase 4 approve flow
    // decodes them back via prost::Message::decode. Selector + proposed
    // field paths stay legible for moderation UI without a decode.
    let part_changes: Vec<Value> = p
        .part_changes
        .iter()
        .map(|change| {
            json!({
                "part_selector": change.part_selector,
                "proposed_field_paths": change.proposed_field_paths,
                "mass_b64": change.mass.as_ref().map(encode_message_b64),
                "facade_b64": change.facade.as_ref().map(encode_message_b64),
                "opening_grid_b64": change.opening_grid.as_ref().map(encode_message_b64),
                "roof_b64": change.roof.as_ref().map(encode_message_b64),
                "ornament_b64": change.ornament.as_ref().map(encode_message_b64),
                "ground_floor_b64": change.ground_floor.as_ref().map(encode_message_b64),
            })
        })
        .collect();
    json!({
        "reasoning": p.reasoning,
        "evidence_artifact_ids": p.evidence_artifact_ids,
        "part_changes": part_changes,
        "evidence_uri_hint": p.evidence_uri_hint,
    })
}

fn encode_message_b64<M: prost::Message>(msg: &M) -> String {
    use base64::Engine;
    let bytes = msg.encode_to_vec();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn community_payload_from_json(value: &Value) -> CommunityCorrectionPayload {
    // Defensive: missing fields become defaults. Round-trip fidelity for
    // PartChange parts is intentionally lossy here; the moderation UI
    // reads from payload_jsonb directly when it needs nested precision.
    CommunityCorrectionPayload {
        reasoning: value
            .get("reasoning")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        evidence_artifact_ids: value
            .get("evidence_artifact_ids")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        part_changes: Vec::new(),
        evidence_uri_hint: value
            .get("evidence_uri_hint")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

fn submission_from_row(row: &PgRow, tenant_slug: &str) -> Result<CorrectionSubmission, Status> {
    let id: Uuid = row.try_get("id").map_err(db_status)?;
    let correction_key: String = row.try_get("correction_key").map_err(db_status)?;
    let status: String = row.try_get("status").map_err(db_status)?;
    let target_type: String = row.try_get("target_type").map_err(db_status)?;
    let target_id: Uuid = row.try_get("target_id").map_err(db_status)?;
    let correction_type: String = row.try_get("correction_type").map_err(db_status)?;
    let submitted_by: String = row.try_get("submitted_by").map_err(db_status)?;
    let submitter_ip_hash: Option<String> = row
        .try_get::<Option<String>, _>("submitter_ip_hash")
        .unwrap_or_default();
    let payload_jsonb: Value = row
        .try_get::<Json<Value>, _>("payload_jsonb")
        .map(|j| j.0)
        .map_err(db_status)?;
    let reviewed_by: Option<String> = row
        .try_get::<Option<String>, _>("reviewed_by")
        .unwrap_or_default();
    let moderator_notes: Option<String> = row
        .try_get::<Option<String>, _>("moderator_notes")
        .unwrap_or_default();
    let accepted_part_selectors: Vec<String> = row
        .try_get::<Vec<String>, _>("accepted_part_selectors")
        .unwrap_or_default();
    let resulting_spec_id: Option<String> = row
        .try_get::<Option<String>, _>("resulting_spec_id")
        .unwrap_or_default();
    let resulting_spec_version: Option<i32> = row
        .try_get::<Option<i32>, _>("resulting_spec_version")
        .unwrap_or_default();
    let reviewed_at_ms = timestamp_ms_from_row(row, "reviewed_at");
    let created_at_ms = timestamp_ms_from_row(row, "created_at");

    Ok(CorrectionSubmission {
        id: id.to_string(),
        tenant_context: Some(TenantContext {
            tenant_id: tenant_slug.to_string(),
            atlas_node_id: String::new(),
            metadata: Default::default(),
        }),
        correction_key,
        target_type: correction_target_type_from_sql(&target_type),
        target_id: target_id.to_string(),
        kind: correction_kind_from_sql(&correction_type),
        status: correction_status_from_sql(&status),
        submitted_by,
        submitter_ip_hash: submitter_ip_hash.unwrap_or_default(),
        payload: Some(community_payload_from_json(&payload_jsonb)),
        reviewed_by: reviewed_by.unwrap_or_default(),
        reviewed_at_ms,
        moderator_notes: moderator_notes.unwrap_or_default(),
        accepted_part_selectors,
        resulting_spec_id: resulting_spec_id.unwrap_or_default(),
        resulting_spec_version: resulting_spec_version.unwrap_or_default() as u32,
        created_at_ms,
    })
}

fn changelog_entry_from_row(row: &PgRow, tenant_slug: &str) -> Result<ChangelogEntry, Status> {
    let id: Uuid = row.try_get("id").map_err(db_status)?;
    let correction_id: Uuid = row.try_get("correction_id").map_err(db_status)?;
    let public_title: String = row.try_get("public_title").map_err(db_status)?;
    let public_summary: String = row
        .try_get::<String, _>("public_summary")
        .unwrap_or_default();
    let resulting_spec_id: Option<String> = row
        .try_get::<Option<String>, _>("resulting_spec_id")
        .unwrap_or_default();
    let resulting_spec_version: Option<i32> = row
        .try_get::<Option<i32>, _>("resulting_spec_version")
        .unwrap_or_default();
    let published_at_ms = timestamp_ms_from_row(row, "published_at");

    Ok(ChangelogEntry {
        id: id.to_string(),
        tenant_context: Some(TenantContext {
            tenant_id: tenant_slug.to_string(),
            atlas_node_id: String::new(),
            metadata: Default::default(),
        }),
        correction_id: correction_id.to_string(),
        public_title,
        public_summary,
        resulting_spec_id: resulting_spec_id.unwrap_or_default(),
        resulting_spec_version: resulting_spec_version.unwrap_or_default() as u32,
        published_at_ms,
    })
}

fn timestamp_ms_from_row(row: &PgRow, column: &str) -> Option<i64> {
    // sqlx exposes `time::OffsetDateTime` for timestamptz when the
    // "time" feature is enabled (workspace default). Two-step decode so
    // the row can also be a non-NULL value without an explicit Option
    // wrapper.
    if let Ok(Some(ts)) = row.try_get::<Option<sqlx::types::time::OffsetDateTime>, _>(column) {
        return Some(ts.unix_timestamp() * 1_000 + i64::from(ts.millisecond()));
    }
    if let Ok(ts) = row.try_get::<sqlx::types::time::OffsetDateTime, _>(column) {
        return Some(ts.unix_timestamp() * 1_000 + i64::from(ts.millisecond()));
    }
    None
}

fn correction_target_type_to_sql(t: i32) -> Result<&'static str, Status> {
    let val = CorrectionTargetType::try_from(t)
        .map_err(|_| Status::invalid_argument(format!("unknown target_type enum value: {t}")))?;
    Ok(match val {
        CorrectionTargetType::Unspecified => {
            return Err(Status::invalid_argument("target_type is required"));
        }
        CorrectionTargetType::Parcel => "parcel",
        CorrectionTargetType::Building => "building",
        CorrectionTargetType::BuildingPart => "building_part",
        CorrectionTargetType::Artifact => "artifact",
        CorrectionTargetType::ArtifactAnchor => "artifact_anchor",
        CorrectionTargetType::ReconstructionSpec => "reconstruction_spec",
        CorrectionTargetType::GeneratedAsset => "generated_asset",
    })
}

fn correction_target_type_from_sql(s: &str) -> i32 {
    match s {
        "parcel" => CorrectionTargetType::Parcel as i32,
        "building" => CorrectionTargetType::Building as i32,
        "building_part" => CorrectionTargetType::BuildingPart as i32,
        "artifact" => CorrectionTargetType::Artifact as i32,
        "artifact_anchor" => CorrectionTargetType::ArtifactAnchor as i32,
        "reconstruction_spec" => CorrectionTargetType::ReconstructionSpec as i32,
        "generated_asset" => CorrectionTargetType::GeneratedAsset as i32,
        _ => CorrectionTargetType::Unspecified as i32,
    }
}

fn correction_kind_to_sql(k: i32) -> Result<&'static str, Status> {
    let val = CorrectionKind::try_from(k)
        .map_err(|_| Status::invalid_argument(format!("unknown kind enum value: {k}")))?;
    Ok(match val {
        CorrectionKind::Unspecified => "community_part_correction",
        CorrectionKind::CommunityPartCorrection => "community_part_correction",
        CorrectionKind::CommunityArtifactAttachment => "community_artifact_attachment",
        CorrectionKind::ModeratorEdit => "moderator_edit",
    })
}

fn correction_kind_from_sql(s: &str) -> i32 {
    match s {
        "community_part_correction" => CorrectionKind::CommunityPartCorrection as i32,
        "community_artifact_attachment" => CorrectionKind::CommunityArtifactAttachment as i32,
        "moderator_edit" => CorrectionKind::ModeratorEdit as i32,
        _ => CorrectionKind::Unspecified as i32,
    }
}

fn correction_status_from_sql(s: &str) -> i32 {
    match s {
        "open" => CorrectionStatus::Open as i32,
        "accepted" => CorrectionStatus::Accepted as i32,
        "rejected" => CorrectionStatus::Rejected as i32,
        "superseded" => CorrectionStatus::Superseded as i32,
        _ => CorrectionStatus::Unspecified as i32,
    }
}

fn maybe_status_filter(status: i32) -> Option<&'static str> {
    match CorrectionStatus::try_from(status).ok()? {
        CorrectionStatus::Unspecified => None,
        CorrectionStatus::Open => Some("open"),
        CorrectionStatus::Accepted => Some("accepted"),
        CorrectionStatus::Rejected => Some("rejected"),
        CorrectionStatus::Superseded => Some("superseded"),
    }
}

fn synthesize_changelog_title(row: &PgRow) -> String {
    let target_type: String = row.try_get("target_type").unwrap_or_default();
    let correction_type: String = row.try_get("correction_type").unwrap_or_default();
    format!(
        "{} correction accepted on {}",
        correction_type.replace('_', " "),
        target_type.replace('_', " "),
    )
}

fn synthesize_changelog_summary(row: &PgRow) -> String {
    let selectors: Vec<String> = row
        .try_get::<Vec<String>, _>("accepted_part_selectors")
        .unwrap_or_default();
    if selectors.is_empty() {
        String::from("Whole-submission accept.")
    } else {
        format!("Accepted parts: {}", selectors.join(", "))
    }
}
