import { createServer } from "node:http";

import { createYoga } from "@graphql-yoga/node";

import { CivicAtlasGrpcClient } from "./grpcClient.js";
import { buildContext, schema } from "./schema.js";

// Axum's native gRPC port. Default 127.0.0.1:50051 matches the
// civic-atlas-server default; override per environment for staging
// and production via CIVIC_ATLAS_GRPC_URL.
const grpcEndpoint =
  process.env.CIVIC_ATLAS_GRPC_URL ?? "127.0.0.1:50051";
const port = Number(process.env.PORT ?? "4010");
const client = new CivicAtlasGrpcClient(grpcEndpoint);

// graphql-yoga masks resolver errors by default ("Unexpected error.")
// to avoid leaking internal stack traces. In dev that's a debugging
// blocker: real upstream messages from Axum (e.g.,
// "civic research is unavailable: THESEUS_BRIDGE_URL is not configured")
// should reach the panel so the user can see what's wrong. Disable
// masking when NODE_ENV !== 'production'. In production, masking
// stays on per the library default.
const maskErrors = process.env.NODE_ENV === "production";

const yoga = createYoga({
  schema,
  context: () => buildContext(client),
  graphqlEndpoint: "/graphql",
  maskedErrors: maskErrors,
  cors: {
    origin: process.env.CIVIC_ATLAS_GRAPHQL_CORS_ORIGIN?.split(",") ?? [
      "http://localhost:3000",
      "http://127.0.0.1:3000",
    ],
    credentials: true,
    methods: ["GET", "POST", "OPTIONS"],
  },
});

createServer(yoga).listen(port, () => {
  console.log(
    `Civic Atlas GraphQL sidecar listening on :${port}/graphql -> gRPC ${grpcEndpoint}`,
  );
});

