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

interface ListPlacesResponse {
  readonly places: CivicObject[];
  readonly nextPageToken: string;
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

export class CivicAtlasGrpcClient {
  constructor(private readonly endpoint: string) {}

  async listPlaces(
    tenantContext: TenantContext,
    pageSize: number,
  ): Promise<readonly CivicObject[]> {
    const response = await fetch(
      `${this.endpoint}/civic_atlas.v1.CivicAtlasService/ListPlaces`,
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-tenant-id": tenantContext.tenantId,
        },
        body: JSON.stringify({
          tenantContext,
          pageSize,
        }),
      },
    );

    if (!response.ok) {
      throw new Error(`Civic Atlas gRPC bridge failed: ${response.status}`);
    }

    const payload = (await response.json()) as ListPlacesResponse;
    return payload.places;
  }

  /**
   * Civic research (gap-driven fractal expansion) entry point.
   *
   * Posts to the Axum `CivicAtlasService.CivicResearch` RPC. Axum fans
   * out to the Theseus bridge via the `theseus-client` crate. Budget +
   * scope are forwarded verbatim as JSON because the harness budget
   * vocabulary evolves faster than the proto contract.
   *
   * Returns the raw shape from Axum: `runId`, `skill`, and
   * `resultsJson` (a JSON string matching the public `SearchResults`
   * GraphQL type). The resolver in `schema.ts` parses `resultsJson`
   * before returning to the browser.
   */
  async civicResearch(
    tenantContext: TenantContext,
    input: CivicResearchInput,
  ): Promise<CivicResearchResponseShape> {
    const response = await fetch(
      `${this.endpoint}/civic_atlas.v1.CivicAtlasService/CivicResearch`,
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-tenant-id": tenantContext.tenantId,
        },
        body: JSON.stringify({
          tenantContext,
          query: input.query,
          budgetJson: input.budget ? JSON.stringify(input.budget) : "",
          scopeJson: input.scope ? JSON.stringify(input.scope) : "",
          sessionId: input.sessionId ?? "",
          folioId: input.folioId ?? "",
        }),
      },
    );

    if (!response.ok) {
      const detail = await response.text().catch(() => "");
      throw new Error(
        `Civic Atlas civic_research call failed: ${response.status} ${detail.slice(0, 200)}`,
      );
    }

    const payload = (await response.json()) as CivicResearchResponseShape;
    return payload;
  }
}

