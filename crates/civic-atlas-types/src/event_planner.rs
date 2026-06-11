//! Convenience re-exports for the EventPlannerService proto types.
//!
//! All `event_planner.proto` messages live in the `civic_atlas.v1`
//! package, so they are already pulled in by the workspace
//! `tonic::include_proto!("civic_atlas.v1")` call in `lib.rs`. This
//! module gives the planner code a focused import path so consumers
//! can write `use civic_atlas_types::event_planner::{EventLayer, ...}`
//! without dragging in every other v1 symbol.
//!
//! Add new domain (non-wire) helper types here as the planner grows.
//! Keep wire types in the proto file.

pub use crate::civic_atlas::v1::{
    event_planner_service_server::{EventPlannerService, EventPlannerServiceServer},
    BookmarkCreateRequest, BookmarkDeleteRequest, BookmarkListRequest, BookmarkListResponse,
    BookmarkMutationResponse, BookmarkUpdateRequest, CameraBookmark, EventApplication,
    EventApplicationListRequest, EventApplicationListResponse, EventApplicationSubmitRequest,
    EventApplicationSubmitResponse, EventLayer, EventLayerListRequest, EventLayerListResponse,
    IntakePendingVendorRequest, IntakePendingVendorResponse, Placement, PlacementCreateRequest,
    PlacementDeleteRequest, PlacementListRequest, PlacementListResponse, PlacementMutationResponse,
    PlacementNote, PlacementNoteCreateRequest, PlacementNoteDeleteRequest,
    PlacementNoteListRequest, PlacementNoteListResponse, PlacementNoteMutationResponse,
    PlacementUpdateRequest, Task, TaskCreateRequest, TaskDeleteRequest, TaskListRequest,
    TaskListResponse, TaskMutationResponse, TaskUpdateRequest,
};
