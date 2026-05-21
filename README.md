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
- `proto/civic_atlas/v1/reconstruction.proto` and
  `proto/civic_atlas/v1/reconstruction_service.proto` define the Phase 2
  ReconstructionSpec and service contract. The tonic `ReconstructionService`
  persists specs and generated assets through PostGIS, projects approved specs
  into part-level `building_parts`, and enqueues replayable RustyRed projection
  intents without making RustyRed the source of truth.
- `proto/theseus_bridge/v1/bridge.proto` defines the Theseus bridge boundary.
- `crates/civic-atlas-server` exposes tonic gRPC plus a Connect-style JSON
  shim for the first migrated `placesList` resolver.
- `crates/civic-atlas-reconstruction-engine` implements the eight-stage
  procedural reconstruction algorithm as typed, independently testable stages:
  evidence assembly, direct extraction, block subgraph construction, spacetime
  embedding hydration, Pairformer-ready prior inference, merge, asset manifest
  generation, and persistence handoff. It also projects each
  `ReconstructionSpec` into a Pascal-style flat node tree for editor,
  correction, texture-provenance, and per-part dossier targeting while keeping
  PostGIS specs as truth.
- `crates/civic-atlas-outbox-worker` drains both reconstruction projection
  intents and `reconstruction_jobs` pipeline requests. Pipeline jobs write
  generated ReconstructionSpecs back to PostGIS and can auto-approve into
  part-level `building_parts` plus replayable RustyRed projection intents.
- `apps/graphql-server` keeps GraphQL browser semantics unchanged.
- `migrations/0001_tenants_rls.sql` creates tenant tables and row-level
  security defaults.
- `migrations/0002_reconstruction_truth_schema.sql` creates the
  reconstruction truth schema, part-level confidence indexes, generated asset
  tables, corrections, and the projection outbox.
- `migrations/0006_reconstruction_pipeline_jobs.sql` creates the tenant-scoped
  queue for running the procedural reconstruction algorithm.
- `crates/civic-atlas-cli` provisions tenant rows and runtime namespace rows in
  one transaction, validates ReconstructionSpec JSON, and submits specs for
  review.

## Local Commands

```bash
cargo check --workspace
cargo test --workspace
cargo clippy --locked -p civic-atlas-cli -- -D warnings
cargo clippy --locked -p civic-atlas-server -- -D warnings
```

```bash
cd apps/graphql-server
npm install
npm run proto:check
npm run proto:shape
npm run typecheck
```

`npm run proto:generate` regenerates checked-in TypeScript (`ts-proto`) and
Python (`grpcio-tools`) artifacts. `npm run proto:check` regenerates those
artifacts and fails if the checked-in output is stale.

```bash
cargo run -p civic-atlas-cli -- spec validate path/to/spec.json
cargo run -p civic-atlas-cli -- spec submit path/to/spec.json
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
- `RECONSTRUCTION_JOB_BATCH_SIZE`: worker batch size for procedural
  reconstruction jobs, default `4`.
- `SCENE_FOUNDRY_URI_PREFIX`: asset manifest URI prefix while the Blender/Ray
  renderer is queued or stubbed, default `scene-foundry://queued`.

## Related Repos

- `Open-Flint-Atlas-main-release`: public web app and resident-facing read
  surface. It consumes projected atlas/read-model data and should not own
  canonical reconstruction writes.
- `civic-atlas-ingest`: bursty Python/Ray-on-RunPod lane for corpus ingestion,
  building-head training/inference, and Blender Scene Foundry rendering. It is
  intentionally separate from this long-running Rust/PostGIS service boundary
  because GPU, data-ingest, and Blender toolchains co-evolve there.
- `Index-API`: Theseus upstream for spacetime embeddings, Pairformer/GNN
  architecture source material, and graph-native reasoning services.
