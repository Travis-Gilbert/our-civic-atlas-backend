# Coordination Note: trafficRealtime GraphQL resolver (backend, TR-B1)

To: frontend lane in `Open-Flint-Atlas-main-release` (Claude Code + Codex)
From: 2026-06-06 backend traffic session in `our-civic-atlas-backend-main-release`
Status: **TR-B1 + TR-B2 shipped** (resolver returns the honest fixture; the
road-network table + seed are now in PostGIS, migration validated against a real
postgis container). **TR-B2b** (resolver reads the table) and **TR-B3** (live MDOT
feed) open.

## What shipped

The Axum-native GraphQL surface now answers `trafficRealtime(networkId)`:

| Artifact | Path |
|---|---|
| Resolver + types | `crates/civic-atlas-server/src/graphql/traffic.rs` |
| Module registration | `crates/civic-atlas-server/src/graphql/mod.rs` (`pub mod traffic;` + schema test) |
| Query root merge | `crates/civic-atlas-server/src/graphql/query.rs` (`QueryRoot(..., TrafficQuery)`) |

It returns the schema Extension 8 shape (`TrafficRealtimeSnapshot` with `segments` +
provenance + `summary`), served at **POST `/graphql`** on Axum (`:4001` dev). The
field names + enums (`TrafficEstimateBasis`, `TrafficSourceStatus`,
`TrafficFeedStatus`) match the frontend contract; the `networkId` argument is typed
`ID` so the frontend operation's `$networkId: ID!` validates.

## Honesty (TR-B1 is a fixture, and says so)

No live feed (TR-B3) and no road-network table (TR-B2) exist yet, so the resolver
returns the same 6 real Flint corridors as the frontend dev fixture, every segment
marked `sourceStatus: FIXTURE` and the snapshot `status: FIXTURE_FALLBACK`. The
engine never reports a LIVE source it does not have (unit test
`fixture_snapshot_is_honest` enforces this).

## Effect on the frontend (no change needed)

`useTrafficRealtime` already queries GraphQL first and falls back to the REST shim
only on a schema/network error. Once this resolver is deployed and reachable, the
frontend flips `source: "fallback"` -> `source: "graphql"` automatically. The data
is identical (same honest corridors), so there is no visual change: only the seam
moves to canonical GraphQL. The frontend can drop `{ fallback: true }` to require
the live seam once the backend is confirmed reachable in that environment.

## Verification

- `cargo test -p civic-atlas-server traffic` -> 2 passed (`fixture_snapshot_is_honest`,
  `schema_builds_with_traffic_fields`).
- `traffic.rs` is clippy-clean.
- NOTE (not introduced here): `civic-atlas-server` already has 5 pre-existing
  clippy `-D warnings` errors on `main` (`cast_abs_to_unsigned` in
  `reconstruction.rs:667`; `result_large_err` in `lib.rs:1442`/`1458`; plus
  `too_many_arguments` in the `civic-atlas-reconstruction-engine` dep). So
  `cargo clippy --locked -p civic-atlas-server -- -D warnings` is red on `main`
  independent of traffic. Worth a separate cleanup pass.

## Done: TR-B2 (road-network table)

- **TR-B2 (shipped)**: migration `0019_traffic_road_network.sql` creates the
  tenant-scoped `traffic_segments` table (RLS via `app.tenant_id`,
  `geography(LINESTRING, 4326)`, gist index) and FK-safely seeds the 6 Flint
  corridors for the `flint` tenant. Validated by applying the full chain
  `0001..0019` to a real `postgis` container (seeds 6 valid LineStrings) plus a
  DB-free structural test (`tests/traffic_road_network_schema.rs`). The migration
  is boot-applied (`run_migrations` -> `sqlx::migrate!`), so this validation was
  the deploy-safety gate.

## Next (backend lane)

- **TR-B2b**: wire `trafficRealtime` to READ `traffic_segments` (set
  `app.tenant_id` in a tenant transaction per `tenant_db.rs`, `ST_AsGeoJSON` the
  geometry), mapping rows through the existing diurnal/shaping path, and fall
  back to the embedded fixture (`fixture_snapshot`) on empty/error so it can never
  break. The table columns mirror the resolver's `SeedCorridor`.
- **TR-B3**: wire the MDOT RIDE feed (server-side credential, never in the
  frontend); UPDATE/INSERT `traffic_segments` rows with `source_status: live` +
  snapshot `status: live` for measured segments, lower confidence for inferred
  ones, calibrate against any counts.
