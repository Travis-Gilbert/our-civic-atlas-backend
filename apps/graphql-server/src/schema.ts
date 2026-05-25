import DataLoader from "dataloader";
import { createSchema } from "graphql-yoga";

import {
  CivicAtlasGrpcClient,
  EventPlannerGrpcClient,
  ReconstructionGrpcClient,
  type CivicObject,
  type ReconstructionSpecShape,
  type TenantContext,
} from "./grpcClient.js";
import {
  eventPlannerResolvers,
  eventPlannerTypeDefs,
} from "./schema/event-planner/index.js";

export interface GraphqlContext {
  readonly client: CivicAtlasGrpcClient;
  readonly eventPlanner: EventPlannerGrpcClient;
  readonly reconstruction: ReconstructionGrpcClient;
  readonly placesLoader: DataLoader<string, readonly CivicObject[]>;
  readonly defaultTenant: TenantContext;
  /** Active planner user id resolved from the session cookie, if any. */
  readonly actorUserId: string | null;
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

type JsonRecord = Record<string, unknown>;

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

export function buildContext(
  client: CivicAtlasGrpcClient,
  eventPlanner: EventPlannerGrpcClient,
  reconstruction: ReconstructionGrpcClient,
  options: { actorUserId?: string | null } = {},
): GraphqlContext {
  const defaultTenant = defaultTenantFromEnv();
  return {
    client,
    eventPlanner,
    reconstruction,
    defaultTenant,
    actorUserId: options.actorUserId ?? null,
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
  let parsed: JsonRecord = {};
  try {
    const decoded = JSON.parse(resultsJson || "{}") as unknown;
    if (decoded && typeof decoded === "object") {
      parsed = decoded as JsonRecord;
    }
  } catch {
    parsed = {};
  }
  const record = (value: unknown): JsonRecord =>
    value && typeof value === "object" && !Array.isArray(value)
      ? (value as JsonRecord)
      : {};
  const arr = (key: string): readonly unknown[] => {
    const value = parsed[key];
    return Array.isArray(value) ? (value as readonly unknown[]) : [];
  };
  const text = (value: unknown, fallback = ""): string =>
    typeof value === "string" && value.trim() ? value : fallback;
  const nullableText = (value: unknown): string | null =>
    typeof value === "string" && value.trim() ? value : null;
  const number = (value: unknown, fallback = 0): number =>
    typeof value === "number" && Number.isFinite(value) ? value : fallback;
  const nullableNumber = (value: unknown): number | null =>
    typeof value === "number" && Number.isFinite(value) ? value : null;
  const stringArray = (value: unknown): string[] =>
    Array.isArray(value)
      ? value.filter((item): item is string => typeof item === "string")
      : [];
  const footprintRecord = (value: unknown) => {
    const item = record(value);
    return {
      widthMeters: number(item.widthMeters ?? item.width_m, 0),
      depthMeters: number(item.depthMeters ?? item.depth_m, 0),
    };
  };
  const coordinatePair = (value: unknown): [number, number] | null => {
    if (
      Array.isArray(value) &&
      value.length >= 2 &&
      typeof value[0] === "number" &&
      typeof value[1] === "number"
    ) {
      return [value[0], value[1]];
    }
    return null;
  };
  const placeRef = (value: unknown) => {
    const item = record(value);
    const id = text(item.id);
    const name = text(item.name);
    return id && name ? { id, name } : null;
  };
  const typedPlaces = arr("places").map((value, index) => {
    const item = record(value);
    return {
      id: text(item.id, `place:${index}`),
      name: text(item.name ?? item.label, "Unknown place"),
      placeType: text(item.placeType ?? item.kind, "place"),
      centroid: coordinatePair(item.centroid),
      confidence: number(item.confidence, 0),
      temporalStatus: text(item.temporalStatus, "unknown"),
    };
  });
  const typedSignals = arr("signals").map((value, index) => {
    const item = record(value);
    return {
      id: text(item.id, `signal:${index}`),
      signalKind: text(item.signalKind ?? item.kind, "civic_research"),
      title: text(item.title ?? item.label, "Research signal"),
      summary: text(item.summary ?? item.snippet),
      publishedAt: nullableText(item.publishedAt),
      relativeTimeLabel: nullableText(item.relativeTimeLabel),
      confidence: number(item.confidence, 0),
      place: placeRef(item.place),
    };
  });
  const typedEvents = arr("events").map((value, index) => {
    const item = record(value);
    return {
      id: text(item.id, `event:${index}`),
      title: text(item.title ?? item.label, "Research event"),
      summary: text(item.summary ?? item.snippet),
      occurredAt: nullableText(item.occurredAt),
      confidence: number(item.confidence, 0),
      place: placeRef(item.place),
    };
  });
  const typedReconstructions = arr("historicalReconstructions").map(
    (value, index) => {
      const item = record(value);
      return {
        id: text(item.id, `reconstruction:${index}`),
        civicObjectId: text(item.civicObjectId ?? item.civic_object_id, `reconstruction:${index}`),
        name: text(item.name ?? item.label, "Research reconstruction"),
        description: text(item.description ?? item.snippet),
        position: coordinatePair(item.position) ?? [0, 0],
        footprint: footprintRecord(item.footprint),
        heightMeters: number(item.heightMeters ?? item.height_m, 0),
        bearingDegrees: number(item.bearingDegrees ?? item.bearing_deg, 0),
        confidence: number(item.confidence, 0),
        facadeConfidence: nullableNumber(item.facadeConfidence ?? item.facade_confidence),
        roofConfidence: nullableNumber(item.roofConfidence ?? item.roof_confidence),
        groundFloorConfidence: nullableNumber(
          item.groundFloorConfidence ?? item.ground_floor_confidence,
        ),
        roofForm: nullableText(item.roofForm ?? item.roof_form),
        timeStart: nullableText(item.timeStart),
        timeEnd: nullableText(item.timeEnd),
        geometryUrl: nullableText(item.geometryUrl ?? item.geometry_url),
        geometryFormat: nullableText(item.geometryFormat ?? item.geometry_format),
        foundryAssetUrl: nullableText(item.foundryAssetUrl ?? item.foundry_asset_url),
        sources: [],
      };
    },
  );
  const typedSources = arr("sources").map((value, index) => {
    const item = record(value);
    return {
      id: text(item.id, `source:${index}`),
      name: text(item.name ?? item.url ?? item.source, "Research source"),
      homepageUrl: nullableText(item.homepageUrl ?? item.homepage_url ?? item.url),
      sourceType: text(item.sourceType ?? item.source, "research"),
      publicUseTerms: nullableText(item.publicUseTerms ?? item.public_use_terms),
      trustTier: text(item.trustTier, "reviewable"),
      lastChecked: nullableText(item.lastChecked ?? item.last_checked),
      knownLimits: stringArray(item.knownLimits ?? item.known_limits),
      containsPersonalData: Boolean(item.containsPersonalData ?? item.contains_personal_data),
    };
  });
  const searchEvidence = [...arr("priorKnowledge"), ...arr("newEvidence")].map(
    (value, index) => {
      const item = record(value);
      return {
        id: text(item.resultId ?? item.id, `research:${index}`),
        signalKind: text(item.kind, "civic_research"),
        title: text(item.label ?? item.title, "Research result"),
        summary: text(item.snippet ?? item.summary),
        publishedAt: null,
        relativeTimeLabel: null,
        confidence: number(item.confidence, number(item.relevanceScore, 0)),
        place: null,
        source: text(item.source),
        url: text(item.url),
      };
    },
  );
  const gapSignals = arr("gapClosures").map((value, index) => {
    const item = record(value);
    const gapId = text(item.gapId ?? item.id, `gap:${index}`);
    const description = text(item.description ?? item.summary);
    const closed = Boolean(item.closed);
    const isConfigMissing =
      gapId.includes("rustyred_unconfigured") ||
      description.toLowerCase().includes("rustyred_unconfigured");
    const title = isConfigMissing
      ? "Research sources are not connected yet"
      : closed
        ? "Research gap closed"
        : "Research needs more source data";
    return {
      id: `gap:${gapId}`,
      signalKind: closed ? "gap_closure" : "research_status",
      title,
      summary:
        description ||
        (closed
          ? "Civic research closed a source gap."
          : "Civic research could not expand this query yet."),
      publishedAt: null,
      relativeTimeLabel: null,
      confidence: 0,
      place: null,
    };
  });
  const evidenceSources = searchEvidence
    .filter((item) => item.url || item.source)
    .map((item, index) => ({
      id: `research-source:${item.id || index}`,
      name: item.url || item.source || "Research source",
      homepageUrl: item.url || null,
      sourceType: item.source || "research",
      publicUseTerms: null,
      trustTier: "reviewable",
      lastChecked: null,
      knownLimits: [],
      containsPersonalData: false,
    }));
  const signals = [
    ...typedSignals,
    ...gapSignals,
    ...searchEvidence.map(({ source: _source, url: _url, ...signal }) => signal),
  ];
  return {
    query: (parsed.query as string | undefined) ?? query,
    totalResultCount: Number(
      parsed.totalResultCount ??
        parsed.totalReturned ??
        typedPlaces.length +
          signals.length +
          typedEvents.length +
          typedReconstructions.length,
    ),
    reranked: Boolean(parsed.reranked),
    acceptedConfidenceFloor: Number(parsed.acceptedConfidenceFloor ?? 0),
    inferredTimeRange:
      (parsed.inferredTimeRange as JsonRecord | null | undefined) ??
      null,
    places: typedPlaces,
    signals,
    events: typedEvents,
    historicalReconstructions: typedReconstructions,
    sources: [...typedSources, ...evidenceSources],
  };
}

type PublicSource = {
  readonly id: string;
  readonly name: string;
  readonly homepageUrl: string | null;
  readonly sourceType: string;
  readonly publicUseTerms: string | null;
  readonly trustTier: string;
  readonly lastChecked: string | null;
  readonly knownLimits: string[];
  readonly containsPersonalData: boolean;
};

type PublicHistoricalReconstruction = {
  readonly id: string;
  readonly civicObjectId: string;
  readonly name: string;
  readonly description: string;
  readonly position: [number, number];
  readonly footprint: { readonly widthMeters: number; readonly depthMeters: number };
  readonly heightMeters: number;
  readonly bearingDegrees: number;
  readonly confidence: number;
  readonly facadeConfidence: number | null;
  readonly roofConfidence: number | null;
  readonly groundFloorConfidence: number | null;
  readonly roofForm: string | null;
  readonly timeStart: string | null;
  readonly timeEnd: string | null;
  readonly geometryUrl: string | null;
  readonly geometryFormat: string | null;
  readonly foundryAssetUrl: string | null;
  readonly sources: PublicSource[];
};

const CARRIAGE_TOWN_SPEC_BY_EXTERNAL_ID: Record<string, string> = {
  "historical:carriage-town:whaley-house": "spec:carriage-town:1",
  "historical:carriage-town:628-kearsley": "spec:carriage-town:2",
  "historical:carriage-town:storefront": "spec:carriage-town:3",
  "historical:carriage-town:workers-cottage": "spec:carriage-town:4",
  "historical:carriage-town:stockton-house": "spec:carriage-town:5",
  "building:carriage-town:1": "spec:carriage-town:1",
  "building:carriage-town:2": "spec:carriage-town:2",
  "building:carriage-town:3": "spec:carriage-town:3",
  "building:carriage-town:4": "spec:carriage-town:4",
  "building:carriage-town:5": "spec:carriage-town:5",
};

function objectRecord(value: unknown): JsonRecord {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as JsonRecord)
    : {};
}

function arrayRecords(value: unknown): JsonRecord[] {
  return Array.isArray(value)
    ? value.filter(
        (item): item is JsonRecord =>
          item !== null && typeof item === "object" && !Array.isArray(item),
      )
    : [];
}

function stringField(value: unknown, fallback = ""): string {
  return typeof value === "string" && value.trim() ? value : fallback;
}

function optionalStringField(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
}

function numberField(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function optionalNumberField(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function firstNumber(value: JsonRecord, keys: readonly string[]): number | null {
  for (const key of keys) {
    const found = optionalNumberField(value[key]);
    if (found !== null) return found;
  }
  return null;
}

function dateFromMillis(value: unknown): string | null {
  const ms = optionalNumberField(value);
  return ms === null ? null : new Date(ms).toISOString();
}

function normalizeSpecId(reconstructionId: string): string {
  if (reconstructionId.startsWith("spec:")) return reconstructionId;
  const direct = CARRIAGE_TOWN_SPEC_BY_EXTERNAL_ID[reconstructionId];
  if (direct) return direct;
  const match = reconstructionId.match(/^building:carriage-town:(\d+)$/);
  return match ? `spec:carriage-town:${match[1]}` : reconstructionId;
}

function normalizeTrustTier(value: unknown): string {
  const raw = stringField(value, "MEDIUM").toUpperCase();
  if (raw.includes("HIGH")) return "HIGH";
  if (raw.includes("LOW")) return "LOW";
  return "MEDIUM";
}

function normalizeSourceType(value: unknown): string {
  const raw = stringField(value, "OTHER").toUpperCase();
  if (raw.includes("PHOTO")) return "PHOTO_ARCHIVE";
  if (raw.includes("MAP") || raw.includes("SURVEY")) return "HISTORICAL_ARCHIVE";
  if (raw.includes("PERMIT") || raw.includes("PUBLIC")) return "PUBLIC_RECORD";
  if (raw.includes("MODEL")) return "ACADEMIC";
  return "HISTORICAL_ARCHIVE";
}

function publicSourceFromProto(value: JsonRecord, fallbackId: string): PublicSource {
  const id = stringField(value.sourceId ?? value.id, fallbackId);
  const uri = optionalStringField(value.uri ?? value.homepageUrl);
  return {
    id,
    name: stringField(value.title ?? value.name ?? value.citation, id),
    homepageUrl: uri,
    sourceType: normalizeSourceType(value.sourceType),
    publicUseTerms: null,
    trustTier: normalizeTrustTier(value.trustTier),
    lastChecked: null,
    knownLimits: [],
    containsPersonalData: false,
  };
}

function provenanceSources(provenance: JsonRecord, fallbackPrefix: string): PublicSource[] {
  return arrayRecords(provenance.sources).map((source, index) =>
    publicSourceFromProto(source, `${fallbackPrefix}:source:${index}`),
  );
}

function provenanceConfidence(provenance: JsonRecord, fallback = 0.5): number {
  return (
    firstNumber(provenance, [
      "partConfidence",
      "part_confidence",
      "confidence",
      "coverageQuality",
      "coverage_quality",
    ]) ?? fallback
  );
}

function uniqueSources(sources: readonly PublicSource[]): PublicSource[] {
  const seen = new Set<string>();
  const result: PublicSource[] = [];
  for (const source of sources) {
    if (seen.has(source.id)) continue;
    seen.add(source.id);
    result.push(source);
  }
  return result;
}

function geometryPoints(value: unknown): Array<[number, number]> {
  if (
    Array.isArray(value) &&
    value.length >= 2 &&
    typeof value[0] === "number" &&
    typeof value[1] === "number"
  ) {
    return [[value[0], value[1]]];
  }
  if (Array.isArray(value)) {
    return value.flatMap(geometryPoints);
  }
  const object = objectRecord(value);
  if (Array.isArray(object.coordinates)) return geometryPoints(object.coordinates);
  if (object.geometry) return geometryPoints(object.geometry);
  return [];
}

function parseGeometryPoints(geometryJson: string): Array<[number, number]> {
  try {
    return geometryPoints(JSON.parse(geometryJson) as unknown);
  } catch {
    return [];
  }
}

function centroidFromPoints(points: readonly [number, number][]): [number, number] | null {
  if (points.length === 0) return null;
  const [lng, lat] = points.reduce(
    ([sumLng, sumLat], point) => [sumLng + point[0], sumLat + point[1]],
    [0, 0],
  );
  return [lng / points.length, lat / points.length];
}

function footprintFromPoints(
  points: readonly [number, number][],
): { widthMeters: number; depthMeters: number } | null {
  if (points.length === 0) return null;
  const lngs = points.map((point) => point[0]);
  const lats = points.map((point) => point[1]);
  const west = Math.min(...lngs);
  const east = Math.max(...lngs);
  const south = Math.min(...lats);
  const north = Math.max(...lats);
  const meanLat = (south + north) / 2;
  const metersPerLng = 111_320 * Math.cos((meanLat * Math.PI) / 180);
  return {
    widthMeters: Math.max(1, Math.abs(east - west) * metersPerLng),
    depthMeters: Math.max(1, Math.abs(north - south) * 110_540),
  };
}

function dimensionMeters(value: unknown): number | null {
  const dimension = objectRecord(value);
  return (
    firstNumber(dimension, ["max", "min"]) ??
    optionalNumberField(value)
  );
}

function roofFormFromSpec(value: unknown): string | null {
  const raw = stringField(value).toLowerCase();
  if (raw.includes("hip")) return "HIPPED";
  if (raw.includes("gable")) return "GABLE";
  if (raw.includes("flat")) return "FLAT";
  return raw ? raw.toUpperCase() : null;
}

function targetNodeId(reconstructionId: string, part: string): string {
  return `reconstruction-node:${reconstructionId}:${part}`;
}

async function placeForCivicObject(
  context: GraphqlContext,
  civicObjectId: string,
): Promise<CivicObject | null> {
  const places = await context.placesLoader.load(context.defaultTenant.tenantId);
  return places.find((place) => place.id === civicObjectId) ?? null;
}

async function reconstructionFromSpec(
  context: GraphqlContext,
  spec: ReconstructionSpecShape,
): Promise<PublicHistoricalReconstruction> {
  const mass = objectRecord(spec.mass);
  const roof = objectRecord(spec.roof);
  const groundFloor = objectRecord(spec.groundFloor);
  const facades = arrayRecords(spec.facades);
  const primaryFacade = facades[0] ?? {};
  const massProvenance = objectRecord(mass.provenance);
  const facadeProvenance = objectRecord(primaryFacade.provenance);
  const roofProvenance = objectRecord(roof.provenance);
  const groundFloorProvenance = objectRecord(groundFloor.provenance);
  const specId = stringField(spec.specId, "spec:unknown");
  const civicObjectId = stringField(spec.civicObjectId, specId);
  const place = await placeForCivicObject(context, civicObjectId).catch(() => null);
  const points = place ? parseGeometryPoints(place.geometryJson) : [];
  const position = centroidFromPoints(points) ?? [0, 0];
  const footprint =
    footprintFromPoints(points) ??
    {
      widthMeters: dimensionMeters(mass.width) ?? 10,
      depthMeters: dimensionMeters(mass.depth) ?? 12,
    };
  const stories = firstNumber(mass, ["stories", "storyCount", "story_count"]) ?? 1;
  const heightMeters = dimensionMeters(mass.height) ?? Math.max(3, stories * 3);
  const assets = arrayRecords(spec.assets);
  const sceneAsset = assets.find((asset) =>
    stringField(asset.assetType ?? asset.asset_type).includes("scene"),
  );
  const sceneAssetUri = optionalStringField(sceneAsset?.uri);
  const renderableGeometryUrl =
    sceneAssetUri && /\.(glb|gltf)$/i.test(sceneAssetUri)
      ? sceneAssetUri
      : null;
  const sources = uniqueSources([
    ...provenanceSources(massProvenance, `${specId}:mass`),
    ...provenanceSources(facadeProvenance, `${specId}:facade`),
    ...provenanceSources(roofProvenance, `${specId}:roof`),
    ...provenanceSources(groundFloorProvenance, `${specId}:ground-floor`),
  ]);

  return {
    id: specId,
    civicObjectId,
    name: stringField(spec.title, place?.name ?? specId),
    description: [
      stringField(mass.form),
      stringField(primaryFacade.primaryMaterial ?? primaryFacade.material),
      stringField(roof.roofType ?? roof.form),
    ]
      .filter(Boolean)
      .join(" · ") || "Backend ReconstructionSpec.",
    position,
    footprint,
    heightMeters,
    bearingDegrees: 0,
    confidence: provenanceConfidence(massProvenance),
    facadeConfidence: provenanceConfidence(facadeProvenance, provenanceConfidence(massProvenance)),
    roofConfidence: provenanceConfidence(roofProvenance, provenanceConfidence(massProvenance)),
    groundFloorConfidence: provenanceConfidence(
      groundFloorProvenance,
      provenanceConfidence(massProvenance),
    ),
    roofForm: roofFormFromSpec(roof.roofType ?? roof.roof_type ?? roof.form),
    timeStart: dateFromMillis(spec.tStartMs),
    timeEnd: dateFromMillis(spec.tEndMs),
    geometryUrl: renderableGeometryUrl,
    geometryFormat: renderableGeometryUrl ? "GLTF" : null,
    foundryAssetUrl: sceneAssetUri,
    sources,
  };
}

async function getSpecForReconstruction(
  context: GraphqlContext,
  reconstructionId: string,
): Promise<ReconstructionSpecShape> {
  const specId = normalizeSpecId(reconstructionId);
  return context.reconstruction.getReconstructionSpec(context.defaultTenant, specId);
}

function evidenceFromSpec(spec: ReconstructionSpecShape) {
  const specId = stringField(spec.specId, "spec:unknown");
  const parts: Array<{ key: string; type: string; provenance: JsonRecord }> = [
    { key: "mass", type: "SANBORN", provenance: objectRecord(objectRecord(spec.mass).provenance) },
    {
      key: "facade",
      type: "PHOTOGRAPH",
      provenance: objectRecord(arrayRecords(spec.facades)[0]?.provenance),
    },
    { key: "roof", type: "SANBORN", provenance: objectRecord(objectRecord(spec.roof).provenance) },
    {
      key: "ground_floor",
      type: "CITY_DIRECTORY",
      provenance: objectRecord(objectRecord(spec.groundFloor).provenance),
    },
  ];
  const items = parts.flatMap((part) =>
    provenanceSources(part.provenance, `${specId}:${part.key}`).map((source, index) => ({
      id: `evidence:${specId}:${part.key}:${index}`,
      reconstructionId: specId,
      source,
      evidenceType: part.type,
      targetNodeId: targetNodeId(specId, part.key),
      confidence: provenanceConfidence(part.provenance),
      thumbnailUrl: null,
      summary: source.name,
      sourceDateLabel: null,
    })),
  );
  return {
    reconstructionId: specId,
    items,
    totalCount: items.length,
  };
}

function conflictsFromSpec(spec: ReconstructionSpecShape) {
  const specId = stringField(spec.specId, "spec:unknown");
  const parts: Array<{ key: string; label: string; provenance: JsonRecord }> = [
    { key: "mass", label: "Shape and size", provenance: objectRecord(objectRecord(spec.mass).provenance) },
    { key: "facade", label: "Walls", provenance: objectRecord(arrayRecords(spec.facades)[0]?.provenance) },
    { key: "roof", label: "Roof", provenance: objectRecord(objectRecord(spec.roof).provenance) },
    { key: "ground_floor", label: "Street level", provenance: objectRecord(objectRecord(spec.groundFloor).provenance) },
  ];
  return parts
    .filter((part) => Boolean(part.provenance.hasSourceConflict ?? part.provenance.has_source_conflict))
    .map((part, index) => ({
      id: `merge-conflict:${specId}:${part.key}:${index}`,
      reconstructionId: specId,
      targetNodeId: targetNodeId(specId, part.key),
      fieldLabel: part.label,
      disagreements: [],
      resolvedValue: "reviewed value",
      resolutionExplanation: "Backend marked this part as having a source conflict.",
      resolutionThreshold: 0.7,
    }));
}

function nodeTreeForReconstruction(reconstruction: PublicHistoricalReconstruction) {
  const rootId = targetNodeId(reconstruction.id, "building");
  const levelId = targetNodeId(reconstruction.id, "level:0");
  const massId = targetNodeId(reconstruction.id, "mass");
  const facadeId = targetNodeId(reconstruction.id, "facade");
  const roofId = targetNodeId(reconstruction.id, "roof");
  const groundFloorId = targetNodeId(reconstruction.id, "ground_floor");
  return {
    version: 1,
    source: "backend-reconstruction-spec",
    rootNodeIds: [rootId],
    nodes: {
      [rootId]: { id: rootId, type: "building", parentId: null, children: [levelId] },
      [levelId]: {
        id: levelId,
        type: "level",
        parentId: rootId,
        children: [massId, facadeId, groundFloorId, roofId],
      },
      [massId]: { id: massId, type: "mass", parentId: levelId, children: [] },
      [facadeId]: { id: facadeId, type: "facade", parentId: levelId, children: [] },
      [groundFloorId]: {
        id: groundFloorId,
        type: "ground_floor",
        parentId: levelId,
        children: [],
      },
      [roofId]: { id: roofId, type: "roof", parentId: levelId, children: [] },
    },
  };
}

async function blockSubgraphForSpec(
  context: GraphqlContext,
  focusSpec: ReconstructionSpecShape,
): Promise<{ reconstructionId: string; neighbors: Array<{ relation: string; strength: number; reconstruction: PublicHistoricalReconstruction }> }> {
  const specId = stringField(focusSpec.specId, "spec:unknown");
  const blockId = stringField(focusSpec.blockId);
  const specs = await context.reconstruction.listReconstructionSpecs(context.defaultTenant, {
    pageSize: 50,
  });
  const neighbors = await Promise.all(
    specs
      .filter((spec) => stringField(spec.specId) !== specId)
      .filter((spec) => !blockId || stringField(spec.blockId) === blockId)
      .slice(0, 8)
      .map(async (spec) => ({
        relation: "same_block_as",
        strength: 0.8,
        reconstruction: await reconstructionFromSpec(context, spec),
      })),
  );
  return { reconstructionId: specId, neighbors };
}

async function resolveReconstructionDossier(
  context: GraphqlContext,
  reconstructionId: string,
) {
  const spec = await getSpecForReconstruction(context, reconstructionId);
  const reconstruction = await reconstructionFromSpec(context, spec);
  const evidence = evidenceFromSpec(spec);
  const conflicts = conflictsFromSpec(spec);
  const blockSubgraph = await blockSubgraphForSpec(context, spec);
  return {
    reconstruction,
    evidence,
    conflicts,
    blockSubgraph,
    nodeTree: nodeTreeForReconstruction(reconstruction),
    summary:
      evidence.totalCount > 0
        ? `${evidence.totalCount} backend source item${evidence.totalCount === 1 ? "" : "s"} ${
            evidence.totalCount === 1 ? "supports" : "support"
          } this reconstruction.`
        : "Backend ReconstructionSpec loaded; no source records are attached to this spec yet.",
    debug: {
      source: "ReconstructionService.GetReconstructionSpec",
      requestedId: reconstructionId,
      resolvedSpecId: reconstruction.id,
      specVersion: spec.specVersion ?? spec.version ?? null,
    },
  };
}

const coreTypeDefs = /* GraphQL */ `
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

    type SearchPlaceRef {
      id: ID!
      name: String!
    }

    type Place {
      id: ID!
      name: String!
      placeType: String!
      centroid: LatLng
      confidence: Float!
      temporalStatus: String!
    }

    type Signal {
      id: ID!
      signalKind: String!
      title: String!
      summary: String!
      publishedAt: DateTime
      relativeTimeLabel: String
      confidence: Float!
      place: SearchPlaceRef
    }

    type SpatialEvent {
      id: ID!
      title: String!
      summary: String!
      occurredAt: DateTime
      confidence: Float!
      place: SearchPlaceRef
    }

    type HistoricalReconstruction {
      id: ID!
      civicObjectId: ID!
      name: String!
      description: String!
      position: LatLng!
      footprint: ReconstructionFootprint!
      heightMeters: Float!
      bearingDegrees: Float!
      confidence: Float!
      facadeConfidence: Float
      roofConfidence: Float
      groundFloorConfidence: Float
      roofForm: String
      timeStart: DateTime
      timeEnd: DateTime
      geometryUrl: String
      geometryFormat: String
      foundryAssetUrl: String
      sources: [Source!]!
    }

    type ReconstructionFootprint {
      widthMeters: Float!
      depthMeters: Float!
    }

    type Source {
      id: ID!
      name: String!
      homepageUrl: String
      sourceType: String!
      publicUseTerms: String
      trustTier: String!
      lastChecked: DateTime
      knownLimits: [String!]!
      containsPersonalData: Boolean!
    }

    type EvidenceItem {
      id: ID!
      reconstructionId: ID!
      source: Source!
      evidenceType: String!
      targetNodeId: String
      confidence: Float!
      thumbnailUrl: String
      summary: String
      sourceDateLabel: String
    }

    type EvidenceBundle {
      reconstructionId: ID!
      items: [EvidenceItem!]!
      totalCount: Int!
    }

    type MergeDisagreement {
      source: Source!
      statedValue: String!
      confidence: Float!
      evidenceItemId: ID!
    }

    type MergeConflict {
      id: ID!
      reconstructionId: ID!
      targetNodeId: String!
      fieldLabel: String!
      disagreements: [MergeDisagreement!]!
      resolvedValue: String!
      resolutionExplanation: String!
      resolutionThreshold: Float!
    }

    type BlockSubgraph {
      reconstructionId: ID!
      neighbors: [BlockNeighbor!]!
    }

    type BlockNeighbor {
      reconstruction: HistoricalReconstruction!
      relation: String!
      strength: Float!
    }

    type ReconstructionDossier {
      reconstruction: HistoricalReconstruction!
      evidence: EvidenceBundle!
      conflicts: [MergeConflict!]!
      blockSubgraph: BlockSubgraph!
      nodeTree: JSON!
      summary: String!
      debug: JSON
    }

    type SavedReconstruction {
      id: ID!
      reconstructionId: ID!
      year: Int!
      shareUrl: String!
      savedAt: DateTime!
      contributorEmailDigest: String
    }

    input SaveReconstructionInput {
      reconstructionId: ID!
      year: Int!
      contributorEmail: String
      caption: String
    }

    """
    Shape returned by searchAtlas and civicResearch alike. The sidecar
    exposes typed buckets even when Axum receives lower-level
    theseus_search priorKnowledge/newEvidence records.
    """
    type SearchResults {
      query: String!
      totalResultCount: Int!
      reranked: Boolean!
      acceptedConfidenceFloor: Float!
      inferredTimeRange: TimeRange
      places: [Place!]!
      signals: [Signal!]!
      events: [SpatialEvent!]!
      historicalReconstructions: [HistoricalReconstruction!]!
      sources: [Source!]!
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
      evidenceForReconstruction(reconstructionId: ID!): EvidenceBundle!
      conflictsForReconstruction(reconstructionId: ID!): [MergeConflict!]!
      blockSubgraphForReconstruction(reconstructionId: ID!): BlockSubgraph!
      reconstructionDossier(reconstructionId: ID!): ReconstructionDossier!
      savedReconstruction(id: ID!): SavedReconstruction
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
      saveReconstruction(input: SaveReconstructionInput!): SavedReconstruction!
    }
  `;

const coreResolvers = {
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
      evidenceForReconstruction: async (
        _parent: unknown,
        args: { readonly reconstructionId: string },
        context: GraphqlContext,
      ) => {
        const spec = await getSpecForReconstruction(context, args.reconstructionId);
        return evidenceFromSpec(spec);
      },
      conflictsForReconstruction: async (
        _parent: unknown,
        args: { readonly reconstructionId: string },
        context: GraphqlContext,
      ) => {
        const spec = await getSpecForReconstruction(context, args.reconstructionId);
        return conflictsFromSpec(spec);
      },
      blockSubgraphForReconstruction: async (
        _parent: unknown,
        args: { readonly reconstructionId: string },
        context: GraphqlContext,
      ) => {
        const spec = await getSpecForReconstruction(context, args.reconstructionId);
        return blockSubgraphForSpec(context, spec);
      },
      reconstructionDossier: (
        _parent: unknown,
        args: { readonly reconstructionId: string },
        context: GraphqlContext,
      ) => resolveReconstructionDossier(context, args.reconstructionId),
      savedReconstruction: () => null,
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
      saveReconstruction: () => {
        throw new Error(
          "saveReconstruction is not implemented in the backend persistence layer yet.",
        );
      },
    },
};

// Compose the two modules into one schema. graphql-yoga's createSchema
// (backed by graphql-tools' makeExecutableSchema) accepts an array of
// type-def strings and merges them; we merge resolvers explicitly so
// the per-type maps (Query, Mutation, etc.) combine cleanly.
export const schema = createSchema<GraphqlContext>({
  typeDefs: [coreTypeDefs, eventPlannerTypeDefs],
  resolvers: {
    Query: {
      ...coreResolvers.Query,
      ...eventPlannerResolvers.Query,
    },
    Mutation: {
      ...coreResolvers.Mutation,
      ...eventPlannerResolvers.Mutation,
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
