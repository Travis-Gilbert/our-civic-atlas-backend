#!/usr/bin/env python3
"""Mint a magic-link invite for a Porchfest planner.

Generates a 32-byte cleartext token, stores its SHA-256 hash in
event_planner_invites, and prints a magic link to stdout. Paste the
link into Slack / SMS / email and the planner clicks it to sign in.

Usage:
    DATABASE_URL=postgres://...                  \\
      python3 scripts/invite_planner.py          \\
        --email derek@example.org                \\
        --display-name "Derek"                   \\
        [--tenant flint]                         \\
        [--base-url https://porchfest.ourcivicatlas.org] \\
        [--expires-hours 48]                     \\
        [--invited-by <uuid>]

Default base URL is read from PORCHFEST_BASE_URL or falls back to
http://porchfest.localhost:3000 (for local dev with the /etc/hosts
shortcut from the middleware).

Requires the `psycopg` (psycopg3) library:
    pip install 'psycopg[binary]'
"""

from __future__ import annotations

import argparse
import hashlib
import os
import secrets
import sys
from datetime import datetime, timedelta, timezone
from typing import Optional

try:
    import psycopg
except ImportError:
    sys.stderr.write(
        "psycopg is required. Install with: pip install 'psycopg[binary]'\n"
    )
    raise


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--email", required=True, help="Planner email address.")
    parser.add_argument(
        "--display-name",
        required=True,
        help='Display name shown to other planners (e.g., "Derek").',
    )
    parser.add_argument(
        "--tenant",
        default="flint",
        help="Tenant slug to scope the invite. Default: flint.",
    )
    parser.add_argument(
        "--base-url",
        default=os.environ.get(
            "PORCHFEST_BASE_URL", "http://porchfest.localhost:3000"
        ),
        help="Public origin to include in the magic link. Default: "
        "PORCHFEST_BASE_URL env or http://porchfest.localhost:3000.",
    )
    parser.add_argument(
        "--expires-hours",
        type=int,
        default=48,
        help="How long (hours) the invite stays valid. Default: 48.",
    )
    parser.add_argument(
        "--invited-by",
        default=None,
        help="Optional uuid of the planner doing the inviting (audit trail).",
    )
    parser.add_argument(
        "--database-url",
        default=os.environ.get("DATABASE_URL"),
        help="Postgres URL; defaults to DATABASE_URL env.",
    )
    return parser.parse_args()


def hash_token(cleartext: str) -> str:
    return hashlib.sha256(cleartext.encode("utf-8")).hexdigest()


def resolve_tenant_id(cursor: "psycopg.Cursor", tenant_slug: str) -> str:
    cursor.execute("SELECT id FROM tenants WHERE slug = %s", (tenant_slug,))
    row = cursor.fetchone()
    if row is None:
        raise SystemExit(
            f"Unknown tenant slug {tenant_slug!r}. Seed the tenants table first."
        )
    return str(row[0])


def main() -> None:
    args = parse_args()
    if not args.database_url:
        raise SystemExit(
            "DATABASE_URL is required (pass --database-url or set the env var)."
        )

    # 32 bytes -> 64 hex chars. `secrets.token_hex(32)` is the
    # textbook entropy source; the bytes are never reused.
    cleartext = secrets.token_hex(32)
    token_hash = hash_token(cleartext)
    expires_at = datetime.now(tz=timezone.utc) + timedelta(
        hours=args.expires_hours
    )

    with psycopg.connect(args.database_url, autocommit=False) as conn:
        with conn.cursor() as cursor:
            tenant_id = resolve_tenant_id(cursor, args.tenant)
            cursor.execute(
                "SELECT set_config('app.tenant_id', %s, false)", (tenant_id,)
            )
            cursor.execute(
                """
                INSERT INTO event_planner_invites
                  (token_hash, tenant_id, email, display_name, invited_by, expires_at)
                VALUES (%s, %s, %s, %s, %s, %s)
                """,
                (
                    token_hash,
                    tenant_id,
                    args.email,
                    args.display_name,
                    args.invited_by,
                    expires_at,
                ),
            )
        conn.commit()

    magic_link = (
        f"{args.base_url.rstrip('/')}/open-flint-atlas/plan/auth/claim/{cleartext}"
    )
    print()
    print(f"Invited {args.display_name} <{args.email}> to tenant {args.tenant}.")
    print(f"Expires at {expires_at.isoformat()}")
    print("Magic link:")
    print(f"  {magic_link}")
    print()
    print("Paste this into Slack / SMS / email. Single-use, expires once consumed.")


if __name__ == "__main__":
    main()
