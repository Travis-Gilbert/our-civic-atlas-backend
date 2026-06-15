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
- `crates/civic-atlas-outbox-worker` drains reconstruction projection intents,
  `reconstruction_jobs` pipeline requests, and Porchfest application receipt
  emails. Pipeline jobs write generated ReconstructionSpecs back to PostGIS and
  can auto-approve into part-level `building_parts` plus replayable RustyRed
  projection intents.
- `apps/graphql-server` keeps GraphQL browser semantics unchanged.
- `migrations/0001_tenants_rls.sql` creates tenant tables and row-level
  security defaults.
- `migrations/0002_reconstruction_truth_schema.sql` creates the
  reconstruction truth schema, part-level confidence indexes, generated asset
  tables, corrections, and the projection outbox.
- `migrations/0006_reconstruction_pipeline_jobs.sql` creates the tenant-scoped
  queue for running the procedural reconstruction algorithm.
- `migrations/0007_zoning_envelope_schema.sql` creates the tenant-scoped
  Phase C zoning source, rule, boundary, and buildable-envelope tables.
- `migrations/0008_scenario_branching_schema.sql` creates tenant-scoped
  Phase D scenario and scenario override tables, including seeded `current`
  rows for existing tenants.
- `migrations/0009_kpi_service_schema.sql` creates tenant-scoped Phase E
  multiplier, KPI definition, demographic baseline, and KPI result tables.
- `migrations/0010_scenario_kpi_runtime_queries.sql` adds stable SQL query
  functions for scenario envelope inheritance, envelope deltas, and latest KPI
  bundles.
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
- `THEOREM_SEARCH_URL`: gRPC URL for the Rust-native `theseus_search.v1.SearchService`
  host. This is the search dial for `civic_research`. If unset, the resolver falls
  back to `THESEUS_BRIDGE_URL` (legacy Django bridge) only so production does not
  regress before the Rust endpoint exists; do NOT point this at the Django bridge.
  Leaving both unset surfaces an honest backend-pending state.
- `THESEUS_BRIDGE_URL`: gRPC URL for the embedding-hydration sidecar
  (`theseus_bridge.v1.TheseusBridge`: spacetime topics, `GetBatchSpacetimeEmbeddings`,
  artifact ingest), consumed by the reconstruction-engine and outbox-worker. This now
  scopes to embedding hydration only, not search.
- `RECONSTRUCTION_JOB_BATCH_SIZE`: worker batch size for procedural
  reconstruction jobs, default `4`.
- `SCENE_FOUNDRY_URI_PREFIX`: asset manifest URI prefix while the Blender/Ray
  renderer is queued or stubbed, default `scene-foundry://queued`.
- `CIVIC_ATLAS_ROLE`: runtime role for the Docker image. Leave unset for the
  API server; set to `outbox-worker` for a Railway worker service.
- `RESEND_API_KEY`: Resend API key consumed by the outbox worker for receipt
  delivery and by the API server for organizer-triggered outreach.
- `RESEND_WEBHOOK_SECRET`: Resend/Svix webhook signing secret (`whsec_...`)
  consumed by the API server at `POST /webhooks/resend`.
- `PORCHFEST_EMAIL_PROVIDER`: provider label for planner channel status,
  default `resend`.
- `PORCHFEST_EMAIL_FROM`: verified sender for Porchfest application emails,
  for example `Carriage Town Porchfest <porchfest@cthna.org>`.
- `PORCHFEST_APPLICATION_NOTIFY_TO`: comma- or semicolon-separated organizer
  notification recipients.
- `PORCHFEST_EMAIL_REPLY_TO`: optional Reply-To address for applicant
  confirmations; defaults to the first notify recipient.
- `PORCHFEST_EMAIL_CHANNEL_LABEL`: human-readable planner channel label, for
  example `Railway: civic-atlas-outbox-worker`.
- `PORCHFEST_EMAIL_BATCH_SIZE`: application receipt email batch size, default
  `8`.

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
