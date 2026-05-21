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

interface ListPlacesGrpcResponse {
  readonly places: CivicObject[];
  readonly nextPageToken?: string;
}

interface CivicResearchGrpcResponse {
  readonly runId: string;
  readonly skill: string;
  readonly resultsJson: string;
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

const packageDefinition = protoLoader.loadSync(civicAtlasProto, {
  keepCase: false,
  longs: Number,
  enums: String,
  defaults: true,
  oneofs: true,
  includeDirs: [protoRoot],
});

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

const civicAtlasV1 = (protoDescriptor as Record<string, unknown>).civic_atlas as {
  readonly v1: { readonly CivicAtlasService: CivicAtlasServiceConstructor };
};

const CivicAtlasServiceCtor = civicAtlasV1.v1.CivicAtlasService;

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
