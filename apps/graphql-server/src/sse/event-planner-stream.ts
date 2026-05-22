/**
 * Server-Sent Events fanout for the Porchfest Planner.
 *
 * Phase 2 wires Postgres LISTEN/NOTIFY to a browser-facing SSE
 * stream so every planner sees every edit in near-real-time without
 * polling. The notify trigger in migration 0013_event_planner_notify
 * fires on every INSERT/UPDATE/DELETE on event_placements +
 * event_tasks; this module multiplexes those notifications out to
 * connected clients.
 *
 * Connection model:
 *   - One long-lived `pg.Client` per tenant slug, opened lazily on
 *     the first SSE connect. The client stays open for the lifetime
 *     of the process; `client.on('notification', ...)` fires per
 *     pg_notify.
 *   - Each SSE response holds a reference to a Set of active
 *     listeners; the LISTEN client iterates that Set and writes one
 *     `data: <json>` line per listener per notification.
 *   - A `: ping` comment frame every 25s defeats proxy idle timeouts
 *     (Cloudflare, Vercel edge, nginx defaults).
 *
 * Why this scale of work belongs in the Node sidecar (not Axum):
 *   - GraphQL clients already point at the sidecar for their HTTP
 *     surface; adding `/sse/event-planner` keeps origin/CORS rules
 *     identical.
 *   - SSE is a long-lived HTTP/1.1 response; tonic gRPC works on
 *     HTTP/2 and the framing models don't mix cleanly.
 *
 * Filtering:
 *   - The client passes ?eventSlug=porchfest-2026; we forward every
 *     notification to every listener (the eventSlug filter is
 *     advisory, since the LISTEN channel is already per-tenant).
 *     Future Phase-3 features that scope by event_layer can filter
 *     here by `event_layer_id` matching the slug's uuid.
 */

import type { IncomingMessage, ServerResponse } from "node:http";

import { Client as PgClient } from "pg";

const PING_INTERVAL_MS = 25_000;
const RECONNECT_BACKOFF_MS = 2_000;

interface PlannerNotification {
  readonly op: "INSERT" | "UPDATE" | "DELETE";
  readonly table: string;
  readonly id: string;
  readonly event_layer_id: string;
  readonly tenant_id: string;
  readonly version: number | null;
}

type Listener = (notification: PlannerNotification) => void;

interface TenantStream {
  readonly tenantSlug: string;
  readonly listeners: Set<Listener>;
  client: PgClient | null;
  connecting: Promise<void> | null;
}

const streams = new Map<string, TenantStream>();

function databaseUrl(): string {
  const url = process.env.DATABASE_URL;
  if (!url) {
    throw new Error(
      "DATABASE_URL is required for the planner SSE stream (no upstream LISTEN target).",
    );
  }
  return url;
}

async function ensureListenClient(stream: TenantStream): Promise<void> {
  if (stream.client) return;
  if (stream.connecting) return stream.connecting;

  stream.connecting = (async () => {
    const client = new PgClient({ connectionString: databaseUrl() });
    client.on("error", (err: Error) => {
      console.error(
        `[planner-sse] LISTEN client error for tenant=${stream.tenantSlug}:`,
        err,
      );
      // Drop the client so the next connection attempt rebuilds it.
      stream.client = null;
    });
    client.on("notification", (msg: { channel: string; payload?: string }) => {
      if (!msg.payload) return;
      let parsed: PlannerNotification;
      try {
        parsed = JSON.parse(msg.payload) as PlannerNotification;
      } catch {
        return;
      }
      for (const listener of stream.listeners) {
        try {
          listener(parsed);
        } catch (err) {
          console.warn("[planner-sse] listener threw:", err);
        }
      }
    });
    await client.connect();
    await client.query(`LISTEN event_planner_${stream.tenantSlug}`);
    stream.client = client;
  })();

  try {
    await stream.connecting;
  } finally {
    stream.connecting = null;
  }
}

function getStream(tenantSlug: string): TenantStream {
  let existing = streams.get(tenantSlug);
  if (!existing) {
    existing = {
      tenantSlug,
      listeners: new Set(),
      client: null,
      connecting: null,
    };
    streams.set(tenantSlug, existing);
  }
  return existing;
}

/**
 * Attach an SSE response to the planner stream for the given tenant.
 * The caller has already validated the request (method, query
 * params). This handles headers, the keep-alive ping, and cleanup.
 */
export async function handlePlannerSse(
  req: IncomingMessage,
  res: ServerResponse,
  options: { tenantSlug: string; eventSlug: string },
): Promise<void> {
  res.writeHead(200, {
    "Content-Type": "text/event-stream",
    "Cache-Control": "no-cache, no-transform",
    Connection: "keep-alive",
    "X-Accel-Buffering": "no",
  });

  // Initial comment so the browser opens the EventSource cleanly,
  // followed by a hello event the client can use to confirm wiring.
  res.write(`: connected tenant=${options.tenantSlug} event=${options.eventSlug}\n\n`);
  res.write(`event: hello\ndata: {"ready":true}\n\n`);

  const stream = getStream(options.tenantSlug);

  // Try to bring the LISTEN client up. If Postgres is unreachable,
  // we still keep the SSE response open so the client doesn't
  // hammer-reconnect; the periodic reconnect attempt below brings
  // it back when the DB returns.
  ensureListenClient(stream).catch((err) => {
    console.warn("[planner-sse] initial connect failed:", err);
  });

  const listener: Listener = (notification) => {
    if (res.writableEnded) return;
    res.write(`event: planner_change\ndata: ${JSON.stringify(notification)}\n\n`);
  };
  stream.listeners.add(listener);

  const pingTimer = setInterval(() => {
    if (res.writableEnded) return;
    res.write(`: ping ${Date.now()}\n\n`);
  }, PING_INTERVAL_MS);

  const reconnectTimer = setInterval(() => {
    if (!stream.client && !stream.connecting) {
      ensureListenClient(stream).catch(() => {
        // Already logged in ensureListenClient.
      });
    }
  }, RECONNECT_BACKOFF_MS);

  const cleanup = () => {
    clearInterval(pingTimer);
    clearInterval(reconnectTimer);
    stream.listeners.delete(listener);
    // We deliberately keep the LISTEN client open even when the last
    // browser disconnects: the next reconnect is faster (no fresh
    // socket+LISTEN). If memory pressure later matters, drop the
    // client here when listeners.size === 0.
    if (!res.writableEnded) res.end();
  };

  req.on("close", cleanup);
  req.on("error", cleanup);
}
