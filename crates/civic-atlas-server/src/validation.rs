//! Geographic claim triangulation.
//!
//! Every reconstruction spec carries multiple claim types about
//! a building: a coordinate (from the building's `geom`), a postal
//! description (in `spec_jsonb`), a district/block claim (in
//! `spec.block_id`), and a name. When these claims disagree the
//! system records a row in `place_provenance_disputes` so a
//! moderator (or ACC/ACT) can sort it out.
//!
//! Today's validators are O(1) PostGIS predicates:
//!
//! - **`validate_building_in_tenant_bbox`** — the building's coord
//!   centroid must fall inside `tenants.bbox`. Catches cross-tenant
//!   coord errors (a "flint" place at a Chicago coord) trivially.
//!
//! - **`validate_building_in_claimed_district`** — when the spec
//!   names a district (parsed from `block_id` like
//!   `block:carriage-town:central`), point-in-polygon-check the
//!   building's coord against `civic_districts.polygon` for that
//!   slug. Catches the Carriage Town / Civic Park name-collision
//!   class of error.
//!
//! Validators only WRITE disputes to `place_provenance_disputes`.
//! They never block the operation. Severity is `flag` by default;
//! `block`-severity disputes are reserved for future cases where
//! a coord is so wrong it cannot be a typo (e.g. wrong hemisphere).

#![allow(clippy::result_large_err)]

use serde_json::{json, Value};
use sqlx::{types::Json, Postgres, Row, Transaction};
use tonic::Status;
use uuid::Uuid;

/// Map dispute records back to the SQL CHECK constraints.
pub mod kinds {
    pub const TENANT_BBOX: &str = "tenant_bbox_mismatch";
    pub const DISTRICT_MEMBERSHIP: &str = "district_membership_mismatch";
}

pub mod target_types {
    pub const BUILDING: &str = "building";
    pub const RECONSTRUCTION_SPEC: &str = "reconstruction_spec";
}

pub mod severity {
    pub const FLAG: &str = "flag";
    pub const WARN: &str = "warn";
    pub const BLOCK: &str = "block";
}

/// One dispute the validator wants to record. The caller writes it
/// to `place_provenance_disputes`. Kept as a struct (not directly
/// inserted) so the caller controls the transaction boundary.
#[derive(Debug, Clone)]
pub struct DisputeRecord {
    pub target_type: &'static str,
    pub target_id: Uuid,
    pub dispute_kind: &'static str,
    pub severity: &'static str,
    pub evidence_text: String,
    pub evidence_jsonb: Value,
}

/// Run the available validators against a building + its claimed
/// reconstruction spec. Returns the dispute records the caller should
/// insert. Empty vec = no disagreements found.
///
/// Validators run in order; all are advisory. The caller decides
/// whether to insert + whether to also report to logs.
pub async fn validate_building_against_spec(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    building_id: Uuid,
    spec_id: &str,
    block_id: Option<&str>,
) -> Result<Vec<DisputeRecord>, Status> {
    let mut disputes = Vec::new();

    if let Some(d) = validate_building_in_tenant_bbox(tx, tenant_id, building_id, spec_id).await? {
        disputes.push(d);
    }

    if let Some(slug) = block_id.and_then(district_slug_from_block_id) {
        if let Some(d) =
            validate_building_in_claimed_district(tx, tenant_id, building_id, spec_id, slug).await?
        {
            disputes.push(d);
        }
    }

    Ok(disputes)
}

/// Insert a batch of dispute records into PostGIS.
pub async fn record_disputes(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    disputes: &[DisputeRecord],
) -> Result<(), Status> {
    for dispute in disputes {
        sqlx::query(
            r#"
            INSERT INTO place_provenance_disputes (
                tenant_id, target_type, target_id, dispute_kind,
                severity, evidence_text, evidence_jsonb
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(tenant_id)
        .bind(dispute.target_type)
        .bind(dispute.target_id)
        .bind(dispute.dispute_kind)
        .bind(dispute.severity)
        .bind(&dispute.evidence_text)
        .bind(Json(&dispute.evidence_jsonb))
        .execute(&mut **tx)
        .await
        .map_err(|e| Status::internal(format!("recording dispute failed: {e}")))?;
    }
    Ok(())
}

/// Parse the district slug out of a block id like
/// `block:carriage-town:central` -> `Some("carriage-town")`.
///
/// Format: `block:<district>:<sub-id>`. Returns `None` for any
/// shape that doesn't match — the validator simply skips
/// district-membership check rather than emitting a false dispute.
fn district_slug_from_block_id(block_id: &str) -> Option<&str> {
    let trimmed = block_id.trim();
    let mut parts = trimmed.splitn(3, ':');
    let prefix = parts.next()?;
    if prefix != "block" {
        return None;
    }
    let slug = parts.next()?;
    if slug.is_empty() {
        return None;
    }
    Some(slug)
}

async fn validate_building_in_tenant_bbox(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    building_id: Uuid,
    spec_id: &str,
) -> Result<Option<DisputeRecord>, Status> {
    let row = sqlx::query(
        r#"
        SELECT
            ST_Contains(t.bbox, ST_Centroid(b.geom)) AS contained,
            ST_AsText(ST_Centroid(b.geom)) AS centroid_wkt,
            ST_AsText(t.bbox) AS bbox_wkt
        FROM buildings b
        JOIN tenants t ON t.id = b.tenant_id
        WHERE b.tenant_id = $1 AND b.id = $2
        "#,
    )
    .bind(tenant_id)
    .bind(building_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| Status::internal(format!("bbox check query failed: {e}")))?;

    let Some(row) = row else {
        // Building missing — caller already validated this; nothing
        // to dispute against an absent entity.
        return Ok(None);
    };

    let contained: Option<bool> = row.try_get("contained").ok();
    let centroid_wkt: Option<String> = row.try_get("centroid_wkt").ok();
    let bbox_wkt: Option<String> = row.try_get("bbox_wkt").ok();

    // contained = None means either bbox or centroid is NULL. Treat
    // NULL bbox as "tenant hasn't declared its region" — skip the
    // check, don't emit a dispute (the validator would generate
    // false positives for every tenant pre-bbox-rollout).
    let contained = match contained {
        Some(b) => b,
        None => return Ok(None),
    };

    if contained {
        return Ok(None);
    }

    let evidence_text = format!(
        "Building {building_id} (spec {spec_id}) centroid {centroid} \
         is outside tenant bounding region {bbox}.",
        centroid = centroid_wkt.as_deref().unwrap_or("<null>"),
        bbox = bbox_wkt.as_deref().unwrap_or("<null>"),
    );

    Ok(Some(DisputeRecord {
        target_type: target_types::BUILDING,
        target_id: building_id,
        dispute_kind: kinds::TENANT_BBOX,
        severity: severity::WARN,
        evidence_text,
        evidence_jsonb: json!({
            "centroid_wkt": centroid_wkt,
            "tenant_bbox_wkt": bbox_wkt,
            "spec_id": spec_id,
        }),
    }))
}

async fn validate_building_in_claimed_district(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    building_id: Uuid,
    spec_id: &str,
    district_slug: &str,
) -> Result<Option<DisputeRecord>, Status> {
    let row = sqlx::query(
        r#"
        SELECT
            ST_Contains(d.polygon, ST_Centroid(b.geom)) AS contained,
            ST_AsText(ST_Centroid(b.geom)) AS centroid_wkt,
            d.display_name AS district_name,
            d.id AS district_id
        FROM buildings b
        JOIN civic_districts d
          ON d.tenant_id = b.tenant_id AND d.slug = $3
        WHERE b.tenant_id = $1 AND b.id = $2
        "#,
    )
    .bind(tenant_id)
    .bind(building_id)
    .bind(district_slug)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| Status::internal(format!("district check query failed: {e}")))?;

    let Some(row) = row else {
        // No district by that slug for this tenant — the spec named
        // something we don't have a polygon for. Could be a typo or
        // could be a district we haven't seeded. Emit nothing for
        // MVP; future passes can add a "unknown_district" dispute
        // kind for moderator awareness.
        return Ok(None);
    };

    let contained: Option<bool> = row.try_get("contained").ok();
    let centroid_wkt: Option<String> = row.try_get("centroid_wkt").ok();
    let district_name: String = row.try_get("district_name").unwrap_or_default();
    let district_id: Option<Uuid> = row.try_get("district_id").ok();

    let contained = match contained {
        Some(b) => b,
        None => return Ok(None),
    };

    if contained {
        return Ok(None);
    }

    let evidence_text = format!(
        "Building {building_id} (spec {spec_id}) claims membership in district \
         '{district_name}' (slug='{district_slug}') but its centroid {centroid} \
         falls outside the district's polygon. \
         This is the Carriage Town / Civic Park name-collision pattern: \
         the description names a district whose actual boundary doesn't \
         contain the coord. Either the coord is wrong, the district name \
         is wrong, or the district polygon needs updating.",
        centroid = centroid_wkt.as_deref().unwrap_or("<null>"),
    );

    Ok(Some(DisputeRecord {
        target_type: target_types::BUILDING,
        target_id: building_id,
        dispute_kind: kinds::DISTRICT_MEMBERSHIP,
        severity: severity::WARN,
        evidence_text,
        evidence_jsonb: json!({
            "centroid_wkt": centroid_wkt,
            "claimed_district_slug": district_slug,
            "claimed_district_id": district_id.map(|u| u.to_string()),
            "claimed_district_name": district_name,
            "spec_id": spec_id,
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn district_slug_parses_canonical_form() {
        assert_eq!(
            district_slug_from_block_id("block:carriage-town:central"),
            Some("carriage-town")
        );
    }

    #[test]
    fn district_slug_rejects_non_block_prefix() {
        assert_eq!(district_slug_from_block_id("parcel:foo:bar"), None);
    }

    #[test]
    fn district_slug_handles_trailing_whitespace() {
        assert_eq!(
            district_slug_from_block_id("  block:downtown:01  "),
            Some("downtown")
        );
    }

    #[test]
    fn district_slug_rejects_empty_slug() {
        assert_eq!(district_slug_from_block_id("block::central"), None);
    }
}
