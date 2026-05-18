import DataLoader from "dataloader";
import { createSchema } from "graphql-yoga";

import { CivicAtlasGrpcClient, type CivicObject } from "./grpcClient.js";

export interface GraphqlContext {
  readonly placesLoader: DataLoader<string, readonly CivicObject[]>;
}

export function buildContext(client: CivicAtlasGrpcClient): GraphqlContext {
  return {
    placesLoader: new DataLoader(async (tenantIds: readonly string[]) =>
      Promise.all(
        tenantIds.map((tenantId) =>
          client.listPlaces({ tenantId, atlasNodeId: `atlas:${tenantId}` }, 500),
        ),
      ),
    ),
  };
}

export const schema = createSchema<GraphqlContext>({
  typeDefs: /* GraphQL */ `
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

    type Query {
      health: Health!
      placesList(tenantId: ID!): [CivicObject!]!
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
  },
});

