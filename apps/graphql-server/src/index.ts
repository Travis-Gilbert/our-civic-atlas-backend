import { createServer } from "node:http";

import { createYoga } from "@graphql-yoga/node";

import { CivicAtlasGrpcClient } from "./grpcClient.js";
import { buildContext, schema } from "./schema.js";

const endpoint =
  process.env.CIVIC_ATLAS_GRPC_WEB_URL ?? "http://127.0.0.1:4001";
const port = Number(process.env.PORT ?? "4010");
const client = new CivicAtlasGrpcClient(endpoint);

const yoga = createYoga({
  schema,
  context: () => buildContext(client),
  graphqlEndpoint: "/graphql",
});

createServer(yoga).listen(port, () => {
  console.log(`Civic Atlas GraphQL sidecar listening on :${port}/graphql`);
});

