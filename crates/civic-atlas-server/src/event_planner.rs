//! Porchfest Planner Phase 1 — read-only EventPlannerService.
//!
//! Implements the three list RPCs from `proto/civic_atlas/v1/event_planner.proto`
//! against the tables in migration `0011_event_layers.sql`. Phase 2 adds
//! write methods; Phase 3 adds day-of-event status transitions.
//!
//! Pattern mirrors `corrections.rs`:
//!   1. Pull `TenantContext` off the request and validate it via
//!      `tenant_resolver::require_tenant_context`.
//!   2. Open a sqlx transaction.
//!   3. Resolve the tenant slug to a `tenants.id` uuid.
//!   4. Set the per-transaction GUC `app.tenant_id` so RLS policies
//!      enforce tenant isolation for every subsequent query on the
//!      transaction.
//!   5. Run the read queries with `ST_AsGeoJSON` so geometry travels
//!      to the GraphQL sidecar as a GeoJSON string the browser can
//!      drop straight into deck.gl.
//!
//! Why a transaction even for reads: `set_config(..., true)` only
//! lives for the current transaction, so a bare connection would
//! leak (or fail to apply) tenancy. The sqlx pool is small (max 5)
//! and a read-only transaction is cheap.
//!
//! Timestamps are surfaced as int64 milliseconds since the Unix
//! epoch to match the rest of the civic_atlas v1 proto contract.
//! `0` is the wire representation of "unset"; the SQL columns are
//! nullable and we map NULL -> 0.

#![allow(clippy::result_large_err)]

use civic_atlas_types::civic_atlas::v1::{
    AuthClaimInviteRequest, AuthClaimInviteResponse, AuthResolveSessionRequest,
    AuthResolveSessionResponse, AuthRevokeSessionRequest, AuthRevokeSessionResponse,
};
use civic_atlas_types::event_planner::{
    BookmarkCreateRequest, BookmarkDeleteRequest, BookmarkListRequest, BookmarkListResponse,
    BookmarkMutationResponse, BookmarkUpdateRequest, CameraBookmark, EventLayer,
    EventLayerListRequest, EventLayerListResponse, EventPlannerService, IntakePendingVendorRequest,
    IntakePendingVendorResponse, Placement, PlacementCreateRequest, PlacementDeleteRequest,
    PlacementListRequest, PlacementListResponse, PlacementMutationResponse, PlacementNote,
    PlacementNoteCreateRequest, PlacementNoteDeleteRequest, PlacementNoteListRequest,
    PlacementNoteListResponse, PlacementNoteMutationResponse, PlacementUpdateRequest, Task,
    TaskCreateRequest, TaskDeleteRequest, TaskListRequest, TaskListResponse, TaskMutationResponse,
    TaskUpdateRequest,
};

use crate::event_planner_auth;
use sqlx::types::time::OffsetDateTime;
use sqlx::{postgres::PgRow, PgPool, Postgres, Row, Transaction};
use tenant_resolver::require_tenant_context;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::AtlasState;

#[derive(Clone)]
pub struct EventPlannerGrpcService {
    state: AtlasState,
}

impl EventPlannerGrpcService {
    pub fn new(state: AtlasState) -> Self {
        Self { state }
    }

    fn pool(&self) -> Result<&PgPool, Status> {
        self.state
            .db_pool()
            .ok_or_else(|| Status::unavailable("DATABASE_URL is required for EventPlannerService"))
    }
}

#[tonic::async_trait]
impl EventPlannerService for EventPlannerGrpcService {
    async fn list_event_layers(
        &self,
        request: Request<EventLayerListRequest>,
    ) -> Result<Response<EventLayerListResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        let rows = sqlx::query(
            r#"
            SELECT
                id,
                slug,
                title,
                starts_at,
                ends_at,
                CASE
                    WHEN bounds IS NULL THEN NULL
                    ELSE ST_AsGeoJSON(bounds::geometry)
                END AS bounds_geojson
            FROM event_layers
            WHERE tenant_id = $1
            ORDER BY starts_at ASC NULLS LAST, title ASC
            "#,
        )
        .bind(tenant_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        let layers = rows.iter().map(event_layer_from_row).collect();
        Ok(Response::new(EventLayerListResponse { layers }))
    }

    async fn list_placements(
        &self,
        request: Request<PlacementListRequest>,
    ) -> Result<Response<PlacementListResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let slug = require_slug(&request.event_layer_slug)?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        let rows = sqlx::query(
            r#"
            SELECT
                p.id,
                p.event_layer_id,
                p.category,
                p.sublabel,
                p.label,
                ST_AsGeoJSON(p.geometry::geometry) AS geometry_geojson,
                p.owner_user_id,
                p.status,
                p.notes,
                p.created_at,
                p.updated_at,
                p.version
            FROM event_placements p
            JOIN event_layers l
              ON l.id = p.event_layer_id AND l.tenant_id = p.tenant_id
            WHERE p.tenant_id = $1
              AND l.slug = $2
            ORDER BY p.category, p.label
            "#,
        )
        .bind(tenant_id)
        .bind(slug)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        let placements = rows.iter().map(placement_from_row).collect();
        Ok(Response::new(PlacementListResponse { placements }))
    }

    async fn list_tasks(
        &self,
        request: Request<TaskListRequest>,
    ) -> Result<Response<TaskListResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let slug = require_slug(&request.event_layer_slug)?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        let rows = sqlx::query(
            r#"
            SELECT
                t.id,
                t.event_layer_id,
                t.title,
                t.owner_display,
                t.due_at,
                t.status,
                t.placement_id,
                t.notes,
                t.created_at,
                t.updated_at,
                t.version
            FROM event_tasks t
            JOIN event_layers l
              ON l.id = t.event_layer_id AND l.tenant_id = t.tenant_id
            WHERE t.tenant_id = $1
              AND l.slug = $2
            ORDER BY
                CASE WHEN t.status = 'open' THEN 0 ELSE 1 END,
                t.due_at ASC NULLS LAST,
                t.created_at ASC
            "#,
        )
        .bind(tenant_id)
        .bind(slug)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        let tasks = rows.iter().map(task_from_row).collect();
        Ok(Response::new(TaskListResponse { tasks }))
    }

    /* --------------------------------------------------------------- */
    /*  Placement mutations                                            */
    /* --------------------------------------------------------------- */

    async fn create_placement(
        &self,
        request: Request<PlacementCreateRequest>,
    ) -> Result<Response<PlacementMutationResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        require_actor(&req.actor_user_id)?;
        let slug = require_slug(&req.event_layer_slug)?;
        let category = require_nonempty(&req.category, "category")?;
        let label = require_nonempty(&req.label, "label")?;
        let geometry = require_nonempty(&req.geometry_geojson, "geometry_geojson")?;
        let status_value = if req.status.trim().is_empty() {
            "placed"
        } else {
            req.status.trim()
        };
        let actor_uuid = parse_uuid(&req.actor_user_id, "actor_user_id")?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        let event_layer_id = resolve_event_layer_id(&mut tx, tenant_id, slug).await?;

        let row = sqlx::query(
            r#"
            WITH inserted AS (
              INSERT INTO event_placements (
                tenant_id, event_layer_id, category, sublabel, label,
                geometry, owner_user_id, status, notes
              )
              VALUES (
                $1, $2, $3, NULLIF($4, ''), $5,
                ST_GeomFromGeoJSON($6)::geography,
                $7, $8, NULLIF($9, '')
              )
              RETURNING *
            )
            SELECT
                id, event_layer_id, category, sublabel, label,
                ST_AsGeoJSON(geometry::geometry) AS geometry_geojson,
                owner_user_id, status, notes,
                created_at, updated_at, version
            FROM inserted
            "#,
        )
        .bind(tenant_id)
        .bind(event_layer_id)
        .bind(category)
        .bind(req.sublabel.trim())
        .bind(label)
        .bind(geometry)
        .bind(actor_uuid)
        .bind(status_value)
        .bind(req.notes.trim())
        .fetch_one(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(PlacementMutationResponse {
            placement: Some(placement_from_row(&row)),
            stale_write: false,
            deleted: false,
        }))
    }

    async fn update_placement(
        &self,
        request: Request<PlacementUpdateRequest>,
    ) -> Result<Response<PlacementMutationResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        require_actor(&req.actor_user_id)?;
        let placement_uuid = parse_uuid(&req.placement_id, "placement_id")?;
        if req.expected_version <= 0 {
            return Err(Status::invalid_argument(
                "expected_version must be > 0; pass the version the client last observed",
            ));
        }

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        // Build the SET clause dynamically from the *_present flags.
        // Using a CASE / COALESCE per column keeps the SQL static and
        // avoids string concatenation; the flags decide which side of
        // each CASE wins.
        let row = sqlx::query(
            r#"
            WITH updated AS (
              UPDATE event_placements SET
                category = CASE WHEN $4 THEN $5 ELSE category END,
                sublabel = CASE WHEN $6 THEN NULLIF($7, '') ELSE sublabel END,
                label = CASE WHEN $8 THEN $9 ELSE label END,
                geometry = CASE WHEN $10 THEN ST_GeomFromGeoJSON($11)::geography ELSE geometry END,
                status = CASE WHEN $12 THEN $13 ELSE status END,
                notes = CASE WHEN $14 THEN NULLIF($15, '') ELSE notes END
              WHERE id = $1
                AND tenant_id = $2
                AND version = $3
              RETURNING *
            )
            SELECT
                id, event_layer_id, category, sublabel, label,
                ST_AsGeoJSON(geometry::geometry) AS geometry_geojson,
                owner_user_id, status, notes,
                created_at, updated_at, version
            FROM updated
            "#,
        )
        .bind(placement_uuid)
        .bind(tenant_id)
        .bind(req.expected_version)
        .bind(req.category_present)
        .bind(&req.category)
        .bind(req.sublabel_present)
        .bind(&req.sublabel)
        .bind(req.label_present)
        .bind(&req.label)
        .bind(req.geometry_present)
        .bind(&req.geometry_geojson)
        .bind(req.status_present)
        .bind(&req.status)
        .bind(req.notes_present)
        .bind(&req.notes)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;

        if let Some(row) = row {
            tx.commit().await.map_err(db_status)?;
            return Ok(Response::new(PlacementMutationResponse {
                placement: Some(placement_from_row(&row)),
                stale_write: false,
                deleted: false,
            }));
        }

        // Zero rows updated: either the placement was deleted out from
        // under us, or another planner won the race. Return the current
        // server state with stale_write=true so the client can reconcile
        // without a second fetch.
        let current = sqlx::query(
            r#"
            SELECT
                id, event_layer_id, category, sublabel, label,
                ST_AsGeoJSON(geometry::geometry) AS geometry_geojson,
                owner_user_id, status, notes,
                created_at, updated_at, version
            FROM event_placements
            WHERE id = $1 AND tenant_id = $2
            "#,
        )
        .bind(placement_uuid)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(PlacementMutationResponse {
            placement: current.as_ref().map(placement_from_row),
            stale_write: true,
            deleted: current.is_none(),
        }))
    }

    async fn delete_placement(
        &self,
        request: Request<PlacementDeleteRequest>,
    ) -> Result<Response<PlacementMutationResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        require_actor(&req.actor_user_id)?;
        let placement_uuid = parse_uuid(&req.placement_id, "placement_id")?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        let deleted = sqlx::query(
            r#"
            DELETE FROM event_placements
            WHERE id = $1
              AND tenant_id = $2
              AND version = $3
            RETURNING id
            "#,
        )
        .bind(placement_uuid)
        .bind(tenant_id)
        .bind(req.expected_version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?
        .is_some();

        if deleted {
            tx.commit().await.map_err(db_status)?;
            return Ok(Response::new(PlacementMutationResponse {
                placement: None,
                stale_write: false,
                deleted: true,
            }));
        }

        // Same stale-write reconciliation pattern as update.
        let current = sqlx::query(
            r#"
            SELECT
                id, event_layer_id, category, sublabel, label,
                ST_AsGeoJSON(geometry::geometry) AS geometry_geojson,
                owner_user_id, status, notes,
                created_at, updated_at, version
            FROM event_placements
            WHERE id = $1 AND tenant_id = $2
            "#,
        )
        .bind(placement_uuid)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(PlacementMutationResponse {
            placement: current.as_ref().map(placement_from_row),
            stale_write: true,
            deleted: current.is_none(),
        }))
    }

    /* --------------------------------------------------------------- */
    /*  Task mutations                                                 */
    /* --------------------------------------------------------------- */

    async fn create_task(
        &self,
        request: Request<TaskCreateRequest>,
    ) -> Result<Response<TaskMutationResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        require_actor(&req.actor_user_id)?;
        let slug = require_slug(&req.event_layer_slug)?;
        let title = require_nonempty(&req.title, "title")?;
        let owner_uuid = parse_optional_uuid(&req.owner_user_id, "owner_user_id")?;
        let placement_uuid = parse_optional_uuid(&req.placement_id, "placement_id")?;
        let due_at = ms_to_offset_datetime(req.due_at_ms);
        let status_value = if req.status.trim().is_empty() {
            "open"
        } else {
            req.status.trim()
        };

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let event_layer_id = resolve_event_layer_id(&mut tx, tenant_id, slug).await?;
        let owner_display = resolve_display_name(&mut tx, owner_uuid).await?;

        let row = sqlx::query(
            r#"
            INSERT INTO event_tasks (
              tenant_id, event_layer_id, title, owner_user_id,
              owner_display, due_at, status, placement_id, notes
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULLIF($9, ''))
            RETURNING
              id, event_layer_id, title, owner_display, due_at,
              status, placement_id, notes, created_at, updated_at, version
            "#,
        )
        .bind(tenant_id)
        .bind(event_layer_id)
        .bind(title)
        .bind(owner_uuid)
        .bind(owner_display)
        .bind(due_at)
        .bind(status_value)
        .bind(placement_uuid)
        .bind(req.notes.trim())
        .fetch_one(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(TaskMutationResponse {
            task: Some(task_from_row(&row)),
            stale_write: false,
            deleted: false,
        }))
    }

    async fn update_task(
        &self,
        request: Request<TaskUpdateRequest>,
    ) -> Result<Response<TaskMutationResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        require_actor(&req.actor_user_id)?;
        let task_uuid = parse_uuid(&req.task_id, "task_id")?;
        if req.expected_version <= 0 {
            return Err(Status::invalid_argument("expected_version must be > 0"));
        }

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        // Owner change resolves to a display string too; do the
        // lookup before the UPDATE so we have a single place to error
        // out if the owner uuid doesn't resolve.
        let new_owner_uuid = if req.owner_present {
            parse_optional_uuid(&req.owner_user_id, "owner_user_id")?
        } else {
            None
        };
        let new_owner_display = if req.owner_present {
            resolve_display_name(&mut tx, new_owner_uuid).await?
        } else {
            None
        };
        let new_placement_uuid = if req.placement_present {
            parse_optional_uuid(&req.placement_id, "placement_id")?
        } else {
            None
        };
        let new_due_at = if req.due_at_present {
            ms_to_offset_datetime(req.due_at_ms)
        } else {
            None
        };

        let row = sqlx::query(
            r#"
            UPDATE event_tasks SET
              title = CASE WHEN $4 THEN $5 ELSE title END,
              owner_user_id = CASE WHEN $6 THEN $7 ELSE owner_user_id END,
              owner_display = CASE WHEN $6 THEN $8 ELSE owner_display END,
              due_at = CASE WHEN $9 THEN $10 ELSE due_at END,
              status = CASE WHEN $11 THEN $12 ELSE status END,
              placement_id = CASE WHEN $13 THEN $14 ELSE placement_id END,
              notes = CASE WHEN $15 THEN NULLIF($16, '') ELSE notes END
            WHERE id = $1
              AND tenant_id = $2
              AND version = $3
            RETURNING
              id, event_layer_id, title, owner_display, due_at,
              status, placement_id, notes, created_at, updated_at, version
            "#,
        )
        .bind(task_uuid)
        .bind(tenant_id)
        .bind(req.expected_version)
        .bind(req.title_present)
        .bind(&req.title)
        .bind(req.owner_present)
        .bind(new_owner_uuid)
        .bind(new_owner_display)
        .bind(req.due_at_present)
        .bind(new_due_at)
        .bind(req.status_present)
        .bind(&req.status)
        .bind(req.placement_present)
        .bind(new_placement_uuid)
        .bind(req.notes_present)
        .bind(&req.notes)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;

        if let Some(row) = row {
            tx.commit().await.map_err(db_status)?;
            return Ok(Response::new(TaskMutationResponse {
                task: Some(task_from_row(&row)),
                stale_write: false,
                deleted: false,
            }));
        }

        let current = sqlx::query(
            r#"
            SELECT
              id, event_layer_id, title, owner_display, due_at,
              status, placement_id, notes, created_at, updated_at, version
            FROM event_tasks
            WHERE id = $1 AND tenant_id = $2
            "#,
        )
        .bind(task_uuid)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(TaskMutationResponse {
            task: current.as_ref().map(task_from_row),
            stale_write: true,
            deleted: current.is_none(),
        }))
    }

    async fn delete_task(
        &self,
        request: Request<TaskDeleteRequest>,
    ) -> Result<Response<TaskMutationResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        require_actor(&req.actor_user_id)?;
        let task_uuid = parse_uuid(&req.task_id, "task_id")?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        let deleted = sqlx::query(
            r#"
            DELETE FROM event_tasks
            WHERE id = $1
              AND tenant_id = $2
              AND version = $3
            RETURNING id
            "#,
        )
        .bind(task_uuid)
        .bind(tenant_id)
        .bind(req.expected_version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?
        .is_some();

        if deleted {
            tx.commit().await.map_err(db_status)?;
            return Ok(Response::new(TaskMutationResponse {
                task: None,
                stale_write: false,
                deleted: true,
            }));
        }

        let current = sqlx::query(
            r#"
            SELECT
              id, event_layer_id, title, owner_display, due_at,
              status, placement_id, notes, created_at, updated_at, version
            FROM event_tasks
            WHERE id = $1 AND tenant_id = $2
            "#,
        )
        .bind(task_uuid)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(TaskMutationResponse {
            task: current.as_ref().map(task_from_row),
            stale_write: true,
            deleted: current.is_none(),
        }))
    }

    /* --------------------------------------------------------------- */
    /*  Auth                                                           */
    /* --------------------------------------------------------------- */

    async fn claim_invite(
        &self,
        request: Request<AuthClaimInviteRequest>,
    ) -> Result<Response<AuthClaimInviteResponse>, Status> {
        // Tenant context is required for the claim — the sidecar passes
        // the active tenant slug. The invite row itself carries the
        // tenant_id, which is what we ultimately use for the session.
        let req = request.into_inner();
        require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if req.token.trim().is_empty() {
            return Err(Status::invalid_argument("token is required"));
        }

        let pool = self.pool()?;
        let claimed = event_planner_auth::claim_invite(pool, &req.token).await?;

        let Some(claim) = claimed else {
            return Ok(Response::new(AuthClaimInviteResponse {
                success: false,
                user_id: String::new(),
                display_name: String::new(),
                email: String::new(),
                session_token: String::new(),
                error: "magic link is invalid, expired, or already used".into(),
            }));
        };

        // Resolve the email for the response. claim_invite returns the
        // user uuid + display name; one more lookup is cheaper than
        // expanding the ClaimedInvite shape and worth it for the UI.
        let email = sqlx::query("SELECT email FROM event_planner_users WHERE id = $1")
            .bind(claim.user_id)
            .fetch_optional(pool)
            .await
            .map_err(db_status)?
            .and_then(|row| row.try_get::<String, _>("email").ok())
            .unwrap_or_default();

        Ok(Response::new(AuthClaimInviteResponse {
            success: true,
            user_id: claim.user_id.to_string(),
            display_name: claim.display_name,
            email,
            session_token: claim.session_token,
            error: String::new(),
        }))
    }

    async fn resolve_session(
        &self,
        request: Request<AuthResolveSessionRequest>,
    ) -> Result<Response<AuthResolveSessionResponse>, Status> {
        let req = request.into_inner();
        require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;

        let pool = self.pool()?;
        let resolved = event_planner_auth::resolve_session(pool, &req.session_token).await?;

        let Some(session) = resolved else {
            return Ok(Response::new(AuthResolveSessionResponse {
                authenticated: false,
                user_id: String::new(),
                display_name: String::new(),
                email: String::new(),
            }));
        };

        Ok(Response::new(AuthResolveSessionResponse {
            authenticated: true,
            user_id: session.user_id.to_string(),
            display_name: session.display_name,
            email: session.email,
        }))
    }

    async fn revoke_session(
        &self,
        request: Request<AuthRevokeSessionRequest>,
    ) -> Result<Response<AuthRevokeSessionResponse>, Status> {
        let req = request.into_inner();
        require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let pool = self.pool()?;
        event_planner_auth::revoke_session(pool, &req.session_token).await?;
        Ok(Response::new(AuthRevokeSessionResponse { revoked: true }))
    }

    /* --------------------------------------------------------------- */
    /*  Phase 3: notes                                                 */
    /* --------------------------------------------------------------- */

    async fn list_placement_notes(
        &self,
        request: Request<PlacementNoteListRequest>,
    ) -> Result<Response<PlacementNoteListResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let placement_uuid = parse_uuid(&req.placement_id, "placement_id")?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        let rows = sqlx::query(
            r#"
            SELECT
                n.id, n.placement_id, n.event_layer_id,
                n.author_user_id, u.display_name AS author_display,
                n.body, n.created_at, n.updated_at, n.version
            FROM event_placement_notes n
            JOIN event_planner_users u ON u.id = n.author_user_id
            WHERE n.tenant_id = $1 AND n.placement_id = $2
            ORDER BY n.created_at ASC
            "#,
        )
        .bind(tenant_id)
        .bind(placement_uuid)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;
        let notes = rows.iter().map(note_from_row).collect();
        Ok(Response::new(PlacementNoteListResponse { notes }))
    }

    async fn create_placement_note(
        &self,
        request: Request<PlacementNoteCreateRequest>,
    ) -> Result<Response<PlacementNoteMutationResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        require_actor(&req.actor_user_id)?;
        let placement_uuid = parse_uuid(&req.placement_id, "placement_id")?;
        let actor_uuid = parse_uuid(&req.actor_user_id, "actor_user_id")?;
        let body = require_nonempty(&req.body, "body")?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        // Copy the parent placement's event_layer_id onto the note
        // row. This denormalization (per migration 0015 comments)
        // lets the SSE consumer route notifications by event slug
        // without joining through placements.
        let row = sqlx::query(
            r#"
            WITH parent AS (
                SELECT event_layer_id
                FROM event_placements
                WHERE id = $2 AND tenant_id = $1
            ),
            inserted AS (
                INSERT INTO event_placement_notes (
                    tenant_id, placement_id, event_layer_id,
                    author_user_id, body
                )
                SELECT $1, $2, parent.event_layer_id, $3, $4
                FROM parent
                RETURNING *
            )
            SELECT
                inserted.id, inserted.placement_id, inserted.event_layer_id,
                inserted.author_user_id, u.display_name AS author_display,
                inserted.body, inserted.created_at, inserted.updated_at,
                inserted.version
            FROM inserted
            JOIN event_planner_users u ON u.id = inserted.author_user_id
            "#,
        )
        .bind(tenant_id)
        .bind(placement_uuid)
        .bind(actor_uuid)
        .bind(body)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;

        let Some(row) = row else {
            return Err(Status::not_found(
                "placement not found in this tenant — note refused",
            ));
        };

        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(PlacementNoteMutationResponse {
            note: Some(note_from_row(&row)),
            deleted: false,
        }))
    }

    async fn delete_placement_note(
        &self,
        request: Request<PlacementNoteDeleteRequest>,
    ) -> Result<Response<PlacementNoteMutationResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        require_actor(&req.actor_user_id)?;
        let note_uuid = parse_uuid(&req.note_id, "note_id")?;
        let actor_uuid = parse_uuid(&req.actor_user_id, "actor_user_id")?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        // Only the original author can delete their own note. The
        // spec calls notes append-only for Phase 3; deletion is the
        // self-service "I posted that wrong" escape hatch, not a
        // moderation tool.
        let deleted = sqlx::query(
            r#"
            DELETE FROM event_placement_notes
            WHERE id = $1 AND tenant_id = $2 AND author_user_id = $3
            RETURNING id
            "#,
        )
        .bind(note_uuid)
        .bind(tenant_id)
        .bind(actor_uuid)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?
        .is_some();

        tx.commit().await.map_err(db_status)?;

        if !deleted {
            return Err(Status::permission_denied(
                "you can only delete notes you authored",
            ));
        }

        Ok(Response::new(PlacementNoteMutationResponse {
            note: None,
            deleted: true,
        }))
    }

    /* --------------------------------------------------------------- */
    /*  Phase 3: camera bookmarks                                      */
    /* --------------------------------------------------------------- */

    async fn list_bookmarks(
        &self,
        request: Request<BookmarkListRequest>,
    ) -> Result<Response<BookmarkListResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let slug = require_slug(&req.event_layer_slug)?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let event_layer_id = resolve_event_layer_id(&mut tx, tenant_id, slug).await?;

        let rows = sqlx::query(
            r#"
            SELECT
                id, event_layer_id, name, center_lng, center_lat,
                zoom, pitch, bearing, created_by,
                created_at, updated_at, version
            FROM event_planner_bookmarks
            WHERE tenant_id = $1 AND event_layer_id = $2
            ORDER BY created_at DESC
            "#,
        )
        .bind(tenant_id)
        .bind(event_layer_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;
        let bookmarks = rows.iter().map(bookmark_from_row).collect();
        Ok(Response::new(BookmarkListResponse { bookmarks }))
    }

    async fn create_bookmark(
        &self,
        request: Request<BookmarkCreateRequest>,
    ) -> Result<Response<BookmarkMutationResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        require_actor(&req.actor_user_id)?;
        let slug = require_slug(&req.event_layer_slug)?;
        let name = require_nonempty(&req.name, "name")?;
        let actor_uuid = parse_uuid(&req.actor_user_id, "actor_user_id")?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let event_layer_id = resolve_event_layer_id(&mut tx, tenant_id, slug).await?;

        let row = sqlx::query(
            r#"
            INSERT INTO event_planner_bookmarks (
              tenant_id, event_layer_id, name,
              center_lng, center_lat, zoom, pitch, bearing,
              created_by
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING
              id, event_layer_id, name, center_lng, center_lat,
              zoom, pitch, bearing, created_by,
              created_at, updated_at, version
            "#,
        )
        .bind(tenant_id)
        .bind(event_layer_id)
        .bind(name)
        .bind(req.center_lng)
        .bind(req.center_lat)
        .bind(req.zoom)
        .bind(req.pitch)
        .bind(req.bearing)
        .bind(actor_uuid)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(BookmarkMutationResponse {
            bookmark: Some(bookmark_from_row(&row)),
            stale_write: false,
            deleted: false,
        }))
    }

    async fn update_bookmark(
        &self,
        request: Request<BookmarkUpdateRequest>,
    ) -> Result<Response<BookmarkMutationResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        require_actor(&req.actor_user_id)?;
        let bookmark_uuid = parse_uuid(&req.bookmark_id, "bookmark_id")?;
        if req.expected_version <= 0 {
            return Err(Status::invalid_argument("expected_version must be > 0"));
        }

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        let row = sqlx::query(
            r#"
            UPDATE event_planner_bookmarks SET
              name = CASE WHEN $4 THEN $5 ELSE name END,
              center_lng = CASE WHEN $6 THEN $7 ELSE center_lng END,
              center_lat = CASE WHEN $8 THEN $9 ELSE center_lat END,
              zoom = CASE WHEN $10 THEN $11 ELSE zoom END,
              pitch = CASE WHEN $12 THEN $13 ELSE pitch END,
              bearing = CASE WHEN $14 THEN $15 ELSE bearing END
            WHERE id = $1
              AND tenant_id = $2
              AND version = $3
            RETURNING
              id, event_layer_id, name, center_lng, center_lat,
              zoom, pitch, bearing, created_by,
              created_at, updated_at, version
            "#,
        )
        .bind(bookmark_uuid)
        .bind(tenant_id)
        .bind(req.expected_version)
        .bind(req.name_present)
        .bind(&req.name)
        .bind(req.center_lng_present)
        .bind(req.center_lng)
        .bind(req.center_lat_present)
        .bind(req.center_lat)
        .bind(req.zoom_present)
        .bind(req.zoom)
        .bind(req.pitch_present)
        .bind(req.pitch)
        .bind(req.bearing_present)
        .bind(req.bearing)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;

        if let Some(row) = row {
            tx.commit().await.map_err(db_status)?;
            return Ok(Response::new(BookmarkMutationResponse {
                bookmark: Some(bookmark_from_row(&row)),
                stale_write: false,
                deleted: false,
            }));
        }

        let current = sqlx::query(
            r#"
            SELECT id, event_layer_id, name, center_lng, center_lat,
                   zoom, pitch, bearing, created_by,
                   created_at, updated_at, version
            FROM event_planner_bookmarks
            WHERE id = $1 AND tenant_id = $2
            "#,
        )
        .bind(bookmark_uuid)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(BookmarkMutationResponse {
            bookmark: current.as_ref().map(bookmark_from_row),
            stale_write: true,
            deleted: current.is_none(),
        }))
    }

    async fn delete_bookmark(
        &self,
        request: Request<BookmarkDeleteRequest>,
    ) -> Result<Response<BookmarkMutationResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        require_actor(&req.actor_user_id)?;
        let bookmark_uuid = parse_uuid(&req.bookmark_id, "bookmark_id")?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;

        let deleted = sqlx::query(
            r#"
            DELETE FROM event_planner_bookmarks
            WHERE id = $1 AND tenant_id = $2 AND version = $3
            RETURNING id
            "#,
        )
        .bind(bookmark_uuid)
        .bind(tenant_id)
        .bind(req.expected_version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?
        .is_some();

        if deleted {
            tx.commit().await.map_err(db_status)?;
            return Ok(Response::new(BookmarkMutationResponse {
                bookmark: None,
                stale_write: false,
                deleted: true,
            }));
        }

        let current = sqlx::query(
            r#"
            SELECT id, event_layer_id, name, center_lng, center_lat,
                   zoom, pitch, bearing, created_by,
                   created_at, updated_at, version
            FROM event_planner_bookmarks
            WHERE id = $1 AND tenant_id = $2
            "#,
        )
        .bind(bookmark_uuid)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;

        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(BookmarkMutationResponse {
            bookmark: current.as_ref().map(bookmark_from_row),
            stale_write: true,
            deleted: current.is_none(),
        }))
    }

    /* --------------------------------------------------------------- */
    /*  Phase 3: Stripe-driven vendor intake                           */
    /* --------------------------------------------------------------- */

    async fn intake_pending_vendor(
        &self,
        request: Request<IntakePendingVendorRequest>,
    ) -> Result<Response<IntakePendingVendorResponse>, Status> {
        let req = request.into_inner();
        let tenant = require_tenant_context(req.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let slug = require_slug(&req.event_layer_slug)?;
        let business_name = require_nonempty(&req.business_name, "business_name")?;
        let idempotency_key = require_nonempty(&req.idempotency_key, "idempotency_key")?;

        let sublabel = match req.vendor_tier.trim().to_lowercase().as_str() {
            "food_truck" | "food-truck" | "food truck" => "Food Truck".to_string(),
            "pop_up" | "pop-up" | "popup" | "pop up" => "Pop-up".to_string(),
            other if !other.is_empty() => req.vendor_tier.trim().to_string(),
            _ => String::new(),
        };

        // Compose the notes payload deterministically so a webhook
        // retry yields the same content and idempotency check passes.
        let notes_payload = format!(
            "[stripe] {idempotency_key}\ncontact: {contact} <{email}>\nneeds: {needs}",
            contact = req.contact_name.trim(),
            email = req.contact_email.trim(),
            needs = req.needs.trim(),
        );

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let event_layer_id = resolve_event_layer_id(&mut tx, tenant_id, slug).await?;

        // Idempotency: look for a prior placement whose notes carry
        // the same `[stripe] <idempotency_key>` line. The check is a
        // plain LIKE — the key format is opaque to the DB and we
        // don't want a separate uniqueness index just for this hot
        // path.
        let dedupe_marker = format!("[stripe] {idempotency_key}");
        let existing = sqlx::query(
            r#"
            SELECT id FROM event_placements
            WHERE tenant_id = $1
              AND event_layer_id = $2
              AND notes IS NOT NULL
              AND notes LIKE $3
            LIMIT 1
            "#,
        )
        .bind(tenant_id)
        .bind(event_layer_id)
        .bind(format!("%{dedupe_marker}%"))
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;

        if let Some(row) = existing {
            let id: Uuid = row.get("id");
            tx.commit().await.map_err(db_status)?;
            return Ok(Response::new(IntakePendingVendorResponse {
                created: false,
                placement_id: id.to_string(),
            }));
        }

        // Compose a Point geometry from the default lng/lat the
        // CTHNA side chose. Validate bounds defensively so a
        // misconfigured Stripe handler can't drop pins in the ocean.
        if !(-180.0..=180.0).contains(&req.default_lng)
            || !(-90.0..=90.0).contains(&req.default_lat)
        {
            return Err(Status::invalid_argument(
                "default_lng / default_lat out of WGS84 range",
            ));
        }
        let geometry = serde_json::json!({
            "type": "Point",
            "coordinates": [req.default_lng, req.default_lat],
        })
        .to_string();

        let row = sqlx::query(
            r#"
            INSERT INTO event_placements (
                tenant_id, event_layer_id, category, sublabel, label,
                geometry, status, notes
            )
            VALUES (
                $1, $2, 'vendor', NULLIF($3, ''), $4,
                ST_GeomFromGeoJSON($5)::geography,
                'pending_placement', $6
            )
            RETURNING id
            "#,
        )
        .bind(tenant_id)
        .bind(event_layer_id)
        .bind(&sublabel)
        .bind(business_name)
        .bind(&geometry)
        .bind(&notes_payload)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_status)?;

        let placement_id: Uuid = row.get("id");
        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(IntakePendingVendorResponse {
            created: true,
            placement_id: placement_id.to_string(),
        }))
    }
}

/* ------------------------------------------------------------------ */
/*  Row mappers                                                        */
/* ------------------------------------------------------------------ */

fn event_layer_from_row(row: &PgRow) -> EventLayer {
    EventLayer {
        id: row.get::<Uuid, _>("id").to_string(),
        slug: row.get::<String, _>("slug"),
        title: row.get::<String, _>("title"),
        starts_at_ms: ts_ms(row, "starts_at"),
        ends_at_ms: ts_ms(row, "ends_at"),
        bounds_geojson: row
            .try_get::<Option<String>, _>("bounds_geojson")
            .ok()
            .flatten()
            .unwrap_or_default(),
    }
}

fn placement_from_row(row: &PgRow) -> Placement {
    Placement {
        id: row.get::<Uuid, _>("id").to_string(),
        event_layer_id: row.get::<Uuid, _>("event_layer_id").to_string(),
        category: row.get::<String, _>("category"),
        sublabel: row
            .try_get::<Option<String>, _>("sublabel")
            .ok()
            .flatten()
            .unwrap_or_default(),
        label: row.get::<String, _>("label"),
        geometry_geojson: row.get::<String, _>("geometry_geojson"),
        owner_user_id: row
            .try_get::<Option<Uuid>, _>("owner_user_id")
            .ok()
            .flatten()
            .map(|id| id.to_string())
            .unwrap_or_default(),
        status: row.get::<String, _>("status"),
        notes: row
            .try_get::<Option<String>, _>("notes")
            .ok()
            .flatten()
            .unwrap_or_default(),
        created_at_ms: ts_ms(row, "created_at"),
        updated_at_ms: ts_ms(row, "updated_at"),
        version: row.try_get::<i64, _>("version").unwrap_or(1),
    }
}

fn task_from_row(row: &PgRow) -> Task {
    Task {
        id: row.get::<Uuid, _>("id").to_string(),
        event_layer_id: row.get::<Uuid, _>("event_layer_id").to_string(),
        title: row.get::<String, _>("title"),
        owner_display: row
            .try_get::<Option<String>, _>("owner_display")
            .ok()
            .flatten()
            .unwrap_or_default(),
        due_at_ms: ts_ms(row, "due_at"),
        status: row.get::<String, _>("status"),
        placement_id: row
            .try_get::<Option<Uuid>, _>("placement_id")
            .ok()
            .flatten()
            .map(|id| id.to_string())
            .unwrap_or_default(),
        notes: row
            .try_get::<Option<String>, _>("notes")
            .ok()
            .flatten()
            .unwrap_or_default(),
        created_at_ms: ts_ms(row, "created_at"),
        updated_at_ms: ts_ms(row, "updated_at"),
        version: row.try_get::<i64, _>("version").unwrap_or(1),
    }
}

fn note_from_row(row: &PgRow) -> PlacementNote {
    PlacementNote {
        id: row.get::<Uuid, _>("id").to_string(),
        placement_id: row.get::<Uuid, _>("placement_id").to_string(),
        event_layer_id: row.get::<Uuid, _>("event_layer_id").to_string(),
        author_user_id: row.get::<Uuid, _>("author_user_id").to_string(),
        author_display: row
            .try_get::<String, _>("author_display")
            .unwrap_or_default(),
        body: row.get::<String, _>("body"),
        created_at_ms: ts_ms(row, "created_at"),
        updated_at_ms: ts_ms(row, "updated_at"),
        version: row.try_get::<i64, _>("version").unwrap_or(1),
    }
}

fn bookmark_from_row(row: &PgRow) -> CameraBookmark {
    CameraBookmark {
        id: row.get::<Uuid, _>("id").to_string(),
        event_layer_id: row.get::<Uuid, _>("event_layer_id").to_string(),
        name: row.get::<String, _>("name"),
        center_lng: row.get::<f64, _>("center_lng"),
        center_lat: row.get::<f64, _>("center_lat"),
        zoom: row.get::<f64, _>("zoom"),
        pitch: row.get::<f64, _>("pitch"),
        bearing: row.get::<f64, _>("bearing"),
        created_by_user_id: row
            .try_get::<Option<Uuid>, _>("created_by")
            .ok()
            .flatten()
            .map(|id| id.to_string())
            .unwrap_or_default(),
        created_at_ms: ts_ms(row, "created_at"),
        updated_at_ms: ts_ms(row, "updated_at"),
        version: row.try_get::<i64, _>("version").unwrap_or(1),
    }
}

/// Decode a timestamptz column into Unix epoch milliseconds. Returns
/// `0` for NULL or any decode failure — the wire contract treats `0`
/// as "unset". Handles both Option<T> and T column types so the helper
/// works for the nullable schema columns (starts_at, due_at) and the
/// NOT-NULL ones (created_at, updated_at) with the same call site.
fn ts_ms(row: &PgRow, column: &str) -> i64 {
    if let Ok(Some(ts)) = row.try_get::<Option<OffsetDateTime>, _>(column) {
        return ts.unix_timestamp() * 1_000 + i64::from(ts.millisecond());
    }
    if let Ok(ts) = row.try_get::<OffsetDateTime, _>(column) {
        return ts.unix_timestamp() * 1_000 + i64::from(ts.millisecond());
    }
    0
}

/* ------------------------------------------------------------------ */
/*  Helpers                                                            */
/* ------------------------------------------------------------------ */

fn db_status(error: sqlx::Error) -> Status {
    Status::internal(format!("database error: {error}"))
}

fn require_slug(slug: &str) -> Result<&str, Status> {
    let trimmed = slug.trim();
    if trimmed.is_empty() {
        Err(Status::invalid_argument("event_layer_slug is required"))
    } else {
        Ok(trimmed)
    }
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

async fn set_transaction_tenant(
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

/* ------------------------------------------------------------------ */
/*  Phase 2 mutation helpers                                          */
/* ------------------------------------------------------------------ */

/// Look up the uuid of the event_layer with the given slug, scoped
/// to the active tenant. Returns 404 if missing.
async fn resolve_event_layer_id(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    slug: &str,
) -> Result<Uuid, Status> {
    let row = sqlx::query(r#"SELECT id FROM event_layers WHERE tenant_id = $1 AND slug = $2"#)
        .bind(tenant_id)
        .bind(slug)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_status)?;
    row.and_then(|r| r.try_get::<Uuid, _>("id").ok())
        .ok_or_else(|| Status::not_found(format!("event layer not found: {slug}")))
}

/// Look up an event_planner_users.display_name. Returns Ok(None) when
/// the input is None (owner cleared); errors when the uuid is provided
/// but doesn't resolve.
async fn resolve_display_name(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Option<Uuid>,
) -> Result<Option<String>, Status> {
    let Some(uuid) = user_id else {
        return Ok(None);
    };
    let row = sqlx::query("SELECT display_name FROM event_planner_users WHERE id = $1")
        .bind(uuid)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_status)?;
    let name = row
        .and_then(|r| r.try_get::<String, _>("display_name").ok())
        .ok_or_else(|| Status::not_found(format!("planner user not found: {uuid}")))?;
    Ok(Some(name))
}

/// Mutations require a planner session. The GraphQL sidecar resolves
/// the session cookie to the user uuid before calling the RPC; an
/// empty `actor_user_id` therefore means unauthenticated.
fn require_actor(actor_user_id: &str) -> Result<(), Status> {
    if actor_user_id.trim().is_empty() {
        Err(Status::unauthenticated(
            "this mutation requires a signed-in planner",
        ))
    } else {
        Ok(())
    }
}

fn require_nonempty<'a>(value: &'a str, field: &str) -> Result<&'a str, Status> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(Status::invalid_argument(format!("{field} is required")))
    } else {
        Ok(trimmed)
    }
}

fn parse_uuid(value: &str, field: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(value.trim())
        .map_err(|_| Status::invalid_argument(format!("{field} must be a UUID")))
}

fn parse_optional_uuid(value: &str, field: &str) -> Result<Option<Uuid>, Status> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Uuid::parse_str(trimmed)
        .map(Some)
        .map_err(|_| Status::invalid_argument(format!("{field} must be a UUID or empty")))
}

fn ms_to_offset_datetime(ms: i64) -> Option<OffsetDateTime> {
    if ms <= 0 {
        return None;
    }
    OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000).ok()
}
