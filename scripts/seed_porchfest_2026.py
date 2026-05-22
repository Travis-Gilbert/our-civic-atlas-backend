#!/usr/bin/env python3
"""Idempotent seeder for the Carriage Town Porchfest 2026 event layer.

Reads the JSON fixture written by `scripts/kml-to-event-layer.mjs` in
the frontend repo and bulk-inserts it into the backing Postgres so
the Phase 1 read path has data to serve.

Usage:
    DATABASE_URL=postgres://... python3 scripts/seed_porchfest_2026.py \\
        --fixture ../../Open-Flint-Atlas-main-release/src/data/open-flint-atlas/fixtures/porchfest-2026.json \\
        [--tenant flint]

The script:
  1. Connects to Postgres using DATABASE_URL (the same env var the
     Rust server uses).
  2. Resolves the tenant slug ("flint" by default) to a tenants.id uuid.
  3. Sets the per-session GUC `app.tenant_id` so the RLS policies in
     migration 0011 allow the inserts.
  4. Deletes any prior event_layers row with (tenant_id, slug) matching
     the fixture, cascading to its placements and tasks.
  5. Inserts the fresh event_layer + placements.

Idempotent: running twice with the same fixture yields the same state.

Requires the `psycopg` (psycopg3) library:
    pip install "psycopg[binary]"

Wire format for geometry: the JSON fixture carries GeoJSON Point
geometries. PostGIS converts them with `ST_GeomFromGeoJSON`; the
geography column casts from the resulting geometry implicitly.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any

try:
    import psycopg
except ImportError:  # pragma: no cover - install-time hint
    sys.stderr.write(
        "psycopg is required. Install with: pip install 'psycopg[binary]'\n"
    )
    raise


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--fixture",
        required=True,
        help="Path to the porchfest JSON fixture (event_layer + placements).",
    )
    parser.add_argument(
        "--tenant",
        default="flint",
        help="Tenant slug to resolve against the tenants table. Default: flint.",
    )
    parser.add_argument(
        "--database-url",
        default=os.environ.get("DATABASE_URL"),
        help="Postgres connection URL. Defaults to the DATABASE_URL env var.",
    )
    return parser.parse_args()


def load_fixture(path: Path) -> dict[str, Any]:
    with path.open("r", encoding="utf-8") as handle:
        payload = json.load(handle)
    if "event_layer" not in payload or "placements" not in payload:
        raise SystemExit(
            f"Fixture {path} is missing 'event_layer' or 'placements' key."
        )
    return payload


def resolve_tenant_id(cursor: psycopg.Cursor, tenant_slug: str) -> str:
    cursor.execute("SELECT id FROM tenants WHERE slug = %s", (tenant_slug,))
    row = cursor.fetchone()
    if row is None:
        raise SystemExit(
            f"Unknown tenant slug {tenant_slug!r}. Seed the tenants table first."
        )
    return str(row[0])


def set_tenant_session(cursor: psycopg.Cursor, tenant_id: str) -> None:
    # `is_local=false` (set_config last arg) so the GUC persists for
    # the rest of the session, since the seeder spans multiple
    # statements that each need RLS access. The connection is single-
    # use and discarded on exit, so this can't leak.
    cursor.execute(
        "SELECT set_config('app.tenant_id', %s, false)",
        (tenant_id,),
    )


def upsert_event_layer(
    cursor: psycopg.Cursor,
    tenant_id: str,
    layer: dict[str, Any],
) -> str:
    slug = layer["slug"]
    title = layer["title"]
    starts_at = layer.get("starts_at")
    ends_at = layer.get("ends_at")

    # Delete and reinsert (rather than ON CONFLICT update) so the
    # placements and tasks cascade-delete as a clean reset. Tasks are
    # not written by Phase 1 but the cascade is harmless if a Phase 2
    # task list exists when re-seeding.
    cursor.execute(
        "DELETE FROM event_layers WHERE tenant_id = %s AND slug = %s",
        (tenant_id, slug),
    )
    cursor.execute(
        """
        INSERT INTO event_layers (tenant_id, slug, title, starts_at, ends_at)
        VALUES (%s, %s, %s, %s, %s)
        RETURNING id
        """,
        (tenant_id, slug, title, starts_at, ends_at),
    )
    row = cursor.fetchone()
    assert row is not None
    return str(row[0])


def insert_placements(
    cursor: psycopg.Cursor,
    tenant_id: str,
    event_layer_id: str,
    placements: list[dict[str, Any]],
) -> int:
    if not placements:
        return 0

    # psycopg3's executemany is fine for ~hundreds of rows. If the
    # corpus ever grows past a few thousand, switch to COPY.
    cursor.executemany(
        """
        INSERT INTO event_placements (
            tenant_id, event_layer_id, category, sublabel, label,
            geometry, status, notes
        ) VALUES (
            %s, %s, %s, %s, %s,
            ST_GeomFromGeoJSON(%s)::geography, %s, %s
        )
        """,
        [
            (
                tenant_id,
                event_layer_id,
                p["category"],
                p.get("sublabel") or None,
                p["label"],
                json.dumps(p["geometry"]),
                p.get("status") or "placed",
                p.get("notes") or None,
            )
            for p in placements
        ],
    )
    return len(placements)


def main() -> None:
    args = parse_args()
    if not args.database_url:
        raise SystemExit(
            "DATABASE_URL is required (pass --database-url or set the env var)."
        )

    fixture_path = Path(args.fixture).resolve()
    if not fixture_path.exists():
        raise SystemExit(f"Fixture not found: {fixture_path}")

    payload = load_fixture(fixture_path)
    layer = payload["event_layer"]
    placements = payload["placements"]

    print(
        f"Seeding tenant={args.tenant} layer={layer['slug']} "
        f"({len(placements)} placement(s)) from {fixture_path}",
    )

    with psycopg.connect(args.database_url, autocommit=False) as conn:
        with conn.cursor() as cursor:
            tenant_id = resolve_tenant_id(cursor, args.tenant)
            set_tenant_session(cursor, tenant_id)
            event_layer_id = upsert_event_layer(cursor, tenant_id, layer)
            count = insert_placements(cursor, tenant_id, event_layer_id, placements)
        conn.commit()

    print(f"Done. Inserted {count} placement(s) under event_layer {event_layer_id}.")


if __name__ == "__main__":
    main()
