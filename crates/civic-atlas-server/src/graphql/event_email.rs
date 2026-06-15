//! GraphQL types and resolvers for event email channel/outreach state.

use std::env;

use async_graphql::{Context, Enum, InputObject, Object, SimpleObject};
use chrono::{DateTime, Utc};
use reqwest::StatusCode as HttpStatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{
    postgres::PgRow,
    types::{time::OffsetDateTime, Json as SqlJson},
    Postgres, Row, Transaction,
};
use uuid::Uuid;

use crate::{
    event_planner::NO_LOGIN_PLANNER_ACTOR_ID, graphql::event_planner::PlannerActor, AtlasState,
};

const NO_LOGIN_PLANNER_EMAIL: &str = "porchfest-crew@ourcivicatlas.local";
const NO_LOGIN_PLANNER_DISPLAY_NAME: &str = "Porchfest Crew";

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum EventEmailDeliveryState {
    Queued,
    Sent,
    Delivered,
    Opened,
    Clicked,
    DeliveryDelayed,
    Bounced,
    Complained,
    Failed,
    Suppressed,
    Received,
}

impl EventEmailDeliveryState {
    fn as_db(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sent => "sent",
            Self::Delivered => "delivered",
            Self::Opened => "opened",
            Self::Clicked => "clicked",
            Self::DeliveryDelayed => "delivery_delayed",
            Self::Bounced => "bounced",
            Self::Complained => "complained",
            Self::Failed => "failed",
            Self::Suppressed => "suppressed",
            Self::Received => "received",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "sent" => Self::Sent,
            "delivered" => Self::Delivered,
            "opened" => Self::Opened,
            "clicked" => Self::Clicked,
            "delivery_delayed" => Self::DeliveryDelayed,
            "bounced" => Self::Bounced,
            "complained" => Self::Complained,
            "failed" => Self::Failed,
            "suppressed" => Self::Suppressed,
            "received" => Self::Received,
            _ => Self::Queued,
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum EventEmailReplyState {
    NotReplied,
    Replied,
    Deferred,
    Manual,
}

impl EventEmailReplyState {
    fn as_db(self) -> &'static str {
        match self {
            Self::NotReplied => "not_replied",
            Self::Replied => "replied",
            Self::Deferred => "deferred",
            Self::Manual => "manual",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "replied" => Self::Replied,
            "deferred" => Self::Deferred,
            "manual" => Self::Manual,
            _ => Self::NotReplied,
        }
    }
}

#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum EventEmailReplyRoutingMode {
    GmailMetadata,
    ResendInbound,
    Manual,
}

impl EventEmailReplyRoutingMode {
    fn as_db(self) -> &'static str {
        match self {
            Self::GmailMetadata => "gmail_metadata",
            Self::ResendInbound => "resend_inbound",
            Self::Manual => "manual",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "gmail_metadata" => Self::GmailMetadata,
            "resend_inbound" => Self::ResendInbound,
            _ => Self::Manual,
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct EventEmailChannel {
    pub id: String,
    pub event_layer_id: String,
    pub provider: String,
    pub sender_email: String,
    pub sender_name: Option<String>,
    pub reply_to_email: Option<String>,
    pub reply_routing_mode: EventEmailReplyRoutingMode,
    pub delivery_webhook_status: String,
    pub provider_deployment_label: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub version: i32,
}

#[derive(SimpleObject, Clone)]
pub struct EventEmailOutreach {
    pub id: String,
    pub event_layer_id: String,
    pub application_id: Option<String>,
    pub recipient_email: String,
    pub subject: String,
    pub preview_text: Option<String>,
    pub resend_email_id: Option<String>,
    pub message_id: Option<String>,
    pub reply_to_email: Option<String>,
    pub delivery_state: EventEmailDeliveryState,
    pub reply_state: EventEmailReplyState,
    pub notes_doc_id: Option<String>,
    pub created_by_user_id: Option<String>,
    pub sent_at: Option<String>,
    pub last_event_at: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub version: i32,
}

#[derive(SimpleObject, Clone)]
pub struct EventEmailChannelMutationResult {
    pub channel: Option<EventEmailChannel>,
    pub stale_write: bool,
}

#[derive(SimpleObject, Clone)]
pub struct EventEmailOutreachMutationResult {
    pub outreach: Option<EventEmailOutreach>,
    pub stale_write: bool,
}

#[derive(InputObject)]
pub struct EventEmailChannelConfigureInput {
    pub event_slug: String,
    pub provider: Option<String>,
    pub sender_email: Option<String>,
    pub sender_name: Option<String>,
    pub reply_to_email: Option<String>,
    pub reply_routing_mode: Option<EventEmailReplyRoutingMode>,
    pub provider_deployment_label: Option<String>,
    pub expected_version: Option<i32>,
}

#[derive(InputObject)]
pub struct EventApplicationEmailSendInput {
    pub event_slug: String,
    pub application_id: String,
    pub subject: String,
    pub body_markdown: String,
    pub reply_to_email: Option<String>,
    pub notes_doc_id: Option<String>,
    pub idempotency_key: Option<String>,
}

#[derive(InputObject)]
pub struct EventEmailOutreachUpdateInput {
    pub outreach_id: String,
    pub expected_version: i32,
    pub delivery_state: Option<EventEmailDeliveryState>,
    pub reply_state: Option<EventEmailReplyState>,
    pub notes_doc_id: Option<String>,
}

#[derive(Default)]
pub struct EventEmailQuery;

#[Object]
impl EventEmailQuery {
    async fn event_email_channel(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = "flint")] tenant_slug: String,
        event_slug: String,
    ) -> async_graphql::Result<Option<EventEmailChannel>> {
        let pool = pool(ctx)?;
        let mut tx = pool.begin().await.map_err(graphql_db_error)?;
        let tenant_id = resolve_tenant_id(&mut tx, &tenant_slug).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let event_layer_id = resolve_event_layer_id(&mut tx, tenant_id, &event_slug).await?;

        let sql = channel_select_sql(
            r#"
            FROM event_email_channels
            WHERE tenant_id = $1
              AND event_layer_id = $2
            "#,
        );
        let row = sqlx::query(&sql)
            .bind(tenant_id)
            .bind(event_layer_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(graphql_db_error)?;
        tx.commit().await.map_err(graphql_db_error)?;

        Ok(row.as_ref().map(channel_from_row))
    }

    async fn event_email_outreach(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = "flint")] tenant_slug: String,
        event_slug: String,
        application_id: Option<String>,
        delivery_state: Option<EventEmailDeliveryState>,
        reply_state: Option<EventEmailReplyState>,
    ) -> async_graphql::Result<Vec<EventEmailOutreach>> {
        let pool = pool(ctx)?;
        let mut tx = pool.begin().await.map_err(graphql_db_error)?;
        let tenant_id = resolve_tenant_id(&mut tx, &tenant_slug).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let event_layer_id = resolve_event_layer_id(&mut tx, tenant_id, &event_slug).await?;
        let application_uuid = optional_uuid(application_id.as_deref(), "application_id")?;
        let delivery_state = delivery_state.map(|state| state.as_db().to_string());
        let reply_state = reply_state.map(|state| state.as_db().to_string());

        let sql = outreach_select_sql(
            r#"
            FROM event_email_outreach
            WHERE tenant_id = $1
              AND event_layer_id = $2
              AND ($3::uuid IS NULL OR application_id = $3)
              AND ($4::text IS NULL OR delivery_state = $4)
              AND ($5::text IS NULL OR reply_state = $5)
            ORDER BY created_at DESC
            LIMIT 200
            "#,
        );
        let rows = sqlx::query(&sql)
            .bind(tenant_id)
            .bind(event_layer_id)
            .bind(application_uuid)
            .bind(delivery_state)
            .bind(reply_state)
            .fetch_all(&mut *tx)
            .await
            .map_err(graphql_db_error)?;
        tx.commit().await.map_err(graphql_db_error)?;

        Ok(rows.iter().map(outreach_from_row).collect())
    }
}

#[derive(Default)]
pub struct EventEmailMutation;

#[Object]
impl EventEmailMutation {
    async fn configure_event_email_channel(
        &self,
        ctx: &Context<'_>,
        input: EventEmailChannelConfigureInput,
    ) -> async_graphql::Result<EventEmailChannelMutationResult> {
        let pool = pool(ctx)?;
        let mut tx = pool.begin().await.map_err(graphql_db_error)?;
        let tenant_id = resolve_tenant_id(&mut tx, &default_tenant_slug()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let event_layer_id = resolve_event_layer_id(&mut tx, tenant_id, &input.event_slug).await?;

        if let Some(expected_version) = input.expected_version {
            if let Some(row) = current_channel(&mut tx, tenant_id, event_layer_id).await? {
                let current_version = row.try_get::<i64, _>("version").unwrap_or(1);
                if current_version != i64::from(expected_version) {
                    tx.commit().await.map_err(graphql_db_error)?;
                    return Ok(EventEmailChannelMutationResult {
                        channel: Some(channel_from_row(&row)),
                        stale_write: true,
                    });
                }
            }
        }

        let provider_env = env::var("PORCHFEST_EMAIL_PROVIDER").ok();
        let provider = nonempty_or(
            input.provider.as_deref(),
            provider_env.as_deref().unwrap_or("resend"),
        );
        let sender_email = nonempty_or(input.sender_email.as_deref(), "porchfest@cthna.org");
        let sender_name_env = env::var("PORCHFEST_EMAIL_SENDER_NAME").ok();
        let sender_name = clean_optional(
            input
                .sender_name
                .as_deref()
                .or(sender_name_env.as_deref())
                .or(Some("Carriage Town PorchFest")),
        );
        let reply_to_env = env::var("PORCHFEST_EMAIL_REPLY_TO").ok();
        let reply_to_email = clean_optional(
            input
                .reply_to_email
                .as_deref()
                .or(reply_to_env.as_deref())
                .or(Some("porchfest@cthna.org")),
        );
        let reply_routing_mode = input
            .reply_routing_mode
            .unwrap_or(EventEmailReplyRoutingMode::Manual)
            .as_db();
        let label_env = env::var("PORCHFEST_EMAIL_CHANNEL_LABEL").ok();
        let provider_deployment_label = clean_optional(
            input
                .provider_deployment_label
                .as_deref()
                .or(label_env.as_deref())
                .or(Some("Railway: civic-atlas-outbox-worker")),
        );
        let webhook_status = if env::var("RESEND_WEBHOOK_SECRET")
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        {
            "configured"
        } else {
            "not_configured"
        };

        let sql = channel_select_sql_with_prefix(
            r#"
            WITH channel AS (
              INSERT INTO event_email_channels (
                  tenant_id,
                  event_layer_id,
                  provider,
                  sender_email,
                  sender_name,
                  reply_to_email,
                  reply_routing_mode,
                  delivery_webhook_status,
                  provider_deployment_label
              )
              VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
              ON CONFLICT (tenant_id, event_layer_id)
              DO UPDATE SET
                  provider = EXCLUDED.provider,
                  sender_email = EXCLUDED.sender_email,
                  sender_name = EXCLUDED.sender_name,
                  reply_to_email = EXCLUDED.reply_to_email,
                  reply_routing_mode = EXCLUDED.reply_routing_mode,
                  delivery_webhook_status = EXCLUDED.delivery_webhook_status,
                  provider_deployment_label = EXCLUDED.provider_deployment_label
              RETURNING *
            )
            "#,
            "FROM channel",
        );
        let row = sqlx::query(&sql)
            .bind(tenant_id)
            .bind(event_layer_id)
            .bind(provider)
            .bind(sender_email)
            .bind(sender_name)
            .bind(reply_to_email)
            .bind(reply_routing_mode)
            .bind(webhook_status)
            .bind(provider_deployment_label)
            .fetch_one(&mut *tx)
            .await
            .map_err(graphql_db_error)?;
        tx.commit().await.map_err(graphql_db_error)?;

        Ok(EventEmailChannelMutationResult {
            channel: Some(channel_from_row(&row)),
            stale_write: false,
        })
    }

    async fn send_event_application_email(
        &self,
        ctx: &Context<'_>,
        input: EventApplicationEmailSendInput,
    ) -> async_graphql::Result<EventEmailOutreachMutationResult> {
        let api_key = env::var("RESEND_API_KEY")
            .map_err(|_| async_graphql::Error::new("RESEND_API_KEY is not configured"))?;
        if api_key.trim().is_empty() {
            return Err(async_graphql::Error::new(
                "RESEND_API_KEY is not configured",
            ));
        }
        let from = env::var("PORCHFEST_EMAIL_FROM")
            .unwrap_or_else(|_| "Carriage Town PorchFest <porchfest@cthna.org>".to_string());
        if from.trim().is_empty() {
            return Err(async_graphql::Error::new(
                "PORCHFEST_EMAIL_FROM is not configured",
            ));
        }

        let pool = pool(ctx)?;
        let mut tx = pool.begin().await.map_err(graphql_db_error)?;
        let tenant_id = resolve_tenant_id(&mut tx, &default_tenant_slug()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let actor_uuid =
            resolve_actor_uuid(&mut tx, tenant_id, actor_user_id(ctx).as_deref()).await?;
        let application_uuid = parse_uuid(&input.application_id, "applicationId")?;
        let application =
            fetch_application_for_email(&mut tx, tenant_id, &input.event_slug, application_uuid)
                .await?;
        let reply_to_env = env::var("PORCHFEST_EMAIL_REPLY_TO").ok();
        let reply_to = clean_optional(
            input
                .reply_to_email
                .as_deref()
                .or(reply_to_env.as_deref())
                .or(Some("porchfest@cthna.org")),
        );
        let idempotency_key =
            clean_optional(input.idempotency_key.as_deref()).unwrap_or_else(|| {
                default_send_idempotency_key(application_uuid, &input.subject, &input.body_markdown)
            });
        let preview_text = markdown_preview(&input.body_markdown);

        let existing = fetch_outreach_by_idempotency(
            &mut tx,
            tenant_id,
            application.event_layer_id,
            &idempotency_key,
        )
        .await?;
        let outreach = if let Some(row) = existing {
            outreach_from_row(&row)
        } else {
            let row = insert_outreach(
                &mut tx,
                NewOutreach {
                    tenant_id,
                    event_layer_id: application.event_layer_id,
                    application_id: Some(application.id),
                    recipient_email: application.contact_email.clone(),
                    subject: input.subject.clone(),
                    preview_text: Some(preview_text),
                    body_markdown: Some(input.body_markdown.clone()),
                    reply_to_email: reply_to.clone(),
                    notes_doc_id: clean_optional(input.notes_doc_id.as_deref()),
                    created_by_user_id: actor_uuid,
                    idempotency_key: Some(idempotency_key.clone()),
                },
            )
            .await?;
            outreach_from_row(&row)
        };

        if outreach.resend_email_id.is_some() {
            tx.commit().await.map_err(graphql_db_error)?;
            return Ok(EventEmailOutreachMutationResult {
                outreach: Some(outreach),
                stale_write: false,
            });
        }
        tx.commit().await.map_err(graphql_db_error)?;

        let email = ResendEmailRequest {
            from: from.trim().to_string(),
            to: vec![application.contact_email.clone()],
            subject: input.subject,
            text: input.body_markdown.clone(),
            html: markdown_to_html(&input.body_markdown),
            reply_to,
            tags: vec![
                ResendTag {
                    name: "event".to_string(),
                    value: sanitize_resend_tag_value(&input.event_slug),
                },
                ResendTag {
                    name: "application_id".to_string(),
                    value: sanitize_resend_tag_value(&application.id.to_string()),
                },
                ResendTag {
                    name: "source".to_string(),
                    value: "planner-outreach".to_string(),
                },
            ],
        };
        match send_resend_email(&api_key, &idempotency_key, &email).await {
            Ok(resend_email_id) => {
                let mut tx = pool.begin().await.map_err(graphql_db_error)?;
                set_transaction_tenant(&mut tx, tenant_id).await?;
                let row =
                    mark_outreach_sent(&mut tx, tenant_id, &outreach.id, &resend_email_id).await?;
                tx.commit().await.map_err(graphql_db_error)?;
                Ok(EventEmailOutreachMutationResult {
                    outreach: Some(outreach_from_row(&row)),
                    stale_write: false,
                })
            }
            Err(error) => {
                let mut tx = pool.begin().await.map_err(graphql_db_error)?;
                set_transaction_tenant(&mut tx, tenant_id).await?;
                let _ = mark_outreach_failed(&mut tx, tenant_id, &outreach.id).await;
                tx.commit().await.map_err(graphql_db_error)?;
                Err(error)
            }
        }
    }

    async fn update_event_email_outreach(
        &self,
        ctx: &Context<'_>,
        input: EventEmailOutreachUpdateInput,
    ) -> async_graphql::Result<EventEmailOutreachMutationResult> {
        let pool = pool(ctx)?;
        let mut tx = pool.begin().await.map_err(graphql_db_error)?;
        let tenant_id = resolve_tenant_id(&mut tx, &default_tenant_slug()).await?;
        set_transaction_tenant(&mut tx, tenant_id).await?;
        let outreach_id = parse_uuid(&input.outreach_id, "outreachId")?;

        let Some(current) = current_outreach(&mut tx, tenant_id, outreach_id).await? else {
            return Err(async_graphql::Error::new("event email outreach not found"));
        };
        let current_version = current.try_get::<i64, _>("version").unwrap_or(1);
        if current_version != i64::from(input.expected_version) {
            tx.commit().await.map_err(graphql_db_error)?;
            return Ok(EventEmailOutreachMutationResult {
                outreach: Some(outreach_from_row(&current)),
                stale_write: true,
            });
        }

        let delivery_state = input.delivery_state.map(|state| state.as_db().to_string());
        let reply_state = input.reply_state.map(|state| state.as_db().to_string());
        let notes_doc_id = clean_optional(input.notes_doc_id.as_deref());
        let sql = outreach_select_sql_with_prefix(
            r#"
            WITH outreach AS (
              UPDATE event_email_outreach
              SET delivery_state = COALESCE($3, delivery_state),
                  reply_state = COALESCE($4, reply_state),
                  notes_doc_id = COALESCE($5, notes_doc_id),
                  last_event_at = CASE
                    WHEN $3::text IS NOT NULL THEN now()
                    ELSE last_event_at
                  END
              WHERE tenant_id = $1
                AND id = $2
              RETURNING *
            )
            "#,
            "FROM outreach",
        );
        let row = sqlx::query(&sql)
            .bind(tenant_id)
            .bind(outreach_id)
            .bind(delivery_state)
            .bind(reply_state)
            .bind(notes_doc_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(graphql_db_error)?;
        tx.commit().await.map_err(graphql_db_error)?;

        Ok(EventEmailOutreachMutationResult {
            outreach: Some(outreach_from_row(&row)),
            stale_write: false,
        })
    }
}

struct ApplicationEmailTarget {
    id: Uuid,
    event_layer_id: Uuid,
    contact_email: String,
}

struct NewOutreach {
    tenant_id: Uuid,
    event_layer_id: Uuid,
    application_id: Option<Uuid>,
    recipient_email: String,
    subject: String,
    preview_text: Option<String>,
    body_markdown: Option<String>,
    reply_to_email: Option<String>,
    notes_doc_id: Option<String>,
    created_by_user_id: Option<Uuid>,
    idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
struct ResendEmailRequest {
    from: String,
    to: Vec<String>,
    subject: String,
    text: String,
    html: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to: Option<String>,
    tags: Vec<ResendTag>,
}

#[derive(Debug, Serialize)]
struct ResendTag {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct ResendEmailResponse {
    id: Option<String>,
    data: Option<ResendEmailResponseData>,
    error: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ResendEmailResponseData {
    id: Option<String>,
}

impl ResendEmailResponse {
    fn email_id(self) -> Option<String> {
        self.id.or_else(|| self.data.and_then(|data| data.id))
    }
}

async fn send_resend_email(
    api_key: &str,
    idempotency_key: &str,
    email: &ResendEmailRequest,
) -> async_graphql::Result<String> {
    let api_url =
        env::var("RESEND_API_URL").unwrap_or_else(|_| "https://api.resend.com/emails".to_string());
    let response = reqwest::Client::new()
        .post(api_url.trim())
        .bearer_auth(api_key.trim())
        .header("content-type", "application/json")
        .header("Idempotency-Key", idempotency_key)
        .json(email)
        .send()
        .await
        .map_err(|error| async_graphql::Error::new(format!("Resend request failed: {error}")))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(async_graphql::Error::new(format!(
            "Resend send failed ({status}): {}",
            truncate_error(&body)
        )));
    }

    let parsed: ResendEmailResponse = serde_json::from_str(&body).map_err(|error| {
        async_graphql::Error::new(format!("Resend response was not understood: {error}"))
    })?;
    if let Some(error) = parsed.error {
        return Err(async_graphql::Error::new(format!(
            "Resend send failed: {}",
            truncate_error(&error.to_string())
        )));
    }
    parsed
        .email_id()
        .ok_or_else(|| async_graphql::Error::new("Resend response did not include an email id"))
}

fn pool(ctx: &Context<'_>) -> async_graphql::Result<sqlx::PgPool> {
    let state = ctx
        .data::<AtlasState>()
        .map_err(|_| async_graphql::Error::new("AtlasState missing from GraphQL context"))?;
    state
        .db_pool()
        .cloned()
        .ok_or_else(|| async_graphql::Error::new("DATABASE_URL is required for event email"))
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

async fn current_channel(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    event_layer_id: Uuid,
) -> async_graphql::Result<Option<PgRow>> {
    let sql = channel_select_sql(
        r#"
        FROM event_email_channels
        WHERE tenant_id = $1
          AND event_layer_id = $2
        "#,
    );
    sqlx::query(&sql)
        .bind(tenant_id)
        .bind(event_layer_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(graphql_db_error)
}

async fn current_outreach(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    outreach_id: Uuid,
) -> async_graphql::Result<Option<PgRow>> {
    let sql = outreach_select_sql(
        r#"
        FROM event_email_outreach
        WHERE tenant_id = $1
          AND id = $2
        "#,
    );
    sqlx::query(&sql)
        .bind(tenant_id)
        .bind(outreach_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(graphql_db_error)
}

async fn fetch_outreach_by_idempotency(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    event_layer_id: Uuid,
    idempotency_key: &str,
) -> async_graphql::Result<Option<PgRow>> {
    let sql = outreach_select_sql(
        r#"
        FROM event_email_outreach
        WHERE tenant_id = $1
          AND event_layer_id = $2
          AND idempotency_key = $3
        "#,
    );
    sqlx::query(&sql)
        .bind(tenant_id)
        .bind(event_layer_id)
        .bind(idempotency_key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(graphql_db_error)
}

async fn fetch_application_for_email(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    event_slug: &str,
    application_id: Uuid,
) -> async_graphql::Result<ApplicationEmailTarget> {
    let row = sqlx::query(
        r#"
        SELECT a.id, a.event_layer_id, a.contact_email
        FROM event_applications a
        JOIN event_layers l
          ON l.id = a.event_layer_id
         AND l.tenant_id = a.tenant_id
        WHERE a.tenant_id = $1
          AND l.slug = $2
          AND a.id = $3
        "#,
    )
    .bind(tenant_id)
    .bind(event_slug)
    .bind(application_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(graphql_db_error)?
    .ok_or_else(|| async_graphql::Error::new("event application not found"))?;

    Ok(ApplicationEmailTarget {
        id: row.get("id"),
        event_layer_id: row.get("event_layer_id"),
        contact_email: row.get("contact_email"),
    })
}

async fn insert_outreach(
    tx: &mut Transaction<'_, Postgres>,
    outreach: NewOutreach,
) -> async_graphql::Result<PgRow> {
    let sql = outreach_select_sql_with_prefix(
        r#"
        WITH outreach AS (
          INSERT INTO event_email_outreach (
              tenant_id,
              event_layer_id,
              application_id,
              recipient_email,
              subject,
              preview_text,
              body_markdown,
              reply_to_email,
              notes_doc_id,
              created_by_user_id,
              idempotency_key
          )
          VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
          RETURNING *
        )
        "#,
        "FROM outreach",
    );
    sqlx::query(&sql)
        .bind(outreach.tenant_id)
        .bind(outreach.event_layer_id)
        .bind(outreach.application_id)
        .bind(outreach.recipient_email)
        .bind(outreach.subject)
        .bind(outreach.preview_text)
        .bind(outreach.body_markdown)
        .bind(outreach.reply_to_email)
        .bind(outreach.notes_doc_id)
        .bind(outreach.created_by_user_id)
        .bind(outreach.idempotency_key)
        .fetch_one(&mut **tx)
        .await
        .map_err(graphql_db_error)
}

async fn mark_outreach_sent(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    outreach_id: &str,
    resend_email_id: &str,
) -> async_graphql::Result<PgRow> {
    let outreach_uuid = parse_uuid(outreach_id, "outreachId")?;
    let sql = outreach_select_sql_with_prefix(
        r#"
        WITH outreach AS (
          UPDATE event_email_outreach
          SET delivery_state = 'sent',
              resend_email_id = $3,
              sent_at = now(),
              last_event_at = now()
          WHERE tenant_id = $1
            AND id = $2
          RETURNING *
        )
        "#,
        "FROM outreach",
    );
    sqlx::query(&sql)
        .bind(tenant_id)
        .bind(outreach_uuid)
        .bind(resend_email_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(graphql_db_error)
}

async fn mark_outreach_failed(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    outreach_id: &str,
) -> async_graphql::Result<()> {
    let outreach_uuid = parse_uuid(outreach_id, "outreachId")?;
    sqlx::query(
        r#"
        UPDATE event_email_outreach
        SET delivery_state = 'failed',
            last_event_at = now()
        WHERE tenant_id = $1
          AND id = $2
        "#,
    )
    .bind(tenant_id)
    .bind(outreach_uuid)
    .execute(&mut **tx)
    .await
    .map_err(graphql_db_error)?;
    Ok(())
}

async fn resolve_actor_uuid(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    actor_user_id: Option<&str>,
) -> async_graphql::Result<Option<Uuid>> {
    let actor_user_id = actor_user_id.unwrap_or(NO_LOGIN_PLANNER_ACTOR_ID);
    let actor_uuid = parse_uuid(actor_user_id, "actorUserId")?;
    let no_login_actor_uuid = parse_uuid(NO_LOGIN_PLANNER_ACTOR_ID, "actorUserId")?;
    if actor_uuid != no_login_actor_uuid {
        return Ok(Some(actor_uuid));
    }
    let row = sqlx::query(
        r#"
        INSERT INTO event_planner_users (tenant_id, email, display_name)
        VALUES ($1, $2, $3)
        ON CONFLICT (tenant_id, email)
        DO UPDATE SET display_name = EXCLUDED.display_name
        RETURNING id
        "#,
    )
    .bind(tenant_id)
    .bind(NO_LOGIN_PLANNER_EMAIL)
    .bind(NO_LOGIN_PLANNER_DISPLAY_NAME)
    .fetch_one(&mut **tx)
    .await
    .map_err(graphql_db_error)?;
    Ok(Some(row.get("id")))
}

fn actor_user_id(ctx: &Context<'_>) -> Option<String> {
    ctx.data_opt::<PlannerActor>()
        .map(|actor| actor.user_id.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn channel_select_sql(from_clause: &str) -> String {
    channel_select_sql_with_prefix("", from_clause)
}

fn channel_select_sql_with_prefix(prefix: &str, from_clause: &str) -> String {
    format!(
        r#"
        {prefix}
        SELECT id,
               event_layer_id,
               provider,
               sender_email,
               sender_name,
               reply_to_email,
               reply_routing_mode,
               delivery_webhook_status,
               provider_deployment_label,
               created_at,
               updated_at,
               version
        {from_clause}
        "#
    )
}

fn outreach_select_sql(from_clause: &str) -> String {
    outreach_select_sql_with_prefix("", from_clause)
}

fn outreach_select_sql_with_prefix(prefix: &str, from_clause: &str) -> String {
    format!(
        r#"
        {prefix}
        SELECT id,
               event_layer_id,
               application_id,
               recipient_email,
               subject,
               preview_text,
               resend_email_id,
               message_id,
               reply_to_email,
               delivery_state,
               reply_state,
               notes_doc_id,
               created_by_user_id,
               sent_at,
               last_event_at,
               created_at,
               updated_at,
               version
        {from_clause}
        "#
    )
}

fn channel_from_row(row: &PgRow) -> EventEmailChannel {
    EventEmailChannel {
        id: row.get::<Uuid, _>("id").to_string(),
        event_layer_id: row.get::<Uuid, _>("event_layer_id").to_string(),
        provider: row.get("provider"),
        sender_email: row.get("sender_email"),
        sender_name: row
            .try_get::<Option<String>, _>("sender_name")
            .ok()
            .flatten(),
        reply_to_email: row
            .try_get::<Option<String>, _>("reply_to_email")
            .ok()
            .flatten(),
        reply_routing_mode: EventEmailReplyRoutingMode::from_db(
            &row.get::<String, _>("reply_routing_mode"),
        ),
        delivery_webhook_status: row.get("delivery_webhook_status"),
        provider_deployment_label: row
            .try_get::<Option<String>, _>("provider_deployment_label")
            .ok()
            .flatten(),
        created_at: ts_iso(row, "created_at"),
        updated_at: ts_iso(row, "updated_at"),
        version: version_i32(row.try_get::<i64, _>("version").unwrap_or(1)),
    }
}

fn outreach_from_row(row: &PgRow) -> EventEmailOutreach {
    EventEmailOutreach {
        id: row.get::<Uuid, _>("id").to_string(),
        event_layer_id: row.get::<Uuid, _>("event_layer_id").to_string(),
        application_id: row
            .try_get::<Option<Uuid>, _>("application_id")
            .ok()
            .flatten()
            .map(|id| id.to_string()),
        recipient_email: row.get("recipient_email"),
        subject: row.get("subject"),
        preview_text: row
            .try_get::<Option<String>, _>("preview_text")
            .ok()
            .flatten(),
        resend_email_id: row
            .try_get::<Option<String>, _>("resend_email_id")
            .ok()
            .flatten(),
        message_id: row
            .try_get::<Option<String>, _>("message_id")
            .ok()
            .flatten(),
        reply_to_email: row
            .try_get::<Option<String>, _>("reply_to_email")
            .ok()
            .flatten(),
        delivery_state: EventEmailDeliveryState::from_db(&row.get::<String, _>("delivery_state")),
        reply_state: EventEmailReplyState::from_db(&row.get::<String, _>("reply_state")),
        notes_doc_id: row
            .try_get::<Option<String>, _>("notes_doc_id")
            .ok()
            .flatten(),
        created_by_user_id: row
            .try_get::<Option<Uuid>, _>("created_by_user_id")
            .ok()
            .flatten()
            .map(|id| id.to_string()),
        sent_at: ts_iso(row, "sent_at"),
        last_event_at: ts_iso(row, "last_event_at"),
        created_at: ts_iso(row, "created_at"),
        updated_at: ts_iso(row, "updated_at"),
        version: version_i32(row.try_get::<i64, _>("version").unwrap_or(1)),
    }
}

fn default_tenant_slug() -> String {
    env::var("CIVIC_ATLAS_DEFAULT_TENANT").unwrap_or_else(|_| "flint".to_string())
}

fn parse_uuid(value: &str, field_name: &str) -> async_graphql::Result<Uuid> {
    Uuid::parse_str(value.trim())
        .map_err(|_| async_graphql::Error::new(format!("{field_name} is not a valid UUID")))
}

fn optional_uuid(value: Option<&str>, field_name: &str) -> async_graphql::Result<Option<Uuid>> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| parse_uuid(value, field_name))
        .transpose()
}

fn nonempty_or(value: Option<&str>, default_value: &str) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default_value)
        .to_string()
}

fn clean_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn markdown_preview(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(180)
        .collect()
}

fn markdown_to_html(value: &str) -> String {
    value
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(|paragraph| format!("<p>{}</p>", escape_html(paragraph).replace('\n', "<br>")))
        .collect::<Vec<_>>()
        .join("")
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn sanitize_resend_tag_value(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                Some(ch)
            } else if ch.is_ascii_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .take(256)
        .collect();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

fn default_send_idempotency_key(
    application_id: Uuid,
    subject: &str,
    body_markdown: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(application_id.as_bytes());
    hasher.update(subject.as_bytes());
    hasher.update(body_markdown.as_bytes());
    let digest = hasher.finalize();
    format!(
        "event-application-email:{application_id}:{}",
        hex_prefix(&digest, 16)
    )
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .take(len)
        .map(|nibble| char::from(b"0123456789abcdef"[usize::from(nibble)]))
        .collect()
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
    async_graphql::Error::new(format!("event email PostGIS operation failed: {error}"))
}

fn truncate_error(value: &str) -> String {
    const LIMIT: usize = 512;
    let trimmed = value.trim();
    let truncated: String = trimmed.chars().take(LIMIT).collect();
    if truncated.len() == trimmed.len() {
        trimmed.to_string()
    } else {
        format!("{truncated}...")
    }
}

#[allow(dead_code)]
fn _keep_http_status_code_linked(_: HttpStatusCode) {}

#[allow(dead_code)]
fn _keep_sql_json_linked(_: SqlJson<Value>) {}

#[cfg(test)]
mod tests {
    use super::{
        default_send_idempotency_key, markdown_preview, markdown_to_html, sanitize_resend_tag_value,
    };
    use uuid::Uuid;

    #[test]
    fn email_text_helpers_keep_payloads_safe() {
        assert_eq!(markdown_preview("hello\n\nworld"), "hello world");
        assert_eq!(
            markdown_to_html("<hi>\nthere"),
            "<p>&lt;hi&gt;<br>there</p>"
        );
        assert_eq!(
            sanitize_resend_tag_value("Goods & Services"),
            "Goods--Services"
        );
    }

    #[test]
    fn default_idempotency_key_is_stable() {
        let id = Uuid::parse_str("00000000-0000-4000-8000-000000000111").unwrap();
        assert_eq!(
            default_send_idempotency_key(id, "Subject", "Body"),
            default_send_idempotency_key(id, "Subject", "Body")
        );
    }
}
