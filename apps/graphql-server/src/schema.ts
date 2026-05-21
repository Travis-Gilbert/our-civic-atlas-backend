import DataLoader from "dataloader";
import { createSchema } from "graphql-yoga";

import {
  CivicAtlasGrpcClient,
  type CivicObject,
  type TenantContext,
} from "./grpcClient.js";

export interface GraphqlContext {
  readonly client: CivicAtlasGrpcClient;
  readonly placesLoader: DataLoader<string, readonly CivicObject[]>;
  readonly defaultTenant: TenantContext;
}

/**
 * Default tenant for unscoped requests. The frontend will eventually
 * pass a tenant header (or carry a session-resolved tenant); until
 * then the GraphQL server falls back to CIVIC_ATLAS_DEFAULT_TENANT
 * env var, matching the Axum backend's fallback.
 */
function defaultTenantFromEnv(): TenantContext {
  const tenantId = process.env.CIVIC_ATLAS_DEFAULT_TENANT ?? "flint";
  return { tenantId, atlasNodeId: `atlas:${tenantId}` };
}

export function buildContext(client: CivicAtlasGrpcClient): GraphqlContext {
  const defaultTenant = defaultTenantFromEnv();
  return {
    client,
    defaultTenant,
    placesLoader: new DataLoader(async (tenantIds: readonly string[]) =>
      Promise.all(
        tenantIds.map((tenantId) =>
          client.listPlaces({ tenantId, atlasNodeId: `atlas:${tenantId}` }, 500),
        ),
      ),
    ),
  };
}

/**
 * Parse the JSON payload Axum returns from the CivicResearch RPC.
 * The server promises a SearchResults shape; we still defensively
 * coerce each field so a malformed upstream response surfaces as
 * empty arrays rather than crashing GraphQL field resolution.
 */
function parseSearchResults(resultsJson: string, query: string) {
  let parsed: Record<string, unknown> = {};
  try {
    const decoded = JSON.parse(resultsJson || "{}") as unknown;
    if (decoded && typeof decoded === "object") {
      parsed = decoded as Record<string, unknown>;
    }
  } catch {
    parsed = {};
  }
  const arr = (key: string): readonly unknown[] => {
    const value = parsed[key];
    return Array.isArray(value) ? (value as readonly unknown[]) : [];
  };
  return {
    query: (parsed.query as string | undefined) ?? query,
    totalResultCount: Number(parsed.totalResultCount ?? 0),
    reranked: Boolean(parsed.reranked),
    acceptedConfidenceFloor: Number(parsed.acceptedConfidenceFloor ?? 0),
    inferredTimeRange:
      (parsed.inferredTimeRange as Record<string, unknown> | null | undefined) ??
      null,
    places: arr("places"),
    signals: arr("signals"),
    events: arr("events"),
    historicalReconstructions: arr("historicalReconstructions"),
    sources: arr("sources"),
  };
}

export const schema = createSchema<GraphqlContext>({
  typeDefs: /* GraphQL */ `
    scalar JSON
    scalar DateTime
    scalar GeoJSON
    scalar LatLng

    type Health {
      status: String!
      service: String!
    }

    type CivicObject {
      id: ID!
      tenantId: ID!
      name: String!
      objectType: String!
      geometryJson: String!
      timeStartMs: Float
      timeEndMs: Float
      confidence: Float!
      sourceIds: [String!]!
      dossierPath: String!
    }

    """
    Time range surfaced by the algorithm when a query carries a
    temporal inference (e.g., "carriage town 1925" -> 1920-1930).
    """
    type TimeRange {
      start: DateTime
      end: DateTime
      label: String
    }

    """
    Shape returned by searchAtlas and civicResearch alike. Lets the
    frontend inject results into existing atlas state with a single
    merge step. Arrays are empty (never null) when the algorithm
    surfaces nothing.
    """
    type SearchResults {
      query: String!
      totalResultCount: Int!
      reranked: Boolean!
      acceptedConfidenceFloor: Float!
      inferredTimeRange: TimeRange
      places: [JSON!]!
      signals: [JSON!]!
      events: [JSON!]!
      historicalReconstructions: [JSON!]!
      sources: [JSON!]!
    }

    """
    Inputs for the civicResearch mutation. Optional fields are
    forwarded to the Theseus harness via Axum; the resolver applies
    sensible defaults when omitted.
    """
    input CivicResearchInput {
      query: String!
      budget: JSON
      scope: JSON
      sessionId: String
      folioId: String
    }

    """
    Payload returned by civicResearch. Run id correlates the call
    with the underlying harness run for replay / compare; skill names
    the algorithm; results carries the evidence.
    """
    type CivicResearchPayload {
      runId: ID!
      skill: String!
      results: SearchResults!
    }

    type Query {
      health: Health!
      placesList(tenantId: ID!): [CivicObject!]!
    }

    type Mutation {
      """
      Run Theseus's gap-driven fractal-expansion algorithm and return
      the evidence it surfaces. The mutation is the user-facing entry
      point of the civic research tool: a designer (or visitor) types
      a query, Theseus crawls authoritative sources for relevant
      historic information, and the result returns in the
      SearchResults shape callers already know.
      """
      civicResearch(input: CivicResearchInput!): CivicResearchPayload!
    }
  `,
  resolvers: {
    Query: {
      health: () => ({
        status: "ok",
        service: "civic-atlas-graphql-server",
      }),
      placesList: async (
        _parent: unknown,
        args: { readonly tenantId: string },
        context: GraphqlContext,
      ) => context.placesLoader.load(args.tenantId),
    },
    Mutation: {
      civicResearch: async (
        _parent: unknown,
        args: {
          readonly input: {
            readonly query: string;
            readonly budget?: Record<string, unknown>;
            readonly scope?: Record<string, unknown>;
            readonly sessionId?: string;
            readonly folioId?: string;
          };
        },
        context: GraphqlContext,
      ) => {
        const trimmed = args.input.query.trim();
        if (trimmed.length === 0) {
          throw new Error(
            "civicResearch: `query` must be a non-empty string.",
          );
        }
        const response = await context.client.civicResearch(
          context.defaultTenant,
          {
            query: trimmed,
            budget: args.input.budget,
            scope: args.input.scope,
            sessionId: args.input.sessionId,
            folioId: args.input.folioId,
          },
        );
        return {
          runId: response.runId,
          skill: response.skill,
          results: parseSearchResults(response.resultsJson, trimmed),
        };
      },
    },
  },
});

