#![allow(clippy::result_large_err)]

use civic_atlas_types::civic_atlas::v1::reconstruction_service_server::ReconstructionService as ReconstructionGrpc;
use civic_atlas_types::civic_atlas::v1::{
    ApproveSpecRequest, ApproveSpecResponse, Facade, GetReconstructionSpecRequest,
    GetReconstructionSpecResponse, GroundFloor, ListAssetsForSpecRequest,
    ListAssetsForSpecResponse, ListReconstructionSpecsRequest, ListReconstructionSpecsResponse,
    Mass, OpeningGrid, Ornament, PartProvenance, ReconstructionAsset, ReconstructionSource,
    ReconstructionSpec, ReconstructionSpecStatus, Roof, SaveDraftSpecRequest,
    SaveDraftSpecResponse, SubmitSpecForReviewRequest, SubmitSpecForReviewResponse, TenantContext,
};
use serde_json::{json, Value};
use sqlx::{postgres::PgRow, types::Json, PgPool, Postgres, Row, Transaction};
use tenant_resolver::require_tenant_context;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::AtlasState;

#[derive(Clone)]
pub struct ReconstructionGrpcService {
    state: AtlasState,
}

impl ReconstructionGrpcService {
    pub fn new(state: AtlasState) -> Self {
        Self { state }
    }

    fn pool(&self) -> Result<&PgPool, Status> {
        self.state.db_pool().ok_or_else(|| {
            Status::unavailable("DATABASE_URL is required for ReconstructionService")
        })
    }
}

#[tonic::async_trait]
impl ReconstructionGrpc for ReconstructionGrpcService {
    async fn get_reconstruction_spec(
        &self,
        request: Request<GetReconstructionSpecRequest>,
    ) -> Result<Response<GetReconstructionSpecResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.spec_id.trim().is_empty() {
            return Err(Status::invalid_argument("spec_id is required"));
        }

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;
        let row = sqlx::query(
            r#"
            SELECT spec_jsonb, status, version, reviewed_by
            FROM reconstruction_specs
            WHERE tenant_id = $1 AND spec_id = $2
            ORDER BY version DESC
            LIMIT 1
            "#,
        )
        .bind(tenant_id)
        .bind(&request.spec_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_status)?;
        tx.commit().await.map_err(db_status)?;

        let row = row.ok_or_else(|| Status::not_found("reconstruction spec not found"))?;
        Ok(Response::new(GetReconstructionSpecResponse {
            spec: Some(spec_from_row(&row)?),
        }))
    }

    async fn list_reconstruction_specs(
        &self,
        request: Request<ListReconstructionSpecsRequest>,
    ) -> Result<Response<ListReconstructionSpecsResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;
        let status = status_filter(request.status)?;
        let page_size = request.page_size.clamp(1, 100) as i64;
        let rows = sqlx::query(
            r#"
            SELECT spec_jsonb, status, version, reviewed_by
            FROM reconstruction_specs
            WHERE tenant_id = $1
              AND ($2 = '' OR civic_object_id = $2)
              AND ($3 = '' OR status = $3)
            ORDER BY spec_id, version DESC
            LIMIT $4
            "#,
        )
        .bind(tenant_id)
        .bind(request.civic_object_id.trim())
        .bind(status.unwrap_or(""))
        .bind(page_size)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_status)?;
        tx.commit().await.map_err(db_status)?;

        let specs = rows
            .iter()
            .map(spec_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Response::new(ListReconstructionSpecsResponse {
            specs,
            next_page_token: String::new(),
        }))
    }

    async fn save_draft_spec(
        &self,
        request: Request<SaveDraftSpecRequest>,
    ) -> Result<Response<SaveDraftSpecResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        let mut spec = request
            .spec
            .ok_or_else(|| Status::invalid_argument("spec is required"))?;
        validate_spec_identity(&spec)?;
        spec.status = ReconstructionSpecStatus::Draft as i32;
        spec.tenant_context = Some(TenantContext {
            tenant_id: tenant.as_str().to_string(),
            atlas_node_id: format!("atlas:{}", tenant.as_str()),
            metadata: Default::default(),
        });

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;
        upsert_spec(&mut tx, tenant_id, &spec, "draft", &spec.created_by).await?;
        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(SaveDraftSpecResponse { spec: Some(spec) }))
    }

    async fn submit_spec_for_review(
        &self,
        request: Request<SubmitSpecForReviewRequest>,
    ) -> Result<Response<SubmitSpecForReviewResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.spec_id.trim().is_empty() {
            return Err(Status::invalid_argument("spec_id is required"));
        }

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;
        let mut spec = fetch_latest_spec(&mut tx, tenant_id, &request.spec_id).await?;
        spec.status = ReconstructionSpecStatus::InReview as i32;
        spec.created_by = request.submitted_by;
        upsert_spec(&mut tx, tenant_id, &spec, "in_review", &spec.created_by).await?;
        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(SubmitSpecForReviewResponse {
            spec: Some(spec),
        }))
    }

    async fn approve_spec(
        &self,
        request: Request<ApproveSpecRequest>,
    ) -> Result<Response<ApproveSpecResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.spec_id.trim().is_empty() {
            return Err(Status::invalid_argument("spec_id is required"));
        }
        if request.version == 0 {
            return Err(Status::invalid_argument("version is required"));
        }

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;
        let mut spec = fetch_spec(&mut tx, tenant_id, &request.spec_id, request.version).await?;
        let building_id = parse_required_uuid(&spec.building_id, "building_id")?;

        for part in project_building_parts(&spec) {
            upsert_building_part(&mut tx, tenant_id, building_id, &part).await?;
        }

        // Geographic claim triangulation. Runs cheap point-in-polygon
        // checks against the tenant bbox + named district (parsed
        // from spec.block_id). Any disagreements get appended to
        // `place_provenance_disputes` for moderator / ACC/ACT review.
        // These are advisory: the approval proceeds either way.
        let block_id_opt = if spec.block_id.trim().is_empty() {
            None
        } else {
            Some(spec.block_id.as_str())
        };
        let disputes = crate::validation::validate_building_against_spec(
            &mut tx,
            tenant_id,
            building_id,
            &spec.spec_id,
            block_id_opt,
        )
        .await?;
        if !disputes.is_empty() {
            tracing::warn!(
                spec_id = %spec.spec_id,
                building_id = %building_id,
                dispute_count = disputes.len(),
                "geographic claim disagreement detected on spec approval"
            );
            crate::validation::record_disputes(&mut tx, tenant_id, &disputes).await?;
        }

        spec.status = ReconstructionSpecStatus::Approved as i32;
        spec.reviewed_by = request.approved_by;
        let update = sqlx::query(
            r#"
            UPDATE reconstruction_specs
            SET status = 'approved',
                reviewed_by = $4,
                approved_at = now(),
                updated_at = now(),
                spec_jsonb = $5
            WHERE tenant_id = $1 AND spec_id = $2 AND version = $3
              AND status IN ('draft', 'in_review')
            "#,
        )
        .bind(tenant_id)
        .bind(&spec.spec_id)
        .bind(spec.version as i32)
        .bind(&spec.reviewed_by)
        .bind(Json(spec_to_json(&spec)))
        .execute(&mut *tx)
        .await
        .map_err(db_status)?;
        if update.rows_affected() == 0 {
            return Err(Status::failed_precondition(
                "only draft or in-review specs can be approved",
            ));
        }

        enqueue_rustyred_projection(&mut tx, tenant_id, &spec).await?;
        tx.commit().await.map_err(db_status)?;

        Ok(Response::new(ApproveSpecResponse { spec: Some(spec) }))
    }

    async fn list_assets_for_spec(
        &self,
        request: Request<ListAssetsForSpecRequest>,
    ) -> Result<Response<ListAssetsForSpecResponse>, Status> {
        let request = request.into_inner();
        let tenant = require_tenant_context(request.tenant_context.as_ref())
            .map_err(|err| Status::unauthenticated(err.to_string()))?;
        if request.spec_id.trim().is_empty() {
            return Err(Status::invalid_argument("spec_id is required"));
        }

        let pool = self.pool()?;
        let mut tx = pool.begin().await.map_err(db_status)?;
        let tenant_id = resolve_tenant_id(&mut tx, tenant.as_str()).await?;
        set_transaction_tenant_uuid(&mut tx, tenant_id).await?;
        let page_size = request.page_size.clamp(1, 100) as i64;
        let rows = sqlx::query(
            r#"
            SELECT asset_id, spec_id, spec_version, asset_type, uri, content_hash, metadata_jsonb
            FROM generated_assets
            WHERE tenant_id = $1
              AND spec_id = $2
              AND ($3 = 0 OR spec_version = $3)
            ORDER BY created_at DESC
            LIMIT $4
            "#,
        )
        .bind(tenant_id)
        .bind(&request.spec_id)
        .bind(request.version as i32)
        .bind(page_size)
        .fetch_all(&mut *tx)
        .await
        .map_err(db_status)?;
        tx.commit().await.map_err(db_status)?;

        let assets = rows
            .iter()
            .map(|row| asset_from_row(row, tenant.as_str()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Response::new(ListAssetsForSpecResponse {
            assets,
            next_page_token: String::new(),
        }))
    }
}

async fn resolve_tenant_id(
    tx: &mut Transaction<'_, Postgres>,
    tenant_key: &str,
) -> Result<Uuid, Status> {
    let tenant_id: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id
        FROM tenants
        WHERE slug = $1 OR id::text = $1
        "#,
    )
    .bind(tenant_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_status)?;
    tenant_id.ok_or_else(|| Status::not_found("tenant not found"))
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

async fn upsert_spec(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    spec: &ReconstructionSpec,
    status: &str,
    created_by: &str,
) -> Result<(), Status> {
    let building_id = optional_uuid(&spec.building_id, "building_id")?;
    let parcel_id = optional_uuid(&spec.parcel_id, "parcel_id")?;
    let result = sqlx::query(
        r#"
        INSERT INTO reconstruction_specs (
          tenant_id, spec_id, version, status, building_id, parcel_id,
          civic_object_id, block_id, title, supersedes_spec_id, spec_jsonb,
          created_by, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, NULLIF($8, ''), $9, NULLIF($10, ''), $11, $12, now())
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
            updated_at = now()
        WHERE reconstruction_specs.status <> 'approved'
        "#,
    )
    .bind(tenant_id)
    .bind(&spec.spec_id)
    .bind(spec.version as i32)
    .bind(status)
    .bind(building_id)
    .bind(parcel_id)
    .bind(&spec.civic_object_id)
    .bind(&spec.block_id)
    .bind(&spec.title)
    .bind(&spec.supersedes_spec_id)
    .bind(Json(spec_to_json(spec)))
    .bind(created_by)
    .execute(&mut **tx)
    .await
    .map_err(db_status)?;
    if result.rows_affected() == 0 {
        return Err(Status::failed_precondition(
            "approved reconstruction specs are immutable",
        ));
    }
    Ok(())
}

async fn fetch_latest_spec(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    spec_id: &str,
) -> Result<ReconstructionSpec, Status> {
    let row = sqlx::query(
        r#"
        SELECT spec_jsonb, status, version, reviewed_by
        FROM reconstruction_specs
        WHERE tenant_id = $1 AND spec_id = $2
        ORDER BY version DESC
        LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .bind(spec_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_status)?;
    row.as_ref()
        .map(spec_from_row)
        .transpose()?
        .ok_or_else(|| Status::not_found("reconstruction spec not found"))
}

async fn fetch_spec(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    spec_id: &str,
    version: u32,
) -> Result<ReconstructionSpec, Status> {
    let row = sqlx::query(
        r#"
        SELECT spec_jsonb, status, version, reviewed_by
        FROM reconstruction_specs
        WHERE tenant_id = $1 AND spec_id = $2 AND version = $3
        "#,
    )
    .bind(tenant_id)
    .bind(spec_id)
    .bind(version as i32)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_status)?;
    row.as_ref()
        .map(spec_from_row)
        .transpose()?
        .ok_or_else(|| Status::not_found("reconstruction spec not found"))
}

#[derive(Debug)]
struct PartProjection {
    key: String,
    part_type: String,
    payload: Value,
    confidence: f64,
    source_ids: Vec<String>,
}

async fn upsert_building_part(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    building_id: Uuid,
    part: &PartProjection,
) -> Result<(), Status> {
    sqlx::query(
        r#"
        INSERT INTO building_parts (
          tenant_id, building_id, part_key, part_type, payload_jsonb, confidence, source_ids, updated_at
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
    .bind(tenant_id)
    .bind(building_id)
    .bind(&part.key)
    .bind(&part.part_type)
    .bind(Json(&part.payload))
    .bind(part.confidence)
    .bind(&part.source_ids)
    .execute(&mut **tx)
    .await
    .map_err(db_status)?;
    Ok(())
}

async fn enqueue_rustyred_projection(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    spec: &ReconstructionSpec,
) -> Result<(), Status> {
    let idempotency_key = projection_idempotency_key(tenant_id, &spec.spec_id, spec.version);
    let payload = json!({
        "projectionKind": "BuildingPresence",
        "specId": spec.spec_id,
        "version": spec.version,
        "buildingId": spec.building_id,
        "civicObjectId": spec.civic_object_id,
    });
    sqlx::query(
        r#"
        INSERT INTO reconstruction_projection_outbox (
          tenant_id, spec_id, spec_version, projection_kind, idempotency_key, payload_jsonb, status
        )
        VALUES ($1, $2, $3, 'rustyred_building_presence', $4, $5, 'pending')
        ON CONFLICT (tenant_id, idempotency_key) DO NOTHING
        "#,
    )
    .bind(tenant_id)
    .bind(&spec.spec_id)
    .bind(spec.version as i32)
    .bind(idempotency_key)
    .bind(Json(payload))
    .execute(&mut **tx)
    .await
    .map_err(db_status)?;
    Ok(())
}

fn project_building_parts(spec: &ReconstructionSpec) -> Vec<PartProjection> {
    let mut parts = Vec::new();
    if let Some(mass) = spec.mass.as_ref() {
        parts.push(part_projection(
            "mass",
            "Mass",
            mass.provenance.as_ref(),
            json!({
                "form": &mass.form,
                "storyCount": mass.story_count,
                "attributes": &mass.attributes,
            }),
        ));
    }
    for (index, facade) in spec.facades.iter().enumerate() {
        push_facade_parts(&mut parts, index, facade);
    }
    if let Some(roof) = spec.roof.as_ref() {
        parts.push(part_projection(
            "roof",
            "Roof",
            roof.provenance.as_ref(),
            json!({
                "form": &roof.form,
                "material": &roof.material,
                "pitchDegrees": roof.pitch_degrees,
                "attributes": &roof.attributes,
            }),
        ));
    }
    for (index, ornament) in spec.ornaments.iter().enumerate() {
        let suffix = if ornament.ornament_id.is_empty() {
            index.to_string()
        } else {
            ornament.ornament_id.clone()
        };
        parts.push(part_projection(
            &format!("ornament:{suffix}"),
            "Ornament",
            ornament.provenance.as_ref(),
            json!({
                "kind": &ornament.kind,
                "location": &ornament.location,
                "material": &ornament.material,
                "attributes": &ornament.attributes,
            }),
        ));
    }
    if let Some(ground_floor) = spec.ground_floor.as_ref() {
        parts.push(part_projection(
            "ground_floor",
            "GroundFloor",
            ground_floor.provenance.as_ref(),
            json!({
                "useType": &ground_floor.use_type,
                "storefrontType": &ground_floor.storefront_type,
                "entryLocation": &ground_floor.entry_location,
                "hasAwning": ground_floor.has_awning,
                "attributes": &ground_floor.attributes,
            }),
        ));
    }
    parts
}

fn push_facade_parts(parts: &mut Vec<PartProjection>, index: usize, facade: &Facade) {
    parts.push(part_projection(
        &format!("facade:{index}"),
        "Facade",
        facade.provenance.as_ref(),
        json!({
            "orientation": &facade.orientation,
            "material": &facade.material,
            "color": &facade.color,
            "attributes": &facade.attributes,
        }),
    ));
    for (grid_index, grid) in facade.opening_grids.iter().enumerate() {
        parts.push(part_projection(
            &format!("facade:{index}:opening_grid:{grid_index}"),
            "OpeningGrid",
            grid.provenance.as_ref(),
            json!({
                "bayCount": grid.bay_count,
                "floorCount": grid.floor_count,
                "rhythm": &grid.rhythm,
                "openingType": &grid.opening_type,
                "attributes": &grid.attributes,
            }),
        ));
    }
}

fn part_projection(
    key: &str,
    part_type: &str,
    provenance: Option<&PartProvenance>,
    payload: Value,
) -> PartProjection {
    let confidence = provenance.map(|item| item.confidence).unwrap_or_default();
    let source_ids = provenance
        .map(|item| {
            item.sources
                .iter()
                .map(|source| source.source_id.clone())
                .filter(|source_id| !source_id.is_empty())
                .collect()
        })
        .unwrap_or_default();
    PartProjection {
        key: key.to_string(),
        part_type: part_type.to_string(),
        payload,
        confidence,
        source_ids,
    }
}

fn spec_to_json(spec: &ReconstructionSpec) -> Value {
    json!({
        "tenantContext": spec.tenant_context.as_ref().map(|tenant| json!({
            "tenantId": &tenant.tenant_id,
            "atlasNodeId": &tenant.atlas_node_id,
            "metadata": &tenant.metadata,
        })),
        "specId": &spec.spec_id,
        "civicObjectId": &spec.civic_object_id,
        "buildingId": &spec.building_id,
        "parcelId": &spec.parcel_id,
        "blockId": &spec.block_id,
        "title": &spec.title,
        "status": status_to_sql(spec.status).unwrap_or("draft"),
        "version": spec.version,
        "supersedesSpecId": &spec.supersedes_spec_id,
        "createdBy": &spec.created_by,
        "reviewedBy": &spec.reviewed_by,
        "mass": spec.mass.as_ref().map(mass_json),
        "facades": spec.facades.iter().map(facade_json).collect::<Vec<_>>(),
        "roof": spec.roof.as_ref().map(roof_json),
        "ornaments": spec.ornaments.iter().map(ornament_json).collect::<Vec<_>>(),
        "groundFloor": spec.ground_floor.as_ref().map(ground_floor_json),
        "metadata": &spec.metadata,
    })
}

fn spec_from_row(row: &PgRow) -> Result<ReconstructionSpec, Status> {
    let value: Json<Value> = row.try_get("spec_jsonb").map_err(db_status)?;
    let status: String = row.try_get("status").map_err(db_status)?;
    let version: i32 = row.try_get("version").map_err(db_status)?;
    let reviewed_by: Option<String> = row.try_get("reviewed_by").map_err(db_status)?;
    let mut spec = spec_from_json(&value.0);
    spec.status = status_from_sql(&status) as i32;
    spec.version = version.max(0) as u32;
    spec.reviewed_by = reviewed_by.unwrap_or_default();
    Ok(spec)
}

fn spec_from_json(value: &Value) -> ReconstructionSpec {
    ReconstructionSpec {
        tenant_context: get_any(value, &["tenantContext", "tenant_context"])
            .map(tenant_context_from_json),
        spec_id: string_json_any(value, &["specId", "spec_id"]),
        civic_object_id: string_json_any(value, &["civicObjectId", "civic_object_id"]),
        building_id: string_json_any(value, &["buildingId", "building_id"]),
        parcel_id: string_json_any(value, &["parcelId", "parcel_id"]),
        block_id: string_json_any(value, &["blockId", "block_id"]),
        title: string_json(value, "title"),
        status: value
            .get("status")
            .and_then(Value::as_str)
            .map(status_from_sql)
            .unwrap_or(ReconstructionSpecStatus::Draft) as i32,
        version: value.get("version").and_then(Value::as_u64).unwrap_or(1) as u32,
        supersedes_spec_id: string_json_any(value, &["supersedesSpecId", "supersedes_spec_id"]),
        created_at_ms: None,
        updated_at_ms: None,
        created_by: string_json_any(value, &["createdBy", "created_by"]),
        reviewed_by: string_json_any(value, &["reviewedBy", "reviewed_by"]),
        mass: get_any(value, &["mass"]).map(mass_from_json),
        facades: value
            .get("facades")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(facade_from_json).collect())
            .unwrap_or_default(),
        roof: get_any(value, &["roof"]).map(roof_from_json),
        ornaments: value
            .get("ornaments")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(ornament_from_json).collect())
            .unwrap_or_default(),
        ground_floor: get_any(value, &["groundFloor", "ground_floor"]).map(ground_floor_from_json),
        assets: Vec::new(),
        metadata: Default::default(),
    }
}

fn asset_from_row(row: &PgRow, tenant_key: &str) -> Result<ReconstructionAsset, Status> {
    let metadata: Json<Value> = row.try_get("metadata_jsonb").map_err(db_status)?;
    let metadata = metadata
        .0
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|text| (key.clone(), text.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ReconstructionAsset {
        asset_id: row.try_get("asset_id").map_err(db_status)?,
        spec_id: row.try_get("spec_id").map_err(db_status)?,
        spec_version: row.try_get::<i32, _>("spec_version").map_err(db_status)? as u32,
        tenant_id: tenant_key.to_string(),
        asset_type: row.try_get("asset_type").map_err(db_status)?,
        uri: row.try_get("uri").map_err(db_status)?,
        content_hash: row
            .try_get::<Option<String>, _>("content_hash")
            .map_err(db_status)?
            .unwrap_or_default(),
        metadata,
    })
}

fn tenant_context_from_json(value: &Value) -> TenantContext {
    TenantContext {
        tenant_id: string_json_any(value, &["tenantId", "tenant_id"]),
        atlas_node_id: string_json_any(value, &["atlasNodeId", "atlas_node_id"]),
        metadata: Default::default(),
    }
}

fn mass_json(mass: &Mass) -> Value {
    json!({
        "provenance": mass.provenance.as_ref().map(provenance_json),
        "form": &mass.form,
        "storyCount": mass.story_count,
        "attributes": &mass.attributes,
    })
}

fn facade_json(facade: &Facade) -> Value {
    json!({
        "provenance": facade.provenance.as_ref().map(provenance_json),
        "orientation": &facade.orientation,
        "material": &facade.material,
        "color": &facade.color,
        "openingGrids": facade.opening_grids.iter().map(opening_grid_json).collect::<Vec<_>>(),
        "attributes": &facade.attributes,
    })
}

fn opening_grid_json(grid: &OpeningGrid) -> Value {
    json!({
        "provenance": grid.provenance.as_ref().map(provenance_json),
        "bayCount": grid.bay_count,
        "floorCount": grid.floor_count,
        "rhythm": &grid.rhythm,
        "openingType": &grid.opening_type,
        "attributes": &grid.attributes,
    })
}

fn roof_json(roof: &Roof) -> Value {
    json!({
        "provenance": roof.provenance.as_ref().map(provenance_json),
        "form": &roof.form,
        "material": &roof.material,
        "pitchDegrees": roof.pitch_degrees,
        "attributes": &roof.attributes,
    })
}

fn ornament_json(ornament: &Ornament) -> Value {
    json!({
        "provenance": ornament.provenance.as_ref().map(provenance_json),
        "ornamentId": &ornament.ornament_id,
        "kind": &ornament.kind,
        "location": &ornament.location,
        "material": &ornament.material,
        "attributes": &ornament.attributes,
    })
}

fn ground_floor_json(ground_floor: &GroundFloor) -> Value {
    json!({
        "provenance": ground_floor.provenance.as_ref().map(provenance_json),
        "useType": &ground_floor.use_type,
        "storefrontType": &ground_floor.storefront_type,
        "entryLocation": &ground_floor.entry_location,
        "hasAwning": ground_floor.has_awning,
        "attributes": &ground_floor.attributes,
    })
}

fn provenance_json(provenance: &PartProvenance) -> Value {
    json!({
        "confidence": provenance.confidence,
        "fromGnnPrior": provenance.from_gnn_prior,
        "reviewerNote": &provenance.reviewer_note,
        "sources": provenance.sources.iter().map(|source| json!({
            "sourceId": &source.source_id,
            "title": &source.title,
            "uri": &source.uri,
        })).collect::<Vec<_>>(),
    })
}

fn mass_from_json(value: &Value) -> Mass {
    Mass {
        provenance: value.get("provenance").map(provenance_from_json),
        form: string_json(value, "form"),
        story_count: get_any(value, &["storyCount", "story_count"])
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        height: None,
        width: None,
        depth: None,
        attributes: Default::default(),
    }
}

fn facade_from_json(value: &Value) -> Facade {
    Facade {
        provenance: value.get("provenance").map(provenance_from_json),
        orientation: string_json(value, "orientation"),
        material: string_json(value, "material"),
        color: string_json(value, "color"),
        opening_grids: value
            .get("openingGrids")
            .or_else(|| value.get("opening_grids"))
            .and_then(Value::as_array)
            .map(|items| items.iter().map(opening_grid_from_json).collect())
            .unwrap_or_default(),
        attributes: Default::default(),
    }
}

fn opening_grid_from_json(value: &Value) -> OpeningGrid {
    OpeningGrid {
        provenance: value.get("provenance").map(provenance_from_json),
        bay_count: get_any(value, &["bayCount", "bay_count"])
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        floor_count: get_any(value, &["floorCount", "floor_count"])
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        rhythm: string_json(value, "rhythm"),
        opening_type: string_json_any(value, &["openingType", "opening_type"]),
        attributes: Default::default(),
    }
}

fn roof_from_json(value: &Value) -> Roof {
    Roof {
        provenance: value.get("provenance").map(provenance_from_json),
        form: string_json(value, "form"),
        material: string_json(value, "material"),
        pitch_degrees: get_any(value, &["pitchDegrees", "pitch_degrees"]).and_then(Value::as_f64),
        attributes: Default::default(),
    }
}

fn ornament_from_json(value: &Value) -> Ornament {
    Ornament {
        provenance: value.get("provenance").map(provenance_from_json),
        ornament_id: string_json_any(value, &["ornamentId", "ornament_id"]),
        kind: string_json(value, "kind"),
        location: string_json(value, "location"),
        material: string_json(value, "material"),
        attributes: Default::default(),
    }
}

fn ground_floor_from_json(value: &Value) -> GroundFloor {
    GroundFloor {
        provenance: value.get("provenance").map(provenance_from_json),
        use_type: string_json_any(value, &["useType", "use_type"]),
        storefront_type: string_json_any(value, &["storefrontType", "storefront_type"]),
        entry_location: string_json_any(value, &["entryLocation", "entry_location"]),
        has_awning: get_any(value, &["hasAwning", "has_awning"])
            .and_then(Value::as_bool)
            .unwrap_or(false),
        attributes: Default::default(),
    }
}

fn provenance_from_json(value: &Value) -> PartProvenance {
    PartProvenance {
        sources: value
            .get("sources")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(source_from_json).collect())
            .unwrap_or_default(),
        confidence: value
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or_default(),
        from_gnn_prior: get_any(value, &["fromGnnPrior", "from_gnn_prior"])
            .and_then(Value::as_bool)
            .unwrap_or_default(),
        reviewer_note: string_json_any(value, &["reviewerNote", "reviewer_note"]),
        coverage_quality: get_any(value, &["coverageQuality", "coverage_quality"])
            .and_then(Value::as_f64)
            .unwrap_or_default(),
        gnn_version: string_json_any(value, &["gnnVersion", "gnn_version"]),
    }
}

fn source_from_json(value: &Value) -> ReconstructionSource {
    ReconstructionSource {
        source_id: string_json_any(value, &["sourceId", "source_id"]),
        title: string_json(value, "title"),
        uri: string_json(value, "uri"),
        citation: string_json(value, "citation"),
        ..Default::default()
    }
}

fn string_json(value: &Value, key: &str) -> String {
    string_json_any(value, &[key])
}

fn string_json_any(value: &Value, keys: &[&str]) -> String {
    get_any(value, keys)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn get_any<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| value.get(*key))
}

fn validate_spec_identity(spec: &ReconstructionSpec) -> Result<(), Status> {
    if spec.spec_id.trim().is_empty() {
        return Err(Status::invalid_argument("spec_id is required"));
    }
    if spec.civic_object_id.trim().is_empty() {
        return Err(Status::invalid_argument("civic_object_id is required"));
    }
    if spec.title.trim().is_empty() {
        return Err(Status::invalid_argument("title is required"));
    }
    if spec.version == 0 {
        return Err(Status::invalid_argument("version is required"));
    }
    Ok(())
}

fn optional_uuid(value: &str, field_name: &str) -> Result<Option<Uuid>, Status> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    value
        .parse()
        .map(Some)
        .map_err(|_| Status::invalid_argument(format!("{field_name} must be a UUID")))
}

fn parse_required_uuid(value: &str, field_name: &str) -> Result<Uuid, Status> {
    value
        .parse()
        .map_err(|_| Status::invalid_argument(format!("{field_name} must be a UUID")))
}

fn status_filter(value: i32) -> Result<Option<&'static str>, Status> {
    if value == ReconstructionSpecStatus::Unspecified as i32 {
        return Ok(None);
    }
    status_to_sql(value)
        .map(Some)
        .ok_or_else(|| Status::invalid_argument("unsupported reconstruction status"))
}

fn status_to_sql(value: i32) -> Option<&'static str> {
    match ReconstructionSpecStatus::try_from(value).ok()? {
        ReconstructionSpecStatus::Unspecified => None,
        ReconstructionSpecStatus::Draft => Some("draft"),
        ReconstructionSpecStatus::InReview => Some("in_review"),
        ReconstructionSpecStatus::Approved => Some("approved"),
        ReconstructionSpecStatus::Superseded => Some("superseded"),
        ReconstructionSpecStatus::Rejected => Some("rejected"),
    }
}

fn status_from_sql(value: &str) -> ReconstructionSpecStatus {
    match value {
        "draft" => ReconstructionSpecStatus::Draft,
        "in_review" => ReconstructionSpecStatus::InReview,
        "approved" => ReconstructionSpecStatus::Approved,
        "superseded" => ReconstructionSpecStatus::Superseded,
        "rejected" => ReconstructionSpecStatus::Rejected,
        _ => ReconstructionSpecStatus::Unspecified,
    }
}

fn projection_idempotency_key(tenant_id: Uuid, spec_id: &str, version: u32) -> String {
    format!("rustyred_building_presence:{tenant_id}:{spec_id}:{version}")
}

fn db_status(error: sqlx::Error) -> Status {
    Status::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use civic_atlas_types::civic_atlas::v1::ReconstructionSource;

    #[test]
    fn approval_projection_keeps_part_level_confidence() {
        let spec = ReconstructionSpec {
            spec_id: "ct-001".to_string(),
            civic_object_id: "building:ct-001".to_string(),
            building_id: Uuid::nil().to_string(),
            title: "Carriage Town storefront".to_string(),
            version: 1,
            mass: Some(Mass {
                provenance: Some(PartProvenance {
                    confidence: 0.8,
                    sources: vec![ReconstructionSource {
                        source_id: "photo:1".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                form: "two-story brick".to_string(),
                ..Default::default()
            }),
            roof: Some(Roof {
                provenance: Some(PartProvenance {
                    confidence: 0.35,
                    from_gnn_prior: true,
                    ..Default::default()
                }),
                form: "flat".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let parts = project_building_parts(&spec);

        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].key, "mass");
        assert_eq!(parts[0].confidence, 0.8);
        assert_eq!(parts[0].source_ids, vec!["photo:1"]);
        assert_eq!(parts[1].key, "roof");
        assert_eq!(parts[1].confidence, 0.35);
    }

    #[test]
    fn projection_outbox_key_is_stable_for_replay() {
        let tenant_id = Uuid::nil();

        assert_eq!(
            projection_idempotency_key(tenant_id, "ct-001", 3),
            "rustyred_building_presence:00000000-0000-0000-0000-000000000000:ct-001:3"
        );
    }
}
