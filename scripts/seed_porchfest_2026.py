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
  4. Upserts the event_layers row for (tenant_id, slug), preserving its id.
  5. Inserts any missing fixture placements keyed by `source_key`.

Idempotent: running twice with the same fixture inserts only missing seed rows.
Existing fixture rows, planner-created rows, tasks, notes, and bookmarks are
preserved. Pass --reset only when you intentionally want to delete the layer
and all dependent planner state before reseeding.

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
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    import psycopg


SOURCE_KEY_PREFIX = "porchfest-fixture"


def load_psycopg() -> Any:
    try:
        import psycopg
    except ImportError:  # pragma: no cover - install-time hint
        sys.stderr.write(
            "psycopg is required. Install with: pip install 'psycopg[binary]'\n"
        )
        raise
    return psycopg


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
    parser.add_argument(
        "--reset",
        action="store_true",
        help="Delete the existing event layer and all dependent planner state "
        "before reseeding. Default is non-destructive.",
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
    *,
    reset: bool,
) -> str:
    slug = layer["slug"]
    title = layer["title"]
    starts_at = layer.get("starts_at")
    ends_at = layer.get("ends_at")

    if reset:
        cursor.execute(
            "DELETE FROM event_layers WHERE tenant_id = %s AND slug = %s",
            (tenant_id, slug),
        )

    cursor.execute(
        """
        INSERT INTO event_layers (tenant_id, slug, title, starts_at, ends_at)
        VALUES (%s, %s, %s, %s, %s)
        ON CONFLICT (tenant_id, slug) DO UPDATE
          SET title = EXCLUDED.title,
              starts_at = EXCLUDED.starts_at,
              ends_at = EXCLUDED.ends_at,
              updated_at = now()
        RETURNING id
        """,
        (tenant_id, slug, title, starts_at, ends_at),
    )
    row = cursor.fetchone()
    assert row is not None
    return str(row[0])


def seed_source_key(event_slug: str, index: int) -> str:
    return f"{SOURCE_KEY_PREFIX}:{event_slug}:{index:03d}"


def existing_seed_state(
    cursor: psycopg.Cursor,
    tenant_id: str,
    event_layer_id: str,
) -> tuple[int, int]:
    cursor.execute(
        """
        SELECT
          COUNT(*) FILTER (WHERE source_key IS NOT NULL),
          COUNT(*) FILTER (WHERE source_key IS NULL)
        FROM event_placements
        WHERE tenant_id = %s AND event_layer_id = %s
        """,
        (tenant_id, event_layer_id),
    )
    row = cursor.fetchone()
    assert row is not None
    return int(row[0] or 0), int(row[1] or 0)


def insert_placements(
    cursor: psycopg.Cursor,
    tenant_id: str,
    event_layer_id: str,
    event_slug: str,
    placements: list[dict[str, Any]],
) -> tuple[int, int]:
    if not placements:
        return 0, 0

    inserted = 0
    skipped = 0
    for index, placement in enumerate(placements):
        cursor.execute(
            """
            INSERT INTO event_placements (
                tenant_id, event_layer_id, category, sublabel, label,
                geometry, status, notes, source_key
            ) VALUES (
                %s, %s, %s, %s, %s,
                ST_GeomFromGeoJSON(%s)::geography, %s, %s, %s
            )
            ON CONFLICT (tenant_id, event_layer_id, source_key)
              WHERE source_key IS NOT NULL
              DO NOTHING
            RETURNING id
            """,
            (
                tenant_id,
                event_layer_id,
                placement["category"],
                placement.get("sublabel") or None,
                placement["label"],
                json.dumps(placement["geometry"]),
                placement.get("status") or "placed",
                placement.get("notes") or None,
                seed_source_key(event_slug, index),
            ),
        )
        if cursor.fetchone() is None:
            skipped += 1
        else:
            inserted += 1
    return inserted, skipped


def main() -> None:
    args = parse_args()
    if not args.database_url:
        raise SystemExit(
            "DATABASE_URL is required (pass --database-url or set the env var)."
        )
    psycopg_module = load_psycopg()

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
    if args.reset:
        print("Reset mode: deleting the existing layer and dependent planner state.")

    with psycopg_module.connect(args.database_url, autocommit=False) as conn:
        with conn.cursor() as cursor:
            tenant_id = resolve_tenant_id(cursor, args.tenant)
            set_tenant_session(cursor, tenant_id)
            event_layer_id = upsert_event_layer(
                cursor,
                tenant_id,
                layer,
                reset=args.reset,
            )
            keyed_count, unkeyed_count = existing_seed_state(
                cursor,
                tenant_id,
                event_layer_id,
            )
            if not args.reset and keyed_count == 0 and unkeyed_count > 0:
                raise SystemExit(
                    f"Event layer {layer['slug']!r} already has "
                    f"{unkeyed_count} unkeyed placement(s). This could be an "
                    "older seed or live planner data, so the non-destructive "
                    "seeder will not duplicate it. Re-run with --reset only "
                    "if you intend to replace the layer, or reconcile the "
                    "existing rows manually."
                )
            inserted, skipped = insert_placements(
                cursor,
                tenant_id,
                event_layer_id,
                layer["slug"],
                placements,
            )
        conn.commit()

    print(
        "Done. "
        f"Inserted {inserted} placement(s), skipped {skipped} existing "
        f"placement(s) under event_layer {event_layer_id}."
    )


if __name__ == "__main__":
    main()
