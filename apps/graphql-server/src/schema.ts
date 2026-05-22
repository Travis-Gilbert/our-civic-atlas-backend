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

type ScenarioRecord = {
  readonly scenarioId: string;
  readonly name: string;
  readonly description: string;
  readonly state: string;
  readonly provenance: string;
  readonly baseScenarioId: string | null;
  readonly updatedAt: string;
};

type ScenarioDeltaRecord = {
  readonly parcelKey: string;
  readonly label: string;
  readonly geometry: Record<string, unknown>;
  readonly heightDeltaM: number;
  readonly floorAreaDeltaM2: number;
  readonly unitsDelta: number;
  readonly bindingConstraint: string;
};

type BuildableEnvelopeRecord = {
  readonly parcelKey: string;
  readonly scenarioId: string;
  readonly geometry: Record<string, unknown>;
  readonly maxHeightMeters: number;
  readonly buildableFloorAreaM2: number;
  readonly residentialUnits: number;
  readonly bindingConstraint: string;
  readonly inheritedFromScenarioId: string | null;
};

type KpiMetricRecord = {
  readonly kpiId: string;
  readonly label: string;
  readonly value: number;
  readonly unit: string;
  readonly uncertaintyLow: number | null;
  readonly uncertaintyHigh: number | null;
  readonly sourceSummary: string;
};

const SCENARIOS: readonly ScenarioRecord[] = [
  {
    scenarioId: "current",
    name: "Current Flint",
    description: "Present-day public atlas rows.",
    state: "published",
    provenance: "actual",
    baseScenarioId: null,
    updatedAt: "2026-05-22T00:00:00-04:00",
  },
  {
    scenarioId: "safe-routes-starter",
    name: "Safe routes starter",
    description:
      "Starter planning scenario with reviewed corridor improvements and parcel capacity deltas.",
    state: "draft",
    provenance: "future",
    baseScenarioId: "current",
    updatedAt: "2026-05-22T00:00:00-04:00",
  },
];

const SCENARIO_DELTAS: readonly ScenarioDeltaRecord[] = [
  {
    parcelKey: "40-01-154-012",
    label: "Carriage Town mixed-use envelope",
    geometry: {
      type: "Polygon",
      coordinates: [
        [
          [-83.709354, 43.041493],
          [-83.709206, 43.041495],
          [-83.7092, 43.041192],
          [-83.709345, 43.041189],
          [-83.709354, 43.041493],
        ],
      ],
    },
    heightDeltaM: 7.3,
    floorAreaDeltaM2: 640,
    unitsDelta: 8,
    bindingConstraint: "height",
  },
  {
    parcelKey: "40-01-154-018",
    label: "Grand Traverse infill test",
    geometry: {
      type: "Polygon",
      coordinates: [
        [
          [-83.70798, 43.04092],
          [-83.70771, 43.04092],
          [-83.70772, 43.04067],
          [-83.70799, 43.04068],
          [-83.70798, 43.04092],
        ],
      ],
    },
    heightDeltaM: 4.2,
    floorAreaDeltaM2: 410,
    unitsDelta: 5,
    bindingConstraint: "far",
  },
];

const SCENARIO_ENVELOPES: readonly BuildableEnvelopeRecord[] =
  SCENARIO_DELTAS.map((delta) => ({
    parcelKey: delta.parcelKey,
    scenarioId: "safe-routes-starter",
    geometry: delta.geometry,
    maxHeightMeters: 12 + delta.heightDeltaM,
    buildableFloorAreaM2: delta.floorAreaDeltaM2,
    residentialUnits: Math.max(0, delta.unitsDelta),
    bindingConstraint: delta.bindingConstraint,
    inheritedFromScenarioId: null,
  }));

const KPI_BY_SCENARIO: Record<string, readonly KpiMetricRecord[]> = {
  current: [
    {
      kpiId: "population_capacity",
      label: "Population capacity",
      value: 142,
      unit: "people",
      uncertaintyLow: 122,
      uncertaintyHigh: 162,
      sourceSummary: "Current envelope units multiplied by ACS household-size assumption.",
    },
    {
      kpiId: "tax_base_capacity",
      label: "Tax base capacity",
      value: 284000,
      unit: "usd/year",
      uncertaintyLow: 203000,
      uncertaintyHigh: 379000,
      sourceSummary: "Current buildable floor area multiplied by a cited planning value.",
    },
  ],
  "safe-routes-starter": [
    {
      kpiId: "population_capacity",
      label: "Population capacity",
      value: 169,
      unit: "people",
      uncertaintyLow: 145,
      uncertaintyHigh: 193,
      sourceSummary: "Scenario envelope units multiplied by ACS household-size assumption.",
    },
    {
      kpiId: "tax_base_capacity",
      label: "Tax base capacity",
      value: 328100,
      unit: "usd/year",
      uncertaintyLow: 234400,
      uncertaintyHigh: 437500,
      sourceSummary: "Scenario buildable floor area multiplied by a cited planning value.",
    },
  ],
};

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

    type Scenario {
      scenarioId: ID!
      name: String!
      description: String!
      state: String!
      provenance: String!
      baseScenarioId: ID
      updatedAt: DateTime!
    }

    type ScenarioDelta {
      parcelKey: ID!
      label: String!
      geometry: GeoJSON!
      heightDeltaM: Float!
      floorAreaDeltaM2: Float!
      unitsDelta: Int!
      bindingConstraint: String!
    }

    type BuildableEnvelope {
      parcelKey: ID!
      scenarioId: ID!
      geometry: GeoJSON!
      maxHeightMeters: Float!
      buildableFloorAreaM2: Float!
      residentialUnits: Int!
      bindingConstraint: String!
      inheritedFromScenarioId: ID
    }

    type ScenarioComparison {
      baseScenarioId: ID!
      targetScenarioId: ID!
      changedParcelCount: Int!
      deltas: [ScenarioDelta!]!
    }

    type ScenarioRecomputeJob {
      jobId: ID!
      scenarioId: ID!
      status: String!
      dirtyParcelCount: Int!
      inheritedParcelCount: Int!
      completedAt: DateTime
      errorMessage: String
    }

    type KpiMetric {
      kpiId: ID!
      label: String!
      value: Float!
      unit: String!
      uncertaintyLow: Float
      uncertaintyHigh: Float
      sourceSummary: String!
    }

    type KpiBundle {
      scenarioId: ID!
      scope: String!
      scopeId: ID!
      computedAt: DateTime!
      metrics: [KpiMetric!]!
    }

    type Query {
      health: Health!
      placesList(tenantId: ID!): [CivicObject!]!
      scenarios(tenantId: ID!): [Scenario!]!
      scenarioEnvelopes(
        tenantId: ID!
        scenarioId: ID!
      ): [BuildableEnvelope!]!
      scenarioComparison(
        tenantId: ID!
        baseScenarioId: ID! = "current"
        targetScenarioId: ID!
      ): ScenarioComparison!
      kpiBundle(
        tenantId: ID!
        scenarioId: ID!
        scope: String! = "city"
        scopeId: ID! = "flint"
      ): KpiBundle!
      kpiDelta(
        tenantId: ID!
        baseScenarioId: ID! = "current"
        targetScenarioId: ID!
        scope: String! = "city"
        scopeId: ID! = "flint"
      ): KpiBundle!
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
      forkScenario(tenantId: ID!, baseScenarioId: ID!, name: String!): Scenario!
      requestScenarioRecompute(tenantId: ID!, scenarioId: ID!, parcelKeys: [ID!]!): ScenarioRecomputeJob!
      publishScenario(tenantId: ID!, scenarioId: ID!): Scenario!
      archiveScenario(tenantId: ID!, scenarioId: ID!): Scenario!
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
      scenarios: (
        _parent: unknown,
        _args: { readonly tenantId: string },
      ) => SCENARIOS,
      scenarioEnvelopes: (
        _parent: unknown,
        args: { readonly scenarioId: string },
      ) =>
        args.scenarioId === "current"
          ? SCENARIO_ENVELOPES.map((row) => ({
              ...row,
              scenarioId: "current",
              inheritedFromScenarioId: null,
            }))
          : SCENARIO_ENVELOPES,
      scenarioComparison: (
        _parent: unknown,
        args: {
          readonly baseScenarioId: string;
          readonly targetScenarioId: string;
        },
      ) => ({
        baseScenarioId: args.baseScenarioId,
        targetScenarioId: args.targetScenarioId,
        changedParcelCount: SCENARIO_DELTAS.length,
        deltas: SCENARIO_DELTAS,
      }),
      kpiBundle: (
        _parent: unknown,
        args: {
          readonly scenarioId: string;
          readonly scope: string;
          readonly scopeId: string;
        },
      ) => ({
        scenarioId: args.scenarioId,
        scope: args.scope,
        scopeId: args.scopeId,
        computedAt: new Date().toISOString(),
        metrics: KPI_BY_SCENARIO[args.scenarioId] ?? [],
      }),
      kpiDelta: (
        _parent: unknown,
        args: {
          readonly baseScenarioId: string;
          readonly targetScenarioId: string;
          readonly scope: string;
          readonly scopeId: string;
        },
      ) => ({
        scenarioId: `${args.targetScenarioId}-vs-${args.baseScenarioId}`,
        scope: args.scope,
        scopeId: args.scopeId,
        computedAt: new Date().toISOString(),
        metrics: kpiDelta(args.baseScenarioId, args.targetScenarioId),
      }),
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
      forkScenario: (
        _parent: unknown,
        args: {
          readonly baseScenarioId: string;
          readonly name: string;
        },
      ) => ({
        scenarioId: `draft:${args.name.toLowerCase().replace(/[^a-z0-9]+/g, "-")}`,
        name: args.name,
        description: "Draft scenario fork created by the GraphQL sidecar.",
        state: "draft",
        provenance: "future",
        baseScenarioId: args.baseScenarioId,
        updatedAt: new Date().toISOString(),
      }),
      requestScenarioRecompute: (
        _parent: unknown,
        args: {
          readonly scenarioId: string;
          readonly parcelKeys: readonly string[];
        },
      ) => ({
        jobId: `scenario-recompute:${args.scenarioId}:${args.parcelKeys.length}`,
        scenarioId: args.scenarioId,
        status: "queued",
        dirtyParcelCount: args.parcelKeys.length,
        inheritedParcelCount: Math.max(0, SCENARIO_ENVELOPES.length - args.parcelKeys.length),
        completedAt: null,
        errorMessage: null,
      }),
      publishScenario: (
        _parent: unknown,
        args: { readonly scenarioId: string },
      ) => ({
        ...scenarioById(args.scenarioId),
        state: "published",
        updatedAt: new Date().toISOString(),
      }),
      archiveScenario: (
        _parent: unknown,
        args: { readonly scenarioId: string },
      ) => ({
        ...scenarioById(args.scenarioId),
        state: "archived",
        updatedAt: new Date().toISOString(),
      }),
    },
  },
});

function scenarioById(scenarioId: string): ScenarioRecord {
  return (
    SCENARIOS.find((scenario) => scenario.scenarioId === scenarioId) ??
    SCENARIOS[0]
  );
}

function kpiDelta(
  baseScenarioId: string,
  targetScenarioId: string,
): readonly KpiMetricRecord[] {
  const base = KPI_BY_SCENARIO[baseScenarioId] ?? [];
  const target = KPI_BY_SCENARIO[targetScenarioId] ?? [];
  return target.map((targetMetric) => {
    const baseMetric = base.find((metric) => metric.kpiId === targetMetric.kpiId);
    return {
      ...targetMetric,
      value: targetMetric.value - (baseMetric?.value ?? 0),
      uncertaintyLow:
        targetMetric.uncertaintyLow != null && baseMetric?.uncertaintyHigh != null
          ? targetMetric.uncertaintyLow - baseMetric.uncertaintyHigh
          : null,
      uncertaintyHigh:
        targetMetric.uncertaintyHigh != null && baseMetric?.uncertaintyLow != null
          ? targetMetric.uncertaintyHigh - baseMetric.uncertaintyLow
          : null,
      sourceSummary: `Delta between ${targetScenarioId} and ${baseScenarioId}.`,
    };
  });
}
