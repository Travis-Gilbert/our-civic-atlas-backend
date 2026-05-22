/**
 * Stripe-to-planner webhook intake.
 *
 * The CTHNA porchfest-2026 site fires this after a successful
 * vendor checkout. The atlas sidecar verifies the HMAC, forwards
 * the payload to the Rust `IntakePendingVendor` RPC, and the Rust
 * side inserts a placement with status='pending_placement'. The
 * existing notify_event_planner_change() trigger fans the new pin
 * out over SSE so the planning team sees it within seconds.
 *
 * Wire contract (CTHNA side must match):
 *   POST /webhooks/porchfest-vendor
 *   Headers:
 *     content-type: application/json
 *     x-porchfest-signature: hex(hmac_sha256(body, secret))
 *   Body (utf-8 json):
 *     {
 *       "tenantSlug": "flint",
 *       "eventLayerSlug": "porchfest-2026",
 *       "businessName": "BBQ Steve",
 *       "vendorTier": "pop_up" | "food_truck",
 *       "contactName": "Steve Z.",
 *       "contactEmail": "steve@example.org",
 *       "needs": "power, water",
 *       "defaultLng": -83.6972,
 *       "defaultLat": 43.0184,
 *       "idempotencyKey": "cs_test_..."
 *     }
 *
 * Security:
 *   - The shared secret lives in PORCHFEST_WEBHOOK_SECRET on both
 *     repos. NEVER log the secret. NEVER include the body in error
 *     responses to the caller.
 *   - HMAC verification uses raw request bytes (NOT a re-serialized
 *     JSON object) because the CTHNA side controls the byte form.
 *   - constant-time comparison via crypto.timingSafeEqual.
 *
 * Idempotency:
 *   - The `idempotencyKey` (Stripe checkout session id) is stored
 *     inside the placement's notes field with a `[stripe] <key>`
 *     prefix. Repeat webhooks (Stripe retries) match the existing
 *     row and return created=false without writing again.
 */

import { createHmac, timingSafeEqual } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";

import type { EventPlannerGrpcClient, TenantContext } from "../grpcClient.js";

const SIGNATURE_HEADER = "x-porchfest-signature";
const DEFAULT_TENANT_SLUG = "flint";
const DEFAULT_EVENT_SLUG = "porchfest-2026";

interface VendorPayload {
  readonly tenantSlug?: string;
  readonly eventLayerSlug?: string;
  readonly businessName?: string;
  readonly vendorTier?: string;
  readonly contactName?: string;
  readonly contactEmail?: string;
  readonly needs?: string;
  readonly defaultLng?: number;
  readonly defaultLat?: number;
  readonly idempotencyKey?: string;
}

async function readBodyBytes(req: IncomingMessage): Promise<Buffer> {
  const chunks: Buffer[] = [];
  for await (const chunk of req) {
    chunks.push(chunk as Buffer);
  }
  return Buffer.concat(chunks);
}

function verifySignature(bodyBytes: Buffer, providedHex: string | undefined): boolean {
  if (!providedHex) return false;
  const secret = process.env.PORCHFEST_WEBHOOK_SECRET ?? "";
  if (!secret) {
    console.warn(
      "[porchfest-vendor] PORCHFEST_WEBHOOK_SECRET is unset — rejecting all webhooks",
    );
    return false;
  }
  const expected = createHmac("sha256", secret).update(bodyBytes).digest("hex");
  const provided = providedHex.trim().toLowerCase().replace(/^sha256=/, "");
  if (provided.length !== expected.length) return false;
  return timingSafeEqual(
    Buffer.from(provided, "hex"),
    Buffer.from(expected, "hex"),
  );
}

function sendJson(
  res: ServerResponse,
  status: number,
  payload: Record<string, unknown>,
): void {
  res.statusCode = status;
  res.setHeader("content-type", "application/json");
  res.end(JSON.stringify(payload));
}

export async function handlePorchfestVendorWebhook(
  req: IncomingMessage,
  res: ServerResponse,
  options: { eventPlanner: EventPlannerGrpcClient },
): Promise<void> {
  if (req.method !== "POST") {
    res.statusCode = 405;
    res.setHeader("allow", "POST");
    res.end();
    return;
  }

  let bodyBytes: Buffer;
  try {
    bodyBytes = await readBodyBytes(req);
  } catch (err) {
    console.warn("[porchfest-vendor] body read failed:", err);
    sendJson(res, 400, { error: "could not read request body" });
    return;
  }

  const signatureHeader = req.headers[SIGNATURE_HEADER];
  const providedHex = Array.isArray(signatureHeader)
    ? signatureHeader[0]
    : signatureHeader;
  if (!verifySignature(bodyBytes, providedHex)) {
    sendJson(res, 401, { error: "invalid signature" });
    return;
  }

  let payload: VendorPayload;
  try {
    payload = JSON.parse(bodyBytes.toString("utf8")) as VendorPayload;
  } catch {
    sendJson(res, 400, { error: "body is not valid JSON" });
    return;
  }

  const businessName = (payload.businessName ?? "").trim();
  const idempotencyKey = (payload.idempotencyKey ?? "").trim();
  if (!businessName || !idempotencyKey) {
    sendJson(res, 400, {
      error: "businessName and idempotencyKey are required",
    });
    return;
  }

  const lng = Number(payload.defaultLng);
  const lat = Number(payload.defaultLat);
  if (!Number.isFinite(lng) || !Number.isFinite(lat)) {
    sendJson(res, 400, {
      error: "defaultLng / defaultLat must be finite numbers",
    });
    return;
  }

  const tenantSlug = (payload.tenantSlug ?? DEFAULT_TENANT_SLUG).trim();
  const eventLayerSlug =
    (payload.eventLayerSlug ?? DEFAULT_EVENT_SLUG).trim();

  const tenantContext: TenantContext = { tenantId: tenantSlug };

  try {
    const result = await options.eventPlanner.intakePendingVendor(
      tenantContext,
      {
        eventLayerSlug,
        businessName,
        vendorTier: payload.vendorTier ?? "",
        contactName: payload.contactName ?? "",
        contactEmail: payload.contactEmail ?? "",
        needs: payload.needs ?? "",
        defaultLng: lng,
        defaultLat: lat,
        idempotencyKey,
      },
    );
    sendJson(res, 200, {
      created: result.created,
      placementId: result.placementId,
    });
  } catch (err) {
    // Don't leak internal error messages to a webhook caller — the
    // CTHNA side just needs a non-2xx to know to retry. Internal
    // detail goes to the sidecar log for debugging.
    console.error("[porchfest-vendor] intake failed:", err);
    sendJson(res, 502, { error: "intake failed" });
  }
}
