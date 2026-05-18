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
}

