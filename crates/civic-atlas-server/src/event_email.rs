//! Resend email webhook ingestion for event-planner outreach.
//!
//! Resend signs webhook requests with Svix headers. The handler verifies the
//! raw request body before parsing JSON, stores every new event by `svix-id`,
//! and then rolls the linked outreach row forward when the event maps to a
//! known delivery state.

use std::env;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{types::Json as SqlJson, Postgres, Row, Transaction};
use svix::webhooks::Webhook;
use tracing::{info, warn};
use uuid::Uuid;

use crate::AtlasState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebhookIngestResponse {
    ok: bool,
    duplicate: bool,
    provider_event_id: String,
    resend_email_id: Option<String>,
    outreach_id: Option<String>,
}

#[derive(Debug)]
struct LinkedOutreach {
    id: Uuid,
    event_layer_id: Uuid,
    application_id: Option<Uuid>,
}

pub async fn resend_webhook(
    State(state): State<AtlasState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match verify_and_ingest_resend_webhook(state, headers, body).await {
        Ok(response) => Json(response).into_response(),
        Err((status, message)) => {
            let body = Json(json!({ "ok": false, "error": message }));
            (status, body).into_response()
        }
    }
}

async fn verify_and_ingest_resend_webhook(
    state: AtlasState,
    headers: HeaderMap,
    body: Bytes,
) -> Result<WebhookIngestResponse, (StatusCode, String)> {
    let secret = env::var("RESEND_WEBHOOK_SECRET")
        .unwrap_or_default()
        .trim()
        .to_string();
    if secret.is_empty() {
        warn!("[resend-webhook] RESEND_WEBHOOK_SECRET is unset");
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "RESEND_WEBHOOK_SECRET is not configured".to_string(),
        ));
    }

    let webhook = Webhook::new(&secret).map_err(|error| {
        warn!(%error, "[resend-webhook] webhook secret is invalid");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "RESEND_WEBHOOK_SECRET is invalid".to_string(),
        )
    })?;
    webhook.verify(&body, &headers).map_err(|error| {
        warn!(%error, "[resend-webhook] signature verification failed");
        (
            StatusCode::BAD_REQUEST,
            "Resend webhook signature is invalid".to_string(),
        )
    })?;

    let provider_event_id = header_value(&headers, "svix-id").ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "Resend webhook is missing svix-id".to_string(),
        )
    })?;
    let payload: Value = serde_json::from_slice(&body).map_err(|error| {
        warn!(%error, "[resend-webhook] payload JSON parse failed");
        (
            StatusCode::BAD_REQUEST,
            "Resend webhook payload is invalid JSON".to_string(),
        )
    })?;
    let event_type = payload
        .get("type")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "Resend webhook payload is missing type".to_string(),
            )
        })?
        .to_string();
    let resend_email_id = payload
        .get("data")
        .and_then(|data| data.get("email_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let delivery_state = delivery_state_for_event(&event_type).map(str::to_string);
    let event_at = payload
        .get("created_at")
        .and_then(Value::as_str)
        .map(str::to_string);

    let pool = state.db_pool().cloned().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "DATABASE_URL is required for Resend webhook ingestion".to_string(),
        )
    })?;

    let tenant_slug =
        env::var("CIVIC_ATLAS_DEFAULT_TENANT").unwrap_or_else(|_| "flint".to_string());
    let mut tx = pool.begin().await.map_err(db_response)?;
    let tenant_id = resolve_tenant_id(&mut tx, &tenant_slug).await?;
    set_transaction_tenant(&mut tx, tenant_id).await?;

    let linked = match resend_email_id.as_deref() {
        Some(email_id) => find_outreach_by_resend_email(&mut tx, tenant_id, email_id).await?,
        None => None,
    };
    let tag_event_slug = payload
        .get("data")
        .and_then(|data| data.get("tags"))
        .and_then(|tags| tags.get("event"))
        .and_then(Value::as_str);
    let fallback_event_layer_id = match tag_event_slug {
        Some(slug) => resolve_event_layer_id(&mut tx, tenant_id, slug).await?,
        None => None,
    };
    let event_layer_id = linked
        .as_ref()
        .map(|outreach| outreach.event_layer_id)
        .or(fallback_event_layer_id);
    let outreach_id = linked.as_ref().map(|outreach| outreach.id);
    let application_id = linked.as_ref().and_then(|outreach| outreach.application_id);

    let inserted = sqlx::query(
        r#"
        INSERT INTO event_email_events (
            tenant_id,
            event_layer_id,
            outreach_id,
            application_id,
            provider,
            provider_event_id,
            resend_email_id,
            event_type,
            delivery_state,
            payload_json,
            event_at,
            processed_at
        )
        VALUES ($1, $2, $3, $4, 'resend', $5, $6, $7, $8, $9, $10::timestamptz, now())
        ON CONFLICT (tenant_id, provider, provider_event_id)
        DO NOTHING
        RETURNING id
        "#,
    )
    .bind(tenant_id)
    .bind(event_layer_id)
    .bind(outreach_id)
    .bind(application_id)
    .bind(&provider_event_id)
    .bind(&resend_email_id)
    .bind(&event_type)
    .bind(&delivery_state)
    .bind(SqlJson(payload))
    .bind(&event_at)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_response)?
    .is_some();

    if inserted {
        if let (Some(outreach_id), Some(delivery_state)) = (outreach_id, delivery_state.as_deref())
        {
            sqlx::query(
                r#"
                UPDATE event_email_outreach
                SET delivery_state = $3,
                    last_event_at = COALESCE($4::timestamptz, now())
                WHERE tenant_id = $1
                  AND id = $2
                "#,
            )
            .bind(tenant_id)
            .bind(outreach_id)
            .bind(delivery_state)
            .bind(&event_at)
            .execute(&mut *tx)
            .await
            .map_err(db_response)?;
        }
        if let Some(event_layer_id) = event_layer_id {
            sqlx::query(
                r#"
                UPDATE event_email_channels
                SET delivery_webhook_status = 'active'
                WHERE tenant_id = $1
                  AND event_layer_id = $2
                "#,
            )
            .bind(tenant_id)
            .bind(event_layer_id)
            .execute(&mut *tx)
            .await
            .map_err(db_response)?;
        }
    }

    tx.commit().await.map_err(db_response)?;
    info!(
        provider_event_id,
        resend_email_id, inserted, "[resend-webhook] event ingested"
    );

    Ok(WebhookIngestResponse {
        ok: true,
        duplicate: !inserted,
        provider_event_id,
        resend_email_id,
        outreach_id: outreach_id.map(|id| id.to_string()),
    })
}

async fn resolve_tenant_id(
    tx: &mut Transaction<'_, Postgres>,
    tenant_slug: &str,
) -> Result<Uuid, (StatusCode, String)> {
    let row = sqlx::query("SELECT id FROM tenants WHERE slug = $1")
        .bind(tenant_slug)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_response)?;
    row.and_then(|row| row.try_get::<Uuid, _>("id").ok())
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                format!("unknown tenant: {tenant_slug}"),
            )
        })
}

async fn set_transaction_tenant(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_response)?;
    Ok(())
}

async fn resolve_event_layer_id(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    event_slug: &str,
) -> Result<Option<Uuid>, (StatusCode, String)> {
    let row = sqlx::query(
        r#"
        SELECT id
        FROM event_layers
        WHERE tenant_id = $1
          AND slug = $2
        "#,
    )
    .bind(tenant_id)
    .bind(event_slug)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_response)?;
    Ok(row.and_then(|row| row.try_get::<Uuid, _>("id").ok()))
}

async fn find_outreach_by_resend_email(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    resend_email_id: &str,
) -> Result<Option<LinkedOutreach>, (StatusCode, String)> {
    let row = sqlx::query(
        r#"
        SELECT id, event_layer_id, application_id
        FROM event_email_outreach
        WHERE tenant_id = $1
          AND resend_email_id = $2
        LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .bind(resend_email_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_response)?;

    Ok(row.map(|row| LinkedOutreach {
        id: row.get("id"),
        event_layer_id: row.get("event_layer_id"),
        application_id: row
            .try_get::<Option<Uuid>, _>("application_id")
            .ok()
            .flatten(),
    }))
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn delivery_state_for_event(event_type: &str) -> Option<&'static str> {
    match event_type {
        "email.sent" | "email.scheduled" => Some("sent"),
        "email.delivered" => Some("delivered"),
        "email.opened" => Some("opened"),
        "email.clicked" => Some("clicked"),
        "email.delivery_delayed" => Some("delivery_delayed"),
        "email.bounced" => Some("bounced"),
        "email.complained" => Some("complained"),
        "email.failed" => Some("failed"),
        "email.suppressed" => Some("suppressed"),
        "email.received" => Some("received"),
        _ => None,
    }
}

fn db_response(error: sqlx::Error) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Resend webhook database write failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::delivery_state_for_event;

    #[test]
    fn delivery_state_maps_resend_lifecycle_events() {
        assert_eq!(delivery_state_for_event("email.sent"), Some("sent"));
        assert_eq!(
            delivery_state_for_event("email.delivery_delayed"),
            Some("delivery_delayed")
        );
        assert_eq!(delivery_state_for_event("email.bounced"), Some("bounced"));
        assert_eq!(delivery_state_for_event("email.received"), Some("received"));
        assert_eq!(delivery_state_for_event("domain.created"), None);
    }
}
