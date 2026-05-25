/**
 * Porchfest Planner GraphQL schema module.
 *
 * Phase 1 read-only surface for the EventPlanner: three queries that
 * resolve through `EventPlannerGrpcClient` (which dials the Axum
 * `EventPlannerService`). Mounted into the main schema via
 * `apps/graphql-server/src/schema.ts`.
 *
 * The module exports the GraphQL `typeDefs` (string) and `resolvers`
 * (object) for `createSchema` to merge. Mutations land here in Phase
 * 2; day-of-event status moves in Phase 3.
 *
 * `tenantSlug` rides on every query because the Axum side validates
 * `TenantContext` per request and resolves the slug to a `tenants.id`
 * uuid before binding the RLS GUC. Defaulting it to "flint" lets the
 * Phase 1 frontend call the queries without threading auth state.
 */

import type { TenantContext } from "../../grpcClient.js";
import type {
  EventPlannerGrpcClient,
  EventLayer,
  Placement,
  PlannerTask,
  PlacementMutationResult,
  TaskMutationResult,
  PlacementNote,
  PlacementNoteMutationResult,
  CameraBookmark,
  BookmarkMutationResult,
} from "../../grpcClient.js";

export const eventPlannerTypeDefs = /* GraphQL */ `
  """
  A one-off civic event surface (e.g., Carriage Town Porchfest 2026)
  that the planner manages a map of placements and a back-of-house
  task list against.
  """
  type EventLayer {
    id: ID!
    slug: String!
    title: String!
    """Event start, ISO 8601. Null when not yet scheduled."""
    startsAt: DateTime
    """Event end, ISO 8601. Null when not yet scheduled."""
    endsAt: DateTime
  }

  """
  A single thing dropped onto an event layer's map: a vendor pop-up,
  a music porch, a parking pin, an amenity. Geometry is a GeoJSON
  blob (Point in practice for Phase 1, but the column accepts any
  geometry so polygons can layer in without a migration).
  """
  type Placement {
    id: ID!
    eventLayerId: ID!
    """
    Free-form category populated by the importer. Phase 1 expected
    values: vendor, music, parking, restroom, kid_zone, food_court,
    rest_area, after_party, amenity. Phase 2 may promote to an enum.
    """
    category: String!
    """Secondary label (vendor type, music genre). Empty when unset."""
    sublabel: String
    label: String!
    """GeoJSON geometry (typically Point)."""
    geometry: GeoJSON!
    """Empty string when the placement has no owner yet."""
    ownerUserId: ID
    """Free-form lifecycle status. Defaults to 'placed'."""
    status: String!
    """Importer / moderator notes. May contain TODO_CATEGORY when the
    KML importer couldn't classify the icon."""
    notes: String
    """
    Monotonically increasing row version. Clients echo this back on
    update mutations as expectedVersion; the server returns
    staleWrite=true and the server's current row when the version
    mismatches (another planner won the race).
    """
    version: Int!
  }

  """
  Back-of-house task for an event. Phase 1 lists tasks but does not
  write them; Phase 2 wires assignment and status moves.
  """
  type EventTask {
    id: ID!
    eventLayerId: ID!
    title: String!
    """Display string for the assignee. Phase 2 will resolve to user."""
    ownerDisplay: String
    dueAt: DateTime
    """Free-form status. Defaults to 'open'."""
    status: String!
    """Optional placement this task is tied to."""
    placementId: ID
    notes: String
    version: Int!
  }

  extend type Query {
    """Every event layer for the tenant. Sorted earliest start first."""
    eventLayers(tenantSlug: String! = "flint"): [EventLayer!]!

    """Every placement on a given event layer, scoped by tenant."""
    placements(
      tenantSlug: String! = "flint"
      eventSlug: String!
    ): [Placement!]!

    """
    Every task on a given event layer. Open tasks first, then by
    due date.
    """
    eventTasks(
      tenantSlug: String! = "flint"
      eventSlug: String!
    ): [EventTask!]!
  }

  """
  Result of any placement mutation. The placement field carries the
  current server-side row (post-mutation on success, server's wins
  on stale write). staleWrite flags optimistic-concurrency races so
  the client can toast and refetch. deleted is true only on
  successful delete.
  """
  type PlacementMutationResult {
    placement: Placement
    staleWrite: Boolean!
    deleted: Boolean!
  }

  type TaskMutationResult {
    task: EventTask
    staleWrite: Boolean!
    deleted: Boolean!
  }

  """
  Optional fields are only applied when present in the input. To
  clear a nullable text field, send an empty string; to leave it
  unchanged, omit the key entirely.
  """
  input PlacementCreateInput {
    eventSlug: String!
    category: String!
    sublabel: String
    label: String!
    geometry: GeoJSON!
    status: String
    notes: String
  }

  input PlacementUpdateInput {
    placementId: ID!
    expectedVersion: Int!
    category: String
    sublabel: String
    label: String
    geometry: GeoJSON
    status: String
    notes: String
  }

  input PlacementDeleteInput {
    placementId: ID!
    expectedVersion: Int!
  }

  input TaskCreateInput {
    eventSlug: String!
    title: String!
    ownerUserId: ID
    dueAt: DateTime
    status: String
    placementId: ID
    notes: String
  }

  input TaskUpdateInput {
    taskId: ID!
    expectedVersion: Int!
    title: String
    ownerUserId: ID
    """Pass null to clear, omit to leave unchanged, or pass an ISO 8601 datetime to set."""
    dueAt: DateTime
    status: String
    placementId: ID
    notes: String
  }

  input TaskDeleteInput {
    taskId: ID!
    expectedVersion: Int!
  }

  # ---- Phase 3: notes ----

  """
  Threaded note attached to a placement. Append-only in Phase 3; the
  author can delete their own notes via deletePlacementNote.
  """
  type PlacementNote {
    id: ID!
    placementId: ID!
    eventLayerId: ID!
    authorUserId: ID!
    authorDisplay: String!
    body: String!
    createdAt: DateTime!
    version: Int!
  }

  type PlacementNoteMutationResult {
    note: PlacementNote
    deleted: Boolean!
  }

  input CreatePlacementNoteInput {
    placementId: ID!
    body: String!
  }

  input DeletePlacementNoteInput {
    noteId: ID!
  }

  # ---- Phase 3: camera bookmarks ----

  type CameraBookmark {
    id: ID!
    eventLayerId: ID!
    name: String!
    centerLng: Float!
    centerLat: Float!
    zoom: Float!
    pitch: Float!
    bearing: Float!
    createdByUserId: ID
    createdAt: DateTime!
    version: Int!
  }

  type BookmarkMutationResult {
    bookmark: CameraBookmark
    staleWrite: Boolean!
    deleted: Boolean!
  }

  input BookmarkCreateInput {
    eventSlug: String!
    name: String!
    centerLng: Float!
    centerLat: Float!
    zoom: Float!
    pitch: Float = 0
    bearing: Float = 0
  }

  input BookmarkUpdateInput {
    bookmarkId: ID!
    expectedVersion: Int!
    name: String
    centerLng: Float
    centerLat: Float
    zoom: Float
    pitch: Float
    bearing: Float
  }

  input BookmarkDeleteInput {
    bookmarkId: ID!
    expectedVersion: Int!
  }

  extend type Query {
    placementNotes(
      tenantSlug: String! = "flint"
      placementId: ID!
    ): [PlacementNote!]!

    cameraBookmarks(
      tenantSlug: String! = "flint"
      eventSlug: String!
    ): [CameraBookmark!]!
  }

  extend type Mutation {
    """
    Create a new placement. Requires a signed-in planner session
    cookie. Status defaults to 'placed' when omitted.
    """
    createPlacement(input: PlacementCreateInput!): PlacementMutationResult!
    updatePlacement(input: PlacementUpdateInput!): PlacementMutationResult!
    deletePlacement(input: PlacementDeleteInput!): PlacementMutationResult!

    createTask(input: TaskCreateInput!): TaskMutationResult!
    updateTask(input: TaskUpdateInput!): TaskMutationResult!
    deleteTask(input: TaskDeleteInput!): TaskMutationResult!

    createPlacementNote(input: CreatePlacementNoteInput!): PlacementNoteMutationResult!
    deletePlacementNote(input: DeletePlacementNoteInput!): PlacementNoteMutationResult!

    createBookmark(input: BookmarkCreateInput!): BookmarkMutationResult!
    updateBookmark(input: BookmarkUpdateInput!): BookmarkMutationResult!
    deleteBookmark(input: BookmarkDeleteInput!): BookmarkMutationResult!
  }
`;

/* ------------------------------------------------------------------ */
/*  Resolvers                                                          */
/* ------------------------------------------------------------------ */

/**
 * Resolvers expect a `GraphqlContext` that exposes `eventPlanner` (a
 * live `EventPlannerGrpcClient`) and a default tenant. The main
 * schema module attaches these to the context per-request.
 */
export interface EventPlannerContext {
  readonly eventPlanner: EventPlannerGrpcClient;
  readonly defaultTenant: TenantContext;
  /**
   * Active planner user id resolved from the session cookie, if any.
   * Mutations require this; reads tolerate a missing actor.
   */
  readonly actorUserId: string | null;
}

function tenantContextFor(
  context: EventPlannerContext,
  tenantSlug: string,
): TenantContext {
  // Today the Axum side resolves any slug back to its uuid before
  // binding RLS, so we forward what the caller asked for and let the
  // server fail clean with `unknown tenant: <slug>` if it isn't real.
  // The defaultTenant is still useful when callers omit the arg (the
  // schema default is "flint" but a future tenant.atlasNodeId hint
  // would ride on context.defaultTenant).
  if (!tenantSlug || tenantSlug === context.defaultTenant.tenantId) {
    return context.defaultTenant;
  }
  return { tenantId: tenantSlug };
}

function timestampToIso(ms: number): string | null {
  // The wire contract uses `0` for "unset". The schema scalar is
  // DateTime (ISO 8601 string), so map 0 -> null.
  if (!ms || ms <= 0) return null;
  return new Date(ms).toISOString();
}

function parseGeoJson(value: string): Record<string, unknown> {
  // Defensive parse: the Axum side promises a valid GeoJSON string,
  // but a malformed row should surface as an empty Feature rather
  // than crash the query. Returning an empty object lets the
  // GeoJSON scalar serialize without throwing.
  if (!value) return {};
  try {
    const parsed = JSON.parse(value) as unknown;
    if (parsed && typeof parsed === "object") {
      return parsed as Record<string, unknown>;
    }
    return {};
  } catch {
    return {};
  }
}

function eventLayerToGraphql(layer: EventLayer) {
  return {
    id: layer.id,
    slug: layer.slug,
    title: layer.title,
    startsAt: timestampToIso(layer.startsAtMs),
    endsAt: timestampToIso(layer.endsAtMs),
  };
}

function placementToGraphql(placement: Placement) {
  return {
    id: placement.id,
    eventLayerId: placement.eventLayerId,
    category: placement.category,
    sublabel: placement.sublabel || null,
    label: placement.label,
    geometry: parseGeoJson(placement.geometryGeojson),
    ownerUserId: placement.ownerUserId || null,
    status: placement.status,
    notes: placement.notes || null,
    version: Number(placement.version ?? 1),
  };
}

function taskToGraphql(task: PlannerTask) {
  return {
    id: task.id,
    eventLayerId: task.eventLayerId,
    title: task.title,
    ownerDisplay: task.ownerDisplay || null,
    dueAt: timestampToIso(task.dueAtMs),
    status: task.status,
    placementId: task.placementId || null,
    notes: task.notes || null,
    version: Number(task.version ?? 1),
  };
}

function placementMutationToGraphql(result: PlacementMutationResult) {
  return {
    placement: result.placement ? placementToGraphql(result.placement) : null,
    staleWrite: result.staleWrite,
    deleted: result.deleted,
  };
}

function taskMutationToGraphql(result: TaskMutationResult) {
  return {
    task: result.task ? taskToGraphql(result.task) : null,
    staleWrite: result.staleWrite,
    deleted: result.deleted,
  };
}

function noteToGraphql(note: PlacementNote) {
  return {
    id: note.id,
    placementId: note.placementId,
    eventLayerId: note.eventLayerId,
    authorUserId: note.authorUserId,
    authorDisplay: note.authorDisplay,
    body: note.body,
    createdAt:
      timestampToIso(note.createdAtMs) ?? new Date(0).toISOString(),
    version: Number(note.version ?? 1),
  };
}

function noteMutationToGraphql(result: PlacementNoteMutationResult) {
  return {
    note: result.note ? noteToGraphql(result.note) : null,
    deleted: result.deleted,
  };
}

function bookmarkToGraphql(bookmark: CameraBookmark) {
  return {
    id: bookmark.id,
    eventLayerId: bookmark.eventLayerId,
    name: bookmark.name,
    centerLng: Number(bookmark.centerLng),
    centerLat: Number(bookmark.centerLat),
    zoom: Number(bookmark.zoom),
    pitch: Number(bookmark.pitch),
    bearing: Number(bookmark.bearing),
    createdByUserId: bookmark.createdByUserId || null,
    createdAt:
      timestampToIso(bookmark.createdAtMs) ?? new Date(0).toISOString(),
    version: Number(bookmark.version ?? 1),
  };
}

function bookmarkMutationToGraphql(result: BookmarkMutationResult) {
  return {
    bookmark: result.bookmark ? bookmarkToGraphql(result.bookmark) : null,
    staleWrite: result.staleWrite,
    deleted: result.deleted,
  };
}

function requireActor(context: EventPlannerContext): string {
  const actor = context.actorUserId;
  if (!actor) {
    throw new Error("This mutation requires a signed-in planner session.");
  }
  return actor;
}

function dueAtMsFromInput(value: string | null | undefined): number {
  if (value === null || value === undefined) return 0;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

function geometryToString(geom: Record<string, unknown> | null | undefined): string {
  if (!geom) return "";
  return JSON.stringify(geom);
}

export const eventPlannerResolvers = {
  Query: {
    eventLayers: async (
      _parent: unknown,
      args: { readonly tenantSlug: string },
      context: EventPlannerContext,
    ) => {
      const tenant = tenantContextFor(context, args.tenantSlug);
      const layers = await context.eventPlanner.listEventLayers(tenant);
      return layers.map(eventLayerToGraphql);
    },
    placements: async (
      _parent: unknown,
      args: { readonly tenantSlug: string; readonly eventSlug: string },
      context: EventPlannerContext,
    ) => {
      const tenant = tenantContextFor(context, args.tenantSlug);
      const placements = await context.eventPlanner.listPlacements(
        tenant,
        args.eventSlug,
      );
      return placements.map(placementToGraphql);
    },
    eventTasks: async (
      _parent: unknown,
      args: { readonly tenantSlug: string; readonly eventSlug: string },
      context: EventPlannerContext,
    ) => {
      const tenant = tenantContextFor(context, args.tenantSlug);
      const tasks = await context.eventPlanner.listTasks(
        tenant,
        args.eventSlug,
      );
      return tasks.map(taskToGraphql);
    },
    placementNotes: async (
      _parent: unknown,
      args: { readonly tenantSlug: string; readonly placementId: string },
      context: EventPlannerContext,
    ) => {
      const tenant = tenantContextFor(context, args.tenantSlug);
      const notes = await context.eventPlanner.listPlacementNotes(
        tenant,
        args.placementId,
      );
      return notes.map(noteToGraphql);
    },
    cameraBookmarks: async (
      _parent: unknown,
      args: { readonly tenantSlug: string; readonly eventSlug: string },
      context: EventPlannerContext,
    ) => {
      const tenant = tenantContextFor(context, args.tenantSlug);
      const bookmarks = await context.eventPlanner.listBookmarks(
        tenant,
        args.eventSlug,
      );
      return bookmarks.map(bookmarkToGraphql);
    },
  },
  Mutation: {
    createPlacement: async (
      _parent: unknown,
      args: {
        readonly input: {
          readonly eventSlug: string;
          readonly category: string;
          readonly sublabel?: string | null;
          readonly label: string;
          readonly geometry: Record<string, unknown>;
          readonly status?: string | null;
          readonly notes?: string | null;
        };
      },
      context: EventPlannerContext,
    ) => {
      const actorUserId = requireActor(context);
      const result = await context.eventPlanner.createPlacement(
        context.defaultTenant,
        {
          eventLayerSlug: args.input.eventSlug,
          category: args.input.category,
          sublabel: args.input.sublabel ?? "",
          label: args.input.label,
          geometryGeojson: geometryToString(args.input.geometry),
          status: args.input.status ?? "",
          notes: args.input.notes ?? "",
          actorUserId,
        },
      );
      return placementMutationToGraphql(result);
    },
    updatePlacement: async (
      _parent: unknown,
      args: {
        readonly input: {
          readonly placementId: string;
          readonly expectedVersion: number;
          readonly category?: string | null;
          readonly sublabel?: string | null;
          readonly label?: string | null;
          readonly geometry?: Record<string, unknown> | null;
          readonly status?: string | null;
          readonly notes?: string | null;
        };
      },
      context: EventPlannerContext,
    ) => {
      const actorUserId = requireActor(context);
      // Translate "key present in input" into the *_present flags by
      // selectively populating the gRPC input object. Only forward
      // keys the caller actually included.
      const input: Record<string, unknown> = {
        placementId: args.input.placementId,
        expectedVersion: args.input.expectedVersion,
        actorUserId,
      };
      if (Object.prototype.hasOwnProperty.call(args.input, "category"))
        input.category = args.input.category ?? "";
      if (Object.prototype.hasOwnProperty.call(args.input, "sublabel"))
        input.sublabel = args.input.sublabel ?? "";
      if (Object.prototype.hasOwnProperty.call(args.input, "label"))
        input.label = args.input.label ?? "";
      if (Object.prototype.hasOwnProperty.call(args.input, "geometry"))
        input.geometryGeojson = geometryToString(args.input.geometry ?? null);
      if (Object.prototype.hasOwnProperty.call(args.input, "status"))
        input.status = args.input.status ?? "";
      if (Object.prototype.hasOwnProperty.call(args.input, "notes"))
        input.notes = args.input.notes ?? "";
      const result = await context.eventPlanner.updatePlacement(
        context.defaultTenant,
        input as unknown as Parameters<EventPlannerGrpcClient["updatePlacement"]>[1],
      );
      return placementMutationToGraphql(result);
    },
    deletePlacement: async (
      _parent: unknown,
      args: {
        readonly input: { readonly placementId: string; readonly expectedVersion: number };
      },
      context: EventPlannerContext,
    ) => {
      const actorUserId = requireActor(context);
      const result = await context.eventPlanner.deletePlacement(
        context.defaultTenant,
        {
          placementId: args.input.placementId,
          expectedVersion: args.input.expectedVersion,
          actorUserId,
        },
      );
      return placementMutationToGraphql(result);
    },
    createTask: async (
      _parent: unknown,
      args: {
        readonly input: {
          readonly eventSlug: string;
          readonly title: string;
          readonly ownerUserId?: string | null;
          readonly dueAt?: string | null;
          readonly status?: string | null;
          readonly placementId?: string | null;
          readonly notes?: string | null;
        };
      },
      context: EventPlannerContext,
    ) => {
      const actorUserId = requireActor(context);
      const result = await context.eventPlanner.createTask(
        context.defaultTenant,
        {
          eventLayerSlug: args.input.eventSlug,
          title: args.input.title,
          ownerUserId: args.input.ownerUserId ?? "",
          dueAtMs: dueAtMsFromInput(args.input.dueAt),
          status: args.input.status ?? "",
          placementId: args.input.placementId ?? "",
          notes: args.input.notes ?? "",
          actorUserId,
        },
      );
      return taskMutationToGraphql(result);
    },
    updateTask: async (
      _parent: unknown,
      args: {
        readonly input: {
          readonly taskId: string;
          readonly expectedVersion: number;
          readonly title?: string | null;
          readonly ownerUserId?: string | null;
          readonly dueAt?: string | null;
          readonly status?: string | null;
          readonly placementId?: string | null;
          readonly notes?: string | null;
        };
      },
      context: EventPlannerContext,
    ) => {
      const actorUserId = requireActor(context);
      const input: Record<string, unknown> = {
        taskId: args.input.taskId,
        expectedVersion: args.input.expectedVersion,
        actorUserId,
      };
      if (Object.prototype.hasOwnProperty.call(args.input, "title"))
        input.title = args.input.title ?? "";
      if (Object.prototype.hasOwnProperty.call(args.input, "ownerUserId"))
        input.ownerUserId = args.input.ownerUserId ?? "";
      if (Object.prototype.hasOwnProperty.call(args.input, "dueAt"))
        input.dueAtMs = dueAtMsFromInput(args.input.dueAt);
      if (Object.prototype.hasOwnProperty.call(args.input, "status"))
        input.status = args.input.status ?? "";
      if (Object.prototype.hasOwnProperty.call(args.input, "placementId"))
        input.placementId = args.input.placementId ?? "";
      if (Object.prototype.hasOwnProperty.call(args.input, "notes"))
        input.notes = args.input.notes ?? "";
      const result = await context.eventPlanner.updateTask(
        context.defaultTenant,
        input as unknown as Parameters<EventPlannerGrpcClient["updateTask"]>[1],
      );
      return taskMutationToGraphql(result);
    },
    deleteTask: async (
      _parent: unknown,
      args: {
        readonly input: { readonly taskId: string; readonly expectedVersion: number };
      },
      context: EventPlannerContext,
    ) => {
      const actorUserId = requireActor(context);
      const result = await context.eventPlanner.deleteTask(
        context.defaultTenant,
        {
          taskId: args.input.taskId,
          expectedVersion: args.input.expectedVersion,
          actorUserId,
        },
      );
      return taskMutationToGraphql(result);
    },
    createPlacementNote: async (
      _parent: unknown,
      args: {
        readonly input: { readonly placementId: string; readonly body: string };
      },
      context: EventPlannerContext,
    ) => {
      const actorUserId = requireActor(context);
      const result = await context.eventPlanner.createPlacementNote(
        context.defaultTenant,
        {
          placementId: args.input.placementId,
          body: args.input.body,
          actorUserId,
        },
      );
      return noteMutationToGraphql(result);
    },
    deletePlacementNote: async (
      _parent: unknown,
      args: { readonly input: { readonly noteId: string } },
      context: EventPlannerContext,
    ) => {
      const actorUserId = requireActor(context);
      const result = await context.eventPlanner.deletePlacementNote(
        context.defaultTenant,
        { noteId: args.input.noteId, actorUserId },
      );
      return noteMutationToGraphql(result);
    },
    createBookmark: async (
      _parent: unknown,
      args: {
        readonly input: {
          readonly eventSlug: string;
          readonly name: string;
          readonly centerLng: number;
          readonly centerLat: number;
          readonly zoom: number;
          readonly pitch?: number | null;
          readonly bearing?: number | null;
        };
      },
      context: EventPlannerContext,
    ) => {
      const actorUserId = requireActor(context);
      const result = await context.eventPlanner.createBookmark(
        context.defaultTenant,
        {
          eventLayerSlug: args.input.eventSlug,
          name: args.input.name,
          centerLng: args.input.centerLng,
          centerLat: args.input.centerLat,
          zoom: args.input.zoom,
          pitch: args.input.pitch ?? 0,
          bearing: args.input.bearing ?? 0,
          actorUserId,
        },
      );
      return bookmarkMutationToGraphql(result);
    },
    updateBookmark: async (
      _parent: unknown,
      args: {
        readonly input: {
          readonly bookmarkId: string;
          readonly expectedVersion: number;
          readonly name?: string | null;
          readonly centerLng?: number | null;
          readonly centerLat?: number | null;
          readonly zoom?: number | null;
          readonly pitch?: number | null;
          readonly bearing?: number | null;
        };
      },
      context: EventPlannerContext,
    ) => {
      const actorUserId = requireActor(context);
      const input: Record<string, unknown> = {
        bookmarkId: args.input.bookmarkId,
        expectedVersion: args.input.expectedVersion,
        actorUserId,
      };
      if (Object.prototype.hasOwnProperty.call(args.input, "name"))
        input.name = args.input.name ?? "";
      if (Object.prototype.hasOwnProperty.call(args.input, "centerLng"))
        input.centerLng = args.input.centerLng ?? 0;
      if (Object.prototype.hasOwnProperty.call(args.input, "centerLat"))
        input.centerLat = args.input.centerLat ?? 0;
      if (Object.prototype.hasOwnProperty.call(args.input, "zoom"))
        input.zoom = args.input.zoom ?? 0;
      if (Object.prototype.hasOwnProperty.call(args.input, "pitch"))
        input.pitch = args.input.pitch ?? 0;
      if (Object.prototype.hasOwnProperty.call(args.input, "bearing"))
        input.bearing = args.input.bearing ?? 0;
      const result = await context.eventPlanner.updateBookmark(
        context.defaultTenant,
        input as unknown as Parameters<
          EventPlannerGrpcClient["updateBookmark"]
        >[1],
      );
      return bookmarkMutationToGraphql(result);
    },
    deleteBookmark: async (
      _parent: unknown,
      args: {
        readonly input: { readonly bookmarkId: string; readonly expectedVersion: number };
      },
      context: EventPlannerContext,
    ) => {
      const actorUserId = requireActor(context);
      const result = await context.eventPlanner.deleteBookmark(
        context.defaultTenant,
        {
          bookmarkId: args.input.bookmarkId,
          expectedVersion: args.input.expectedVersion,
          actorUserId,
        },
      );
      return bookmarkMutationToGraphql(result);
    },
  },
};
