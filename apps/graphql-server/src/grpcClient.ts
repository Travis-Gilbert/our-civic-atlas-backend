/**
 * Civic Atlas gRPC client (Node sidecar -> Rust Axum, native gRPC).
 *
 * Speaks native gRPC over HTTP/2 to civic-atlas-server on
 * CIVIC_ATLAS_GRPC_URL (default 127.0.0.1:50051). The earlier
 * JSON-over-HTTP-to-tonic-web scaffold did not actually work because
 * tonic-web expects gRPC-Web binary framing, not plain JSON.
 *
 * Proto contracts are loaded at runtime via `@grpc/proto-loader`
 * against the workspace `proto/` directory. This avoids a build-time
 * codegen step for the sidecar (the Rust side already regenerates
 * Rust bindings via `tonic-build` in `civic-atlas-types/build.rs`).
 *
 * If the sidecar grows to many RPCs, switch to `buf generate` with
 * `protoc-gen-es` for typed bindings; for the foundation scope
 * (placesList + civicResearch) runtime loading is sufficient and
 * keeps the build pipeline simple.
 */

import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

import * as grpc from "@grpc/grpc-js";
import * as protoLoader from "@grpc/proto-loader";

export interface TenantContext {
  readonly tenantId: string;
  readonly atlasNodeId?: string;
}

export interface CivicObject {
  readonly id: string;
  readonly tenantId: string;
  readonly name: string;
  readonly objectType: string;
  readonly geometryJson: string;
  readonly timeStartMs: number | null;
  readonly timeEndMs: number | null;
  readonly confidence: number;
  readonly sourceIds: readonly string[];
  readonly dossierPath: string;
}

export interface CivicResearchInput {
  readonly query: string;
  readonly budget?: Record<string, unknown>;
  readonly scope?: Record<string, unknown>;
  readonly sessionId?: string;
  readonly folioId?: string;
}

export interface CivicResearchResponseShape {
  readonly runId: string;
  readonly skill: string;
  readonly resultsJson: string;
}

export type ReconstructionSpecShape = Record<string, unknown>;

interface ListPlacesGrpcResponse {
  readonly places: CivicObject[];
  readonly nextPageToken?: string;
}

interface CivicResearchGrpcResponse {
  readonly runId: string;
  readonly skill: string;
  readonly resultsJson: string;
}

interface GetReconstructionSpecGrpcResponse {
  readonly spec?: ReconstructionSpecShape | null;
}

interface ListReconstructionSpecsGrpcResponse {
  readonly specs?: ReconstructionSpecShape[];
  readonly nextPageToken?: string;
}

/* ------------------------------------------------------------------ */
/*  EventPlanner DTOs                                                  */
/* ------------------------------------------------------------------ */

export interface EventLayer {
  readonly id: string;
  readonly slug: string;
  readonly title: string;
  readonly startsAtMs: number;
  readonly endsAtMs: number;
  readonly boundsGeojson: string;
}

export interface Placement {
  readonly id: string;
  readonly eventLayerId: string;
  readonly category: string;
  readonly sublabel: string;
  readonly label: string;
  readonly geometryGeojson: string;
  readonly ownerUserId: string;
  readonly status: string;
  readonly notes: string;
  readonly createdAtMs: number;
  readonly updatedAtMs: number;
  readonly version: number;
}

export interface PlannerTask {
  readonly id: string;
  readonly eventLayerId: string;
  readonly title: string;
  readonly ownerDisplay: string;
  readonly dueAtMs: number;
  readonly status: string;
  readonly placementId: string;
  readonly notes: string;
  readonly createdAtMs: number;
  readonly updatedAtMs: number;
  readonly version: number;
}

interface EventLayerListGrpcResponse {
  readonly layers?: EventLayer[];
}

interface PlacementListGrpcResponse {
  readonly placements?: Placement[];
}

interface TaskListGrpcResponse {
  readonly tasks?: PlannerTask[];
}

interface PlacementMutationGrpcResponse {
  readonly placement?: Placement;
  readonly staleWrite?: boolean;
  readonly deleted?: boolean;
}

interface TaskMutationGrpcResponse {
  readonly task?: PlannerTask;
  readonly staleWrite?: boolean;
  readonly deleted?: boolean;
}

interface AuthClaimInviteGrpcResponse {
  readonly success?: boolean;
  readonly userId?: string;
  readonly displayName?: string;
  readonly email?: string;
  readonly sessionToken?: string;
  readonly error?: string;
}

interface AuthResolveSessionGrpcResponse {
  readonly authenticated?: boolean;
  readonly userId?: string;
  readonly displayName?: string;
  readonly email?: string;
}

interface AuthRevokeSessionGrpcResponse {
  readonly revoked?: boolean;
}

export interface PlacementNote {
  readonly id: string;
  readonly placementId: string;
  readonly eventLayerId: string;
  readonly authorUserId: string;
  readonly authorDisplay: string;
  readonly body: string;
  readonly createdAtMs: number;
  readonly updatedAtMs: number;
  readonly version: number;
}

export interface CameraBookmark {
  readonly id: string;
  readonly eventLayerId: string;
  readonly name: string;
  readonly centerLng: number;
  readonly centerLat: number;
  readonly zoom: number;
  readonly pitch: number;
  readonly bearing: number;
  readonly createdByUserId: string;
  readonly createdAtMs: number;
  readonly updatedAtMs: number;
  readonly version: number;
}

interface PlacementNoteListGrpcResponse {
  readonly notes?: PlacementNote[];
}

interface PlacementNoteMutationGrpcResponse {
  readonly note?: PlacementNote;
  readonly deleted?: boolean;
}

interface BookmarkListGrpcResponse {
  readonly bookmarks?: CameraBookmark[];
}

interface BookmarkMutationGrpcResponse {
  readonly bookmark?: CameraBookmark;
  readonly staleWrite?: boolean;
  readonly deleted?: boolean;
}

interface IntakePendingVendorGrpcResponse {
  readonly created?: boolean;
  readonly placementId?: string;
}

export interface PlacementNoteMutationResult {
  readonly note: PlacementNote | null;
  readonly deleted: boolean;
}

export interface BookmarkMutationResult {
  readonly bookmark: CameraBookmark | null;
  readonly staleWrite: boolean;
  readonly deleted: boolean;
}

export interface BookmarkCreateInput {
  readonly eventLayerSlug: string;
  readonly name: string;
  readonly centerLng: number;
  readonly centerLat: number;
  readonly zoom: number;
  readonly pitch: number;
  readonly bearing: number;
  readonly actorUserId: string;
}

export interface BookmarkUpdateInput {
  readonly bookmarkId: string;
  readonly expectedVersion: number;
  readonly name?: string;
  readonly centerLng?: number;
  readonly centerLat?: number;
  readonly zoom?: number;
  readonly pitch?: number;
  readonly bearing?: number;
  readonly actorUserId: string;
}

export interface BookmarkDeleteInput {
  readonly bookmarkId: string;
  readonly expectedVersion: number;
  readonly actorUserId: string;
}

export interface IntakePendingVendorInput {
  readonly eventLayerSlug: string;
  readonly businessName: string;
  readonly vendorTier: string;
  readonly contactName: string;
  readonly contactEmail: string;
  readonly needs: string;
  readonly defaultLng: number;
  readonly defaultLat: number;
  readonly idempotencyKey: string;
}

/**
 * Shared shape for placement mutations returned to the GraphQL
 * resolvers. `placement` is null on a hard delete; `staleWrite`
 * means another writer won the race and the included row (when
 * present) carries the server's current state.
 */
export interface PlacementMutationResult {
  readonly placement: Placement | null;
  readonly staleWrite: boolean;
  readonly deleted: boolean;
}

export interface TaskMutationResult {
  readonly task: PlannerTask | null;
  readonly staleWrite: boolean;
  readonly deleted: boolean;
}

export interface PlacementCreateInput {
  readonly eventLayerSlug: string;
  readonly category: string;
  readonly sublabel?: string;
  readonly label: string;
  readonly geometryGeojson: string;
  readonly status?: string;
  readonly notes?: string;
  readonly actorUserId: string;
}

export interface PlacementUpdateInput {
  readonly placementId: string;
  readonly expectedVersion: number;
  readonly category?: string;
  readonly sublabel?: string;
  readonly label?: string;
  readonly geometryGeojson?: string;
  readonly status?: string;
  readonly notes?: string;
  readonly actorUserId: string;
}

export interface PlacementDeleteInput {
  readonly placementId: string;
  readonly expectedVersion: number;
  readonly actorUserId: string;
}

export interface TaskCreateInput {
  readonly eventLayerSlug: string;
  readonly title: string;
  readonly ownerUserId?: string;
  readonly dueAtMs?: number;
  readonly status?: string;
  readonly placementId?: string;
  readonly notes?: string;
  readonly actorUserId: string;
}

export interface TaskUpdateInput {
  readonly taskId: string;
  readonly expectedVersion: number;
  readonly title?: string;
  readonly ownerUserId?: string | null;
  readonly dueAtMs?: number | null;
  readonly status?: string;
  readonly placementId?: string | null;
  readonly notes?: string;
  readonly actorUserId: string;
}

export interface TaskDeleteInput {
  readonly taskId: string;
  readonly expectedVersion: number;
  readonly actorUserId: string;
}

/* ------------------------------------------------------------------ */
/*  Proto loader                                                       */
/* ------------------------------------------------------------------ */

const here = dirname(fileURLToPath(import.meta.url));
const protoRoot = resolve(here, "..", "..", "..", "proto");
const civicAtlasProto = resolve(
  protoRoot,
  "civic_atlas",
  "v1",
  "civic_atlas.proto",
);
const eventPlannerProto = resolve(
  protoRoot,
  "civic_atlas",
  "v1",
  "event_planner.proto",
);
const reconstructionServiceProto = resolve(
  protoRoot,
  "civic_atlas",
  "v1",
  "reconstruction_service.proto",
);

// These protos share the same `civic_atlas.v1` package and import
// TenantContext from civic_atlas.proto.
// loadSync accepts an array, so a single descriptor pass picks up
// all services. keepCase:false converts snake_case proto field
// names (`event_layer_id`, `starts_at_ms`) to camelCase
// (`eventLayerId`, `startsAtMs`) for the Node side.
const packageDefinition = protoLoader.loadSync(
  [civicAtlasProto, eventPlannerProto, reconstructionServiceProto],
  {
    keepCase: false,
    longs: Number,
    enums: String,
    defaults: true,
    oneofs: true,
    includeDirs: [protoRoot],
  },
);

const protoDescriptor = grpc.loadPackageDefinition(packageDefinition);

interface CivicAtlasServiceClient extends grpc.Client {
  ListPlaces(
    request: object,
    metadata: grpc.Metadata,
    callback: (err: grpc.ServiceError | null, response: ListPlacesGrpcResponse) => void,
  ): grpc.ClientUnaryCall;
  CivicResearch(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: CivicResearchGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
}

interface CivicAtlasServiceConstructor {
  new (address: string, credentials: grpc.ChannelCredentials): CivicAtlasServiceClient;
}

interface EventPlannerServiceClient extends grpc.Client {
  ListEventLayers(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: EventLayerListGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  ListPlacements(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: PlacementListGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  ListTasks(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: TaskListGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  CreatePlacement(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: PlacementMutationGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  UpdatePlacement(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: PlacementMutationGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  DeletePlacement(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: PlacementMutationGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  CreateTask(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: TaskMutationGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  UpdateTask(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: TaskMutationGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  DeleteTask(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: TaskMutationGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  ClaimInvite(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: AuthClaimInviteGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  ResolveSession(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: AuthResolveSessionGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  RevokeSession(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: AuthRevokeSessionGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  ListPlacementNotes(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: PlacementNoteListGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  CreatePlacementNote(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: PlacementNoteMutationGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  DeletePlacementNote(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: PlacementNoteMutationGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  ListBookmarks(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: BookmarkListGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  CreateBookmark(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: BookmarkMutationGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  UpdateBookmark(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: BookmarkMutationGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  DeleteBookmark(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: BookmarkMutationGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  IntakePendingVendor(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: IntakePendingVendorGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
}

interface EventPlannerServiceConstructor {
  new (address: string, credentials: grpc.ChannelCredentials): EventPlannerServiceClient;
}

interface ReconstructionServiceClient extends grpc.Client {
  GetReconstructionSpec(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: GetReconstructionSpecGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
  ListReconstructionSpecs(
    request: object,
    metadata: grpc.Metadata,
    callback: (
      err: grpc.ServiceError | null,
      response: ListReconstructionSpecsGrpcResponse,
    ) => void,
  ): grpc.ClientUnaryCall;
}

interface ReconstructionServiceConstructor {
  new (address: string, credentials: grpc.ChannelCredentials): ReconstructionServiceClient;
}

const civicAtlasV1 = (protoDescriptor as Record<string, unknown>).civic_atlas as {
  readonly v1: {
    readonly CivicAtlasService: CivicAtlasServiceConstructor;
    readonly EventPlannerService: EventPlannerServiceConstructor;
    readonly ReconstructionService: ReconstructionServiceConstructor;
  };
};

const CivicAtlasServiceCtor = civicAtlasV1.v1.CivicAtlasService;
const EventPlannerServiceCtor = civicAtlasV1.v1.EventPlannerService;
const ReconstructionServiceCtor = civicAtlasV1.v1.ReconstructionService;

/* ------------------------------------------------------------------ */
/*  Client                                                             */
/* ------------------------------------------------------------------ */

function normalizeAddress(endpoint: string): string {
  // gRPC clients want "host:port", not a URL. Strip scheme + path if
  // the env var was set with one.
  return endpoint
    .replace(/^https?:\/\//, "")
    .replace(/^grpc:\/\//, "")
    .replace(/\/.*$/, "");
}

export class CivicAtlasGrpcClient {
  private readonly client: CivicAtlasServiceClient;

  constructor(endpoint: string) {
    const address = normalizeAddress(endpoint);
    this.client = new CivicAtlasServiceCtor(address, grpc.credentials.createInsecure());
  }

  private metadataFor(tenantId: string): grpc.Metadata {
    const metadata = new grpc.Metadata();
    metadata.add("x-tenant-id", tenantId);
    return metadata;
  }

  listPlaces(
    tenantContext: TenantContext,
    pageSize: number,
  ): Promise<readonly CivicObject[]> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.ListPlaces(
        { tenantContext, pageSize },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(
              new Error(
                `Civic Atlas ListPlaces failed: ${err.code} ${err.details ?? err.message}`,
              ),
            );
            return;
          }
          resolveResult(response.places ?? []);
        },
      );
    });
  }

  /**
   * Civic research (gap-driven fractal expansion) entry point.
   *
   * Calls Axum's `CivicAtlasService.CivicResearch` over native gRPC.
   * Axum dials the Theseus bridge via the `theseus-client` crate.
   * Budget + scope are forwarded as JSON strings because the
   * harness budget vocabulary evolves faster than the proto contract.
   *
   * Returns the raw shape from Axum: `runId`, `skill`, and
   * `resultsJson` (a JSON string matching the public `SearchResults`
   * GraphQL type). The resolver in `schema.ts` parses `resultsJson`
   * before returning to the browser.
   */
  civicResearch(
    tenantContext: TenantContext,
    input: CivicResearchInput,
  ): Promise<CivicResearchResponseShape> {
    const request = {
      tenantContext,
      query: input.query,
      budgetJson: input.budget ? JSON.stringify(input.budget) : "",
      scopeJson: input.scope ? JSON.stringify(input.scope) : "",
      sessionId: input.sessionId ?? "",
      folioId: input.folioId ?? "",
    };

    return new Promise((resolveResult, rejectResult) => {
      this.client.CivicResearch(
        request,
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(
              new Error(
                `Civic Atlas CivicResearch failed: ${err.code} ${err.details ?? err.message}`,
              ),
            );
            return;
          }
          resolveResult({
            runId: response.runId,
            skill: response.skill,
            resultsJson: response.resultsJson,
          });
        },
      );
    });
  }
}

/**
 * gRPC client for the ReconstructionService. This is the backend bridge
 * the public Atelier needs: the browser still talks GraphQL, while the
 * sidecar asks Axum for canonical ReconstructionSpec rows over gRPC.
 */
export class ReconstructionGrpcClient {
  private readonly client: ReconstructionServiceClient;

  constructor(endpoint: string) {
    const address = normalizeAddress(endpoint);
    this.client = new ReconstructionServiceCtor(
      address,
      grpc.credentials.createInsecure(),
    );
  }

  private metadataFor(tenantId: string): grpc.Metadata {
    const metadata = new grpc.Metadata();
    metadata.add("x-tenant-id", tenantId);
    return metadata;
  }

  getReconstructionSpec(
    tenantContext: TenantContext,
    specId: string,
  ): Promise<ReconstructionSpecShape> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.GetReconstructionSpec(
        { tenantContext, specId },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(
              new Error(
                `ReconstructionService GetReconstructionSpec failed: ${err.code} ${err.details ?? err.message}`,
              ),
            );
            return;
          }
          if (!response.spec) {
            rejectResult(new Error("ReconstructionService returned no spec."));
            return;
          }
          resolveResult(response.spec);
        },
      );
    });
  }

  listReconstructionSpecs(
    tenantContext: TenantContext,
    input: {
      readonly civicObjectId?: string;
      readonly status?: string;
      readonly pageSize?: number;
    } = {},
  ): Promise<readonly ReconstructionSpecShape[]> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.ListReconstructionSpecs(
        {
          tenantContext,
          civicObjectId: input.civicObjectId ?? "",
          status: input.status ?? "RECONSTRUCTION_SPEC_STATUS_APPROVED",
          pageSize: input.pageSize ?? 50,
        },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(
              new Error(
                `ReconstructionService ListReconstructionSpecs failed: ${err.code} ${err.details ?? err.message}`,
              ),
            );
            return;
          }
          resolveResult(response.specs ?? []);
        },
      );
    });
  }
}

/**
 * gRPC client for the EventPlannerService — the Porchfest Planner
 * read surface. Speaks to the same Axum process as CivicAtlasGrpcClient
 * over the same host:port (CIVIC_ATLAS_GRPC_URL), so we accept the
 * same endpoint in the constructor.
 *
 * Phase 1 wires the three list RPCs. Phase 2 adds the placement and
 * task mutations; new methods land here when the proto grows.
 */
export class EventPlannerGrpcClient {
  private readonly client: EventPlannerServiceClient;

  constructor(endpoint: string) {
    const address = normalizeAddress(endpoint);
    this.client = new EventPlannerServiceCtor(
      address,
      grpc.credentials.createInsecure(),
    );
  }

  private metadataFor(tenantId: string): grpc.Metadata {
    const metadata = new grpc.Metadata();
    metadata.add("x-tenant-id", tenantId);
    return metadata;
  }

  listEventLayers(tenantContext: TenantContext): Promise<readonly EventLayer[]> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.ListEventLayers(
        { tenantContext },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(
              new Error(
                `EventPlanner ListEventLayers failed: ${err.code} ${err.details ?? err.message}`,
              ),
            );
            return;
          }
          resolveResult(response.layers ?? []);
        },
      );
    });
  }

  listPlacements(
    tenantContext: TenantContext,
    eventLayerSlug: string,
  ): Promise<readonly Placement[]> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.ListPlacements(
        { tenantContext, eventLayerSlug },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(
              new Error(
                `EventPlanner ListPlacements failed: ${err.code} ${err.details ?? err.message}`,
              ),
            );
            return;
          }
          resolveResult(response.placements ?? []);
        },
      );
    });
  }

  listTasks(
    tenantContext: TenantContext,
    eventLayerSlug: string,
  ): Promise<readonly PlannerTask[]> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.ListTasks(
        { tenantContext, eventLayerSlug },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(
              new Error(
                `EventPlanner ListTasks failed: ${err.code} ${err.details ?? err.message}`,
              ),
            );
            return;
          }
          resolveResult(response.tasks ?? []);
        },
      );
    });
  }

  /* -------------------------------------------------------------- */
  /*  Mutations                                                      */
  /* -------------------------------------------------------------- */

  createPlacement(
    tenantContext: TenantContext,
    input: PlacementCreateInput,
  ): Promise<PlacementMutationResult> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.CreatePlacement(
        {
          tenantContext,
          eventLayerSlug: input.eventLayerSlug,
          category: input.category,
          sublabel: input.sublabel ?? "",
          label: input.label,
          geometryGeojson: input.geometryGeojson,
          status: input.status ?? "",
          notes: input.notes ?? "",
          actorUserId: input.actorUserId,
        },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("CreatePlacement", err));
            return;
          }
          resolveResult({
            placement: response.placement ?? null,
            staleWrite: Boolean(response.staleWrite),
            deleted: Boolean(response.deleted),
          });
        },
      );
    });
  }

  updatePlacement(
    tenantContext: TenantContext,
    input: PlacementUpdateInput,
  ): Promise<PlacementMutationResult> {
    const present = (key: keyof PlacementUpdateInput) =>
      Object.prototype.hasOwnProperty.call(input, key);
    return new Promise((resolveResult, rejectResult) => {
      this.client.UpdatePlacement(
        {
          tenantContext,
          placementId: input.placementId,
          expectedVersion: input.expectedVersion,
          category: input.category ?? "",
          categoryPresent: present("category"),
          sublabel: input.sublabel ?? "",
          sublabelPresent: present("sublabel"),
          label: input.label ?? "",
          labelPresent: present("label"),
          geometryGeojson: input.geometryGeojson ?? "",
          geometryPresent: present("geometryGeojson"),
          status: input.status ?? "",
          statusPresent: present("status"),
          notes: input.notes ?? "",
          notesPresent: present("notes"),
          actorUserId: input.actorUserId,
        },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("UpdatePlacement", err));
            return;
          }
          resolveResult({
            placement: response.placement ?? null,
            staleWrite: Boolean(response.staleWrite),
            deleted: Boolean(response.deleted),
          });
        },
      );
    });
  }

  deletePlacement(
    tenantContext: TenantContext,
    input: PlacementDeleteInput,
  ): Promise<PlacementMutationResult> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.DeletePlacement(
        {
          tenantContext,
          placementId: input.placementId,
          expectedVersion: input.expectedVersion,
          actorUserId: input.actorUserId,
        },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("DeletePlacement", err));
            return;
          }
          resolveResult({
            placement: response.placement ?? null,
            staleWrite: Boolean(response.staleWrite),
            deleted: Boolean(response.deleted),
          });
        },
      );
    });
  }

  createTask(
    tenantContext: TenantContext,
    input: TaskCreateInput,
  ): Promise<TaskMutationResult> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.CreateTask(
        {
          tenantContext,
          eventLayerSlug: input.eventLayerSlug,
          title: input.title,
          ownerUserId: input.ownerUserId ?? "",
          dueAtMs: input.dueAtMs ?? 0,
          status: input.status ?? "",
          placementId: input.placementId ?? "",
          notes: input.notes ?? "",
          actorUserId: input.actorUserId,
        },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("CreateTask", err));
            return;
          }
          resolveResult({
            task: response.task ?? null,
            staleWrite: Boolean(response.staleWrite),
            deleted: Boolean(response.deleted),
          });
        },
      );
    });
  }

  updateTask(
    tenantContext: TenantContext,
    input: TaskUpdateInput,
  ): Promise<TaskMutationResult> {
    const has = (key: keyof TaskUpdateInput) =>
      Object.prototype.hasOwnProperty.call(input, key);
    return new Promise((resolveResult, rejectResult) => {
      this.client.UpdateTask(
        {
          tenantContext,
          taskId: input.taskId,
          expectedVersion: input.expectedVersion,
          title: input.title ?? "",
          titlePresent: has("title"),
          ownerUserId: input.ownerUserId ?? "",
          ownerPresent: has("ownerUserId"),
          dueAtMs: input.dueAtMs ?? 0,
          dueAtPresent: has("dueAtMs"),
          status: input.status ?? "",
          statusPresent: has("status"),
          placementId: input.placementId ?? "",
          placementPresent: has("placementId"),
          notes: input.notes ?? "",
          notesPresent: has("notes"),
          actorUserId: input.actorUserId,
        },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("UpdateTask", err));
            return;
          }
          resolveResult({
            task: response.task ?? null,
            staleWrite: Boolean(response.staleWrite),
            deleted: Boolean(response.deleted),
          });
        },
      );
    });
  }

  deleteTask(
    tenantContext: TenantContext,
    input: TaskDeleteInput,
  ): Promise<TaskMutationResult> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.DeleteTask(
        {
          tenantContext,
          taskId: input.taskId,
          expectedVersion: input.expectedVersion,
          actorUserId: input.actorUserId,
        },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("DeleteTask", err));
            return;
          }
          resolveResult({
            task: response.task ?? null,
            staleWrite: Boolean(response.staleWrite),
            deleted: Boolean(response.deleted),
          });
        },
      );
    });
  }

  /* -------------------------------------------------------------- */
  /*  Auth                                                           */
  /* -------------------------------------------------------------- */

  claimInvite(
    tenantContext: TenantContext,
    token: string,
  ): Promise<AuthClaimInviteGrpcResponse> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.ClaimInvite(
        { tenantContext, token },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("ClaimInvite", err));
            return;
          }
          resolveResult(response);
        },
      );
    });
  }

  resolveSession(
    tenantContext: TenantContext,
    sessionToken: string,
  ): Promise<AuthResolveSessionGrpcResponse> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.ResolveSession(
        { tenantContext, sessionToken },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("ResolveSession", err));
            return;
          }
          resolveResult(response);
        },
      );
    });
  }

  revokeSession(
    tenantContext: TenantContext,
    sessionToken: string,
  ): Promise<AuthRevokeSessionGrpcResponse> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.RevokeSession(
        { tenantContext, sessionToken },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("RevokeSession", err));
            return;
          }
          resolveResult(response);
        },
      );
    });
  }

  /* -------------------------------------------------------------- */
  /*  Phase 3: notes                                                 */
  /* -------------------------------------------------------------- */

  listPlacementNotes(
    tenantContext: TenantContext,
    placementId: string,
  ): Promise<readonly PlacementNote[]> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.ListPlacementNotes(
        { tenantContext, placementId },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("ListPlacementNotes", err));
            return;
          }
          resolveResult(response.notes ?? []);
        },
      );
    });
  }

  createPlacementNote(
    tenantContext: TenantContext,
    input: { placementId: string; body: string; actorUserId: string },
  ): Promise<PlacementNoteMutationResult> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.CreatePlacementNote(
        { tenantContext, ...input },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("CreatePlacementNote", err));
            return;
          }
          resolveResult({
            note: response.note ?? null,
            deleted: Boolean(response.deleted),
          });
        },
      );
    });
  }

  deletePlacementNote(
    tenantContext: TenantContext,
    input: { noteId: string; actorUserId: string },
  ): Promise<PlacementNoteMutationResult> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.DeletePlacementNote(
        { tenantContext, ...input },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("DeletePlacementNote", err));
            return;
          }
          resolveResult({
            note: response.note ?? null,
            deleted: Boolean(response.deleted),
          });
        },
      );
    });
  }

  /* -------------------------------------------------------------- */
  /*  Phase 3: bookmarks                                             */
  /* -------------------------------------------------------------- */

  listBookmarks(
    tenantContext: TenantContext,
    eventLayerSlug: string,
  ): Promise<readonly CameraBookmark[]> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.ListBookmarks(
        { tenantContext, eventLayerSlug },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("ListBookmarks", err));
            return;
          }
          resolveResult(response.bookmarks ?? []);
        },
      );
    });
  }

  createBookmark(
    tenantContext: TenantContext,
    input: BookmarkCreateInput,
  ): Promise<BookmarkMutationResult> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.CreateBookmark(
        { tenantContext, ...input },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("CreateBookmark", err));
            return;
          }
          resolveResult({
            bookmark: response.bookmark ?? null,
            staleWrite: Boolean(response.staleWrite),
            deleted: Boolean(response.deleted),
          });
        },
      );
    });
  }

  updateBookmark(
    tenantContext: TenantContext,
    input: BookmarkUpdateInput,
  ): Promise<BookmarkMutationResult> {
    const has = (key: keyof BookmarkUpdateInput) =>
      Object.prototype.hasOwnProperty.call(input, key);
    return new Promise((resolveResult, rejectResult) => {
      this.client.UpdateBookmark(
        {
          tenantContext,
          bookmarkId: input.bookmarkId,
          expectedVersion: input.expectedVersion,
          actorUserId: input.actorUserId,
          name: input.name ?? "",
          namePresent: has("name"),
          centerLng: input.centerLng ?? 0,
          centerLngPresent: has("centerLng"),
          centerLat: input.centerLat ?? 0,
          centerLatPresent: has("centerLat"),
          zoom: input.zoom ?? 0,
          zoomPresent: has("zoom"),
          pitch: input.pitch ?? 0,
          pitchPresent: has("pitch"),
          bearing: input.bearing ?? 0,
          bearingPresent: has("bearing"),
        },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("UpdateBookmark", err));
            return;
          }
          resolveResult({
            bookmark: response.bookmark ?? null,
            staleWrite: Boolean(response.staleWrite),
            deleted: Boolean(response.deleted),
          });
        },
      );
    });
  }

  deleteBookmark(
    tenantContext: TenantContext,
    input: BookmarkDeleteInput,
  ): Promise<BookmarkMutationResult> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.DeleteBookmark(
        { tenantContext, ...input },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("DeleteBookmark", err));
            return;
          }
          resolveResult({
            bookmark: response.bookmark ?? null,
            staleWrite: Boolean(response.staleWrite),
            deleted: Boolean(response.deleted),
          });
        },
      );
    });
  }

  /* -------------------------------------------------------------- */
  /*  Phase 3: Stripe-driven vendor intake                           */
  /* -------------------------------------------------------------- */

  intakePendingVendor(
    tenantContext: TenantContext,
    input: IntakePendingVendorInput,
  ): Promise<{ created: boolean; placementId: string }> {
    return new Promise((resolveResult, rejectResult) => {
      this.client.IntakePendingVendor(
        { tenantContext, ...input },
        this.metadataFor(tenantContext.tenantId),
        (err, response) => {
          if (err) {
            rejectResult(grpcError("IntakePendingVendor", err));
            return;
          }
          resolveResult({
            created: Boolean(response.created),
            placementId: response.placementId ?? "",
          });
        },
      );
    });
  }
}

function grpcError(rpc: string, err: grpc.ServiceError): Error {
  return new Error(
    `EventPlanner ${rpc} failed: ${err.code} ${err.details ?? err.message}`,
  );
}
