import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";

import { createYoga } from "@graphql-yoga/node";

import {
  CivicAtlasGrpcClient,
  EventPlannerGrpcClient,
  ReconstructionGrpcClient,
} from "./grpcClient.js";
import { buildContext, schema } from "./schema.js";
import { handlePlannerSse } from "./sse/event-planner-stream.js";
import { handlePorchfestVendorWebhook } from "./webhooks/porchfest-vendor.js";

const PLANNER_SESSION_COOKIE = "porchfest_planner_session";

function parseSessionCookie(req: IncomingMessage | undefined): string {
  const cookieHeader = req?.headers?.cookie;
  if (!cookieHeader) return "";
  for (const part of cookieHeader.split(";")) {
    const [name, ...rest] = part.trim().split("=");
    if (name === PLANNER_SESSION_COOKIE) {
      return decodeURIComponent(rest.join("="));
    }
  }
  return "";
}

async function resolveActor(
  client: EventPlannerGrpcClient,
  defaultTenantId: string,
  sessionToken: string,
): Promise<string | null> {
  if (!sessionToken) return null;
  try {
    const response = await client.resolveSession(
      { tenantId: defaultTenantId },
      sessionToken,
    );
    return response.authenticated ? (response.userId ?? null) : null;
  } catch (err) {
    console.warn("[graphql-server] session resolve failed:", err);
    return null;
  }
}

// Axum's native gRPC port. Default 127.0.0.1:50051 matches the
// civic-atlas-server default; override per environment for staging
// and production via CIVIC_ATLAS_GRPC_URL.
const grpcEndpoint =
  process.env.CIVIC_ATLAS_GRPC_URL ?? "127.0.0.1:50051";
const port = Number(process.env.PORT ?? "4010");
const client = new CivicAtlasGrpcClient(grpcEndpoint);
const eventPlanner = new EventPlannerGrpcClient(grpcEndpoint);
const reconstruction = new ReconstructionGrpcClient(grpcEndpoint);

// graphql-yoga masks resolver errors by default ("Unexpected error.")
// to avoid leaking internal stack traces. In dev that's a debugging
// blocker: real upstream messages from Axum (e.g.,
// "civic research is unavailable: THESEUS_BRIDGE_URL is not configured")
// should reach the panel so the user can see what's wrong. Disable
// masking when NODE_ENV !== 'production'. In production, masking
// stays on per the library default.
const maskErrors = process.env.NODE_ENV === "production";

const defaultTenantId = process.env.CIVIC_ATLAS_DEFAULT_TENANT ?? "flint";

const yoga = createYoga({
  schema,
  // Resolve the session cookie -> planner user id on every request
  // so mutations can attribute writes to a human. The Yoga context
  // hook receives the raw Node `req`; we extract the cookie there.
  context: async (initialContext: { req?: IncomingMessage }) => {
    const sessionToken = parseSessionCookie(initialContext.req);
    const actorUserId = await resolveActor(
      eventPlanner,
      defaultTenantId,
      sessionToken,
    );
    return buildContext(client, eventPlanner, reconstruction, { actorUserId });
  },
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

async function readJsonBody(req: IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = [];
  for await (const chunk of req) {
    chunks.push(chunk as Buffer);
  }
  if (chunks.length === 0) return null;
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8")) as unknown;
  } catch {
    return null;
  }
}

function sessionCookie(value: string, options: { maxAgeSeconds: number }): string {
  // HttpOnly + Secure when the deployment is HTTPS (we leave it on
  // unconditionally and rely on the browser to accept it on
  // localhost when running over plain HTTP in dev — modern browsers
  // permit Secure on localhost). SameSite=Lax keeps the cookie on
  // top-level navigations (the claim flow lands on a new page).
  const flags = [
    `${PLANNER_SESSION_COOKIE}=${encodeURIComponent(value)}`,
    "Path=/",
    "HttpOnly",
    "Secure",
    "SameSite=Lax",
    `Max-Age=${options.maxAgeSeconds}`,
  ];
  return flags.join("; ");
}

// HTTP entry point. The path table is tiny:
//   GET  /sse/event-planner?tenantSlug=flint&eventSlug=porchfest-2026
//     -> handlePlannerSse (long-lived SSE response)
//   POST /auth/claim   { tenantSlug, token } -> set cookie, return user
//   POST /auth/sign-out                       -> clear cookie
//   everything else -> Yoga (POST /graphql, GET /graphql, etc.)
//
// Yoga still owns its own request decoding (multipart uploads, etc.)
// so we only intercept the SSE + auth paths explicitly; everything
// else falls through to yoga.handleNodeRequest.
createServer(async (req: IncomingMessage, res: ServerResponse) => {
  if (req.url && req.url.startsWith("/sse/event-planner")) {
    if (req.method !== "GET") {
      res.statusCode = 405;
      res.setHeader("allow", "GET");
      res.end();
      return;
    }
    const url = new URL(req.url, `http://${req.headers.host ?? "localhost"}`);
    const tenantSlug =
      url.searchParams.get("tenantSlug") ?? defaultTenantId;
    const eventSlug = url.searchParams.get("eventSlug") ?? "";
    if (!eventSlug) {
      res.statusCode = 400;
      res.end("eventSlug query parameter is required");
      return;
    }
    await handlePlannerSse(req, res, { tenantSlug, eventSlug });
    return;
  }
  if (req.url === "/auth/claim" && req.method === "POST") {
    const body = (await readJsonBody(req)) as
      | { tenantSlug?: string; token?: string }
      | null;
    const tenantSlug = body?.tenantSlug ?? defaultTenantId;
    const token = body?.token ?? "";
    if (!token) {
      res.statusCode = 400;
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ error: "token required" }));
      return;
    }
    const result = await eventPlanner.claimInvite(
      { tenantId: tenantSlug },
      token,
    );
    res.setHeader("content-type", "application/json");
    if (!result.success || !result.sessionToken) {
      res.statusCode = 401;
      res.end(
        JSON.stringify({ error: result.error ?? "magic link not accepted" }),
      );
      return;
    }
    res.setHeader(
      "set-cookie",
      sessionCookie(result.sessionToken, {
        maxAgeSeconds: 60 * 60 * 24 * 30,
      }),
    );
    res.statusCode = 200;
    res.end(
      JSON.stringify({
        userId: result.userId,
        displayName: result.displayName,
        email: result.email,
      }),
    );
    return;
  }
  if (req.url === "/webhooks/porchfest-vendor") {
    await handlePorchfestVendorWebhook(req, res, { eventPlanner });
    return;
  }
  if (req.url === "/auth/sign-out" && req.method === "POST") {
    const cookieToken = parseSessionCookie(req);
    if (cookieToken) {
      try {
        await eventPlanner.revokeSession(
          { tenantId: defaultTenantId },
          cookieToken,
        );
      } catch (err) {
        console.warn("[graphql-server] revoke session failed:", err);
      }
    }
    res.setHeader("set-cookie", sessionCookie("", { maxAgeSeconds: 0 }));
    res.setHeader("content-type", "application/json");
    res.statusCode = 200;
    res.end(JSON.stringify({ signedOut: true }));
    return;
  }
  return yoga(req, res);
}).listen(port, () => {
  console.log(
    `Civic Atlas GraphQL sidecar listening on :${port}/graphql -> gRPC ${grpcEndpoint}`,
  );
  console.log(
    `  SSE: :${port}/sse/event-planner?tenantSlug=${defaultTenantId}&eventSlug=<slug>`,
  );
});
