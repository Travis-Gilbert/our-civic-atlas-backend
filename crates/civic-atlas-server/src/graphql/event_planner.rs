//! GraphQL types and resolvers for the Porchfest/Event Planner surface.
//!
//! The database/RLS/write semantics already live in `EventPlannerGrpcService`.
//! This module is intentionally a thin async-graphql adapter: it maps the
//! frontend's checked-in GraphQL contract onto the existing in-process gRPC
//! service so production Axum `/graphql` no longer depends on the retired Node
//! sidecar for planner fields.

use async_graphql::{Context, InputObject, MaybeUndefined, Object, SimpleObject};
use chrono::{DateTime, Utc};
use civic_atlas_types::civic_atlas::v1::TenantContext;
use civic_atlas_types::event_planner::{
    EventLayerListRequest, EventPlannerService, PlacementCreateRequest, PlacementDeleteRequest,
    PlacementListRequest, PlacementMutationResponse, PlacementUpdateRequest, TaskCreateRequest,
    TaskDeleteRequest, TaskListRequest, TaskMutationResponse, TaskUpdateRequest,
};
use serde_json::{json, Value};
use tonic::Request;

use crate::event_planner::{EventPlannerGrpcService, NO_LOGIN_PLANNER_ACTOR_ID};
use crate::AtlasState;

#[derive(Clone)]
pub struct PlannerActor {
    pub user_id: String,
}

#[derive(SimpleObject, Default, Clone)]
pub struct EventLayer {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
}

#[derive(SimpleObject, Default, Clone)]
pub struct Placement {
    pub id: String,
    pub event_layer_id: String,
    pub category: String,
    pub sublabel: Option<String>,
    pub label: String,
    pub geometry: async_graphql::Json<Value>,
    pub owner_user_id: Option<String>,
    pub status: String,
    pub notes: Option<String>,
    pub version: i32,
}

#[derive(SimpleObject, Default, Clone)]
pub struct EventTask {
    pub id: String,
    pub event_layer_id: String,
    pub title: String,
    pub owner_display: Option<String>,
    pub due_at: Option<String>,
    pub status: String,
    pub placement_id: Option<String>,
    pub notes: Option<String>,
    pub version: i32,
}

#[derive(SimpleObject, Default, Clone)]
pub struct PlacementMutationResult {
    pub placement: Option<Placement>,
    pub stale_write: bool,
    pub deleted: bool,
}

#[derive(SimpleObject, Default, Clone)]
pub struct TaskMutationResult {
    pub task: Option<EventTask>,
    pub stale_write: bool,
    pub deleted: bool,
}

#[derive(InputObject)]
pub struct PlacementCreateInput {
    pub event_slug: String,
    pub category: String,
    pub sublabel: Option<String>,
    pub label: String,
    pub geometry: async_graphql::Json<Value>,
    pub status: Option<String>,
    pub notes: Option<String>,
}

#[derive(InputObject)]
pub struct PlacementUpdateInput {
    pub placement_id: String,
    pub expected_version: i32,
    pub category: MaybeUndefined<String>,
    pub sublabel: MaybeUndefined<String>,
    pub label: MaybeUndefined<String>,
    pub geometry: MaybeUndefined<async_graphql::Json<Value>>,
    pub status: MaybeUndefined<String>,
    pub notes: MaybeUndefined<String>,
}

#[derive(InputObject)]
pub struct PlacementDeleteInput {
    pub placement_id: String,
    pub expected_version: i32,
}

#[derive(InputObject)]
pub struct TaskCreateInput {
    pub event_slug: String,
    pub title: String,
    pub owner_user_id: Option<String>,
    pub due_at: Option<String>,
    pub status: Option<String>,
    pub placement_id: Option<String>,
    pub notes: Option<String>,
}

#[derive(InputObject)]
pub struct TaskUpdateInput {
    pub task_id: String,
    pub expected_version: i32,
    pub title: MaybeUndefined<String>,
    pub owner_user_id: MaybeUndefined<String>,
    pub due_at: MaybeUndefined<String>,
    pub status: MaybeUndefined<String>,
    pub placement_id: MaybeUndefined<String>,
    pub notes: MaybeUndefined<String>,
}

#[derive(InputObject)]
pub struct TaskDeleteInput {
    pub task_id: String,
    pub expected_version: i32,
}

pub struct EventPlannerQuery;

#[Object]
impl EventPlannerQuery {
    async fn event_layers(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = "flint")] tenant_slug: String,
    ) -> async_graphql::Result<Vec<EventLayer>> {
        let service = service(ctx)?;
        let response = service
            .list_event_layers(Request::new(EventLayerListRequest {
                tenant_context: Some(tenant_context(&tenant_slug)),
            }))
            .await
            .map_err(graphql_status)?
            .into_inner();

        Ok(response.layers.into_iter().map(map_event_layer).collect())
    }

    async fn placements(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = "flint")] tenant_slug: String,
        event_slug: String,
    ) -> async_graphql::Result<Vec<Placement>> {
        let service = service(ctx)?;
        let response = service
            .list_placements(Request::new(PlacementListRequest {
                tenant_context: Some(tenant_context(&tenant_slug)),
                event_layer_slug: event_slug,
            }))
            .await
            .map_err(graphql_status)?
            .into_inner();

        Ok(response.placements.into_iter().map(map_placement).collect())
    }

    async fn event_tasks(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = "flint")] tenant_slug: String,
        event_slug: String,
    ) -> async_graphql::Result<Vec<EventTask>> {
        let service = service(ctx)?;
        let response = service
            .list_tasks(Request::new(TaskListRequest {
                tenant_context: Some(tenant_context(&tenant_slug)),
                event_layer_slug: event_slug,
            }))
            .await
            .map_err(graphql_status)?
            .into_inner();

        Ok(response.tasks.into_iter().map(map_task).collect())
    }
}

pub struct EventPlannerMutation;

#[Object]
impl EventPlannerMutation {
    async fn create_placement(
        &self,
        ctx: &Context<'_>,
        input: PlacementCreateInput,
    ) -> async_graphql::Result<PlacementMutationResult> {
        let actor_user_id = actor_user_id(ctx);
        let response = service(ctx)?
            .create_placement(Request::new(PlacementCreateRequest {
                tenant_context: Some(default_tenant_context()),
                event_layer_slug: input.event_slug,
                category: input.category,
                sublabel: input.sublabel.unwrap_or_default(),
                label: input.label,
                geometry_geojson: json_to_string(input.geometry),
                status: input.status.unwrap_or_default(),
                notes: input.notes.unwrap_or_default(),
                actor_user_id,
            }))
            .await
            .map_err(graphql_status)?
            .into_inner();

        Ok(map_placement_mutation(response))
    }

    async fn update_placement(
        &self,
        ctx: &Context<'_>,
        input: PlacementUpdateInput,
    ) -> async_graphql::Result<PlacementMutationResult> {
        let actor_user_id = actor_user_id(ctx);
        let category = field_update(input.category, stringify);
        let sublabel = field_update(input.sublabel, stringify);
        let label = field_update(input.label, stringify);
        let geometry = field_update(input.geometry, json_to_string);
        let status = field_update(input.status, stringify);
        let notes = field_update(input.notes, stringify);

        let response = service(ctx)?
            .update_placement(Request::new(PlacementUpdateRequest {
                tenant_context: Some(default_tenant_context()),
                placement_id: input.placement_id,
                expected_version: i64::from(input.expected_version),
                category: category.value,
                category_present: category.present,
                sublabel: sublabel.value,
                sublabel_present: sublabel.present,
                label: label.value,
                label_present: label.present,
                geometry_geojson: geometry.value,
                geometry_present: geometry.present,
                status: status.value,
                status_present: status.present,
                notes: notes.value,
                notes_present: notes.present,
                actor_user_id,
            }))
            .await
            .map_err(graphql_status)?
            .into_inner();

        Ok(map_placement_mutation(response))
    }

    async fn delete_placement(
        &self,
        ctx: &Context<'_>,
        input: PlacementDeleteInput,
    ) -> async_graphql::Result<PlacementMutationResult> {
        let actor_user_id = actor_user_id(ctx);
        let response = service(ctx)?
            .delete_placement(Request::new(PlacementDeleteRequest {
                tenant_context: Some(default_tenant_context()),
                placement_id: input.placement_id,
                expected_version: i64::from(input.expected_version),
                actor_user_id,
            }))
            .await
            .map_err(graphql_status)?
            .into_inner();

        Ok(map_placement_mutation(response))
    }

    async fn create_task(
        &self,
        ctx: &Context<'_>,
        input: TaskCreateInput,
    ) -> async_graphql::Result<TaskMutationResult> {
        let actor_user_id = actor_user_id(ctx);
        let response = service(ctx)?
            .create_task(Request::new(TaskCreateRequest {
                tenant_context: Some(default_tenant_context()),
                event_layer_slug: input.event_slug,
                title: input.title,
                owner_user_id: input.owner_user_id.unwrap_or_default(),
                due_at_ms: iso_to_ms(input.due_at.as_deref()),
                status: input.status.unwrap_or_default(),
                placement_id: input.placement_id.unwrap_or_default(),
                notes: input.notes.unwrap_or_default(),
                actor_user_id,
            }))
            .await
            .map_err(graphql_status)?
            .into_inner();

        Ok(map_task_mutation(response))
    }

    async fn update_task(
        &self,
        ctx: &Context<'_>,
        input: TaskUpdateInput,
    ) -> async_graphql::Result<TaskMutationResult> {
        let actor_user_id = actor_user_id(ctx);
        let title = field_update(input.title, stringify);
        let owner = field_update(input.owner_user_id, stringify);
        let due_at = field_update(input.due_at, |value| iso_to_ms(Some(&value)));
        let status = field_update(input.status, stringify);
        let placement = field_update(input.placement_id, stringify);
        let notes = field_update(input.notes, stringify);

        let response = service(ctx)?
            .update_task(Request::new(TaskUpdateRequest {
                tenant_context: Some(default_tenant_context()),
                task_id: input.task_id,
                expected_version: i64::from(input.expected_version),
                title: title.value,
                title_present: title.present,
                owner_user_id: owner.value,
                owner_present: owner.present,
                due_at_ms: due_at.value,
                due_at_present: due_at.present,
                status: status.value,
                status_present: status.present,
                placement_id: placement.value,
                placement_present: placement.present,
                notes: notes.value,
                notes_present: notes.present,
                actor_user_id,
            }))
            .await
            .map_err(graphql_status)?
            .into_inner();

        Ok(map_task_mutation(response))
    }

    async fn delete_task(
        &self,
        ctx: &Context<'_>,
        input: TaskDeleteInput,
    ) -> async_graphql::Result<TaskMutationResult> {
        let actor_user_id = actor_user_id(ctx);
        let response = service(ctx)?
            .delete_task(Request::new(TaskDeleteRequest {
                tenant_context: Some(default_tenant_context()),
                task_id: input.task_id,
                expected_version: i64::from(input.expected_version),
                actor_user_id,
            }))
            .await
            .map_err(graphql_status)?
            .into_inner();

        Ok(map_task_mutation(response))
    }
}

impl Default for EventPlannerQuery {
    fn default() -> Self {
        Self
    }
}

impl Default for EventPlannerMutation {
    fn default() -> Self {
        Self
    }
}

struct FieldUpdate<T> {
    present: bool,
    value: T,
}

fn field_update<T, U, F>(field: MaybeUndefined<T>, convert: F) -> FieldUpdate<U>
where
    U: Default,
    F: FnOnce(T) -> U,
{
    match field {
        MaybeUndefined::Undefined => FieldUpdate {
            present: false,
            value: U::default(),
        },
        MaybeUndefined::Null => FieldUpdate {
            present: true,
            value: U::default(),
        },
        MaybeUndefined::Value(value) => FieldUpdate {
            present: true,
            value: convert(value),
        },
    }
}

fn service(ctx: &Context<'_>) -> async_graphql::Result<EventPlannerGrpcService> {
    let state = ctx
        .data::<AtlasState>()
        .map_err(|_| async_graphql::Error::new("AtlasState missing from GraphQL context"))?;
    Ok(EventPlannerGrpcService::new(state.clone()))
}

fn actor_user_id(ctx: &Context<'_>) -> String {
    let actor = ctx
        .data_opt::<PlannerActor>()
        .map(|actor| actor.user_id.trim())
        .filter(|actor| !actor.is_empty());
    actor.unwrap_or(NO_LOGIN_PLANNER_ACTOR_ID).to_string()
}

fn tenant_context(tenant_slug: &str) -> TenantContext {
    TenantContext {
        tenant_id: nonempty_or_default(tenant_slug, "flint"),
        ..Default::default()
    }
}

fn default_tenant_context() -> TenantContext {
    let tenant =
        std::env::var("CIVIC_ATLAS_DEFAULT_TENANT").unwrap_or_else(|_| "flint".to_string());
    tenant_context(&tenant)
}

fn nonempty_or_default(value: &str, default_value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default_value.to_string()
    } else {
        trimmed.to_string()
    }
}

fn stringify(value: String) -> String {
    value
}

fn json_to_string(value: async_graphql::Json<Value>) -> String {
    value.0.to_string()
}

fn geometry_json(value: &str) -> async_graphql::Json<Value> {
    match serde_json::from_str::<Value>(value) {
        Ok(value) if value.is_object() => async_graphql::Json(value),
        _ => async_graphql::Json(json!({})),
    }
}

fn timestamp_to_iso(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    let secs = ms / 1_000;
    let nanos = ((ms % 1_000).abs() as u32) * 1_000_000;
    DateTime::<Utc>::from_timestamp(secs, nanos).map(|dt| dt.to_rfc3339())
}

fn iso_to_ms(value: Option<&str>) -> i64 {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return 0;
    };
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

fn version_i32(version: i64) -> i32 {
    i32::try_from(version).unwrap_or(i32::MAX)
}

fn graphql_status(status: tonic::Status) -> async_graphql::Error {
    async_graphql::Error::new(format!("EventPlannerService failed: {status}"))
}

fn map_event_layer(layer: civic_atlas_types::event_planner::EventLayer) -> EventLayer {
    EventLayer {
        id: layer.id,
        slug: layer.slug,
        title: layer.title,
        starts_at: timestamp_to_iso(layer.starts_at_ms),
        ends_at: timestamp_to_iso(layer.ends_at_ms),
    }
}

fn map_placement(placement: civic_atlas_types::event_planner::Placement) -> Placement {
    Placement {
        id: placement.id,
        event_layer_id: placement.event_layer_id,
        category: placement.category,
        sublabel: empty_to_none(placement.sublabel),
        label: placement.label,
        geometry: geometry_json(&placement.geometry_geojson),
        owner_user_id: empty_to_none(placement.owner_user_id),
        status: placement.status,
        notes: empty_to_none(placement.notes),
        version: version_i32(placement.version),
    }
}

fn map_task(task: civic_atlas_types::event_planner::Task) -> EventTask {
    EventTask {
        id: task.id,
        event_layer_id: task.event_layer_id,
        title: task.title,
        owner_display: empty_to_none(task.owner_display),
        due_at: timestamp_to_iso(task.due_at_ms),
        status: task.status,
        placement_id: empty_to_none(task.placement_id),
        notes: empty_to_none(task.notes),
        version: version_i32(task.version),
    }
}

fn map_placement_mutation(response: PlacementMutationResponse) -> PlacementMutationResult {
    PlacementMutationResult {
        placement: response.placement.map(map_placement),
        stale_write: response.stale_write,
        deleted: response.deleted,
    }
}

fn map_task_mutation(response: TaskMutationResponse) -> TaskMutationResult {
    TaskMutationResult {
        task: response.task.map(map_task),
        stale_write: response.stale_write,
        deleted: response.deleted,
    }
}

fn empty_to_none(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(value)
    }
}
