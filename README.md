# Our Civic Atlas Backend

Backend workspace for the Civic Atlas service boundary.

The browser keeps GraphQL. The Node sidecar owns browser-facing GraphQL
resolvers and calls the Rust service boundary over gRPC-Web or Connect-style
service routes. The Axum service composes tenant-scoped PostGIS, RustyRed, and
Theseus bridge clients.

## Current Slice

- `proto/civic_atlas/v1/civic_atlas.proto` defines the Phase 0 service surface.
- `proto/civic_atlas/v1/spacetime_atlas.proto` defines the public
  `spacetime-atlas` Phase 1 endpoint contract.
- `proto/theseus_bridge/v1/bridge.proto` defines the Theseus bridge boundary.
- `crates/civic-atlas-server` exposes tonic gRPC plus a Connect-style JSON
  shim for the first migrated `placesList` resolver.
- `apps/graphql-server` keeps GraphQL browser semantics unchanged.
- `migrations/0001_tenants_rls.sql` creates tenant tables and row-level
  security defaults.
- `crates/civic-atlas-cli` provisions tenant rows and runtime namespace rows in
  one transaction.

## Local Commands

```bash
cargo check --workspace
cargo test --workspace
```

```bash
cd apps/graphql-server
npm install
npm run typecheck
```

## Environment

- `DATABASE_URL`: PostgreSQL/PostGIS connection for tenant provisioning and
  runtime transactions.
- `CIVIC_ATLAS_HTTP_ADDR`: Axum HTTP address, default `127.0.0.1:4001`.
- `CIVIC_ATLAS_GRPC_ADDR`: tonic gRPC address, default `127.0.0.1:50051`.
- `CIVIC_ATLAS_PLACES_FIXTURE`: optional GeoJSON places fixture for the first
  `placesList` migration path.
- `CIVIC_ATLAS_DEFAULT_TENANT`: default fixture tenant, default `flint`.
- `THESEUS_BRIDGE_URL`: gRPC URL for the Theseus bridge process.
