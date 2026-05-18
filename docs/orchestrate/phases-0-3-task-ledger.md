# Orchestrate Plan: Civic Atlas Backend, RustyRed Geotemporal, ReconstructionSpec, Carriage Town

## Executive Summary

- Goal: preserve browser GraphQL while introducing a tenant-scoped Civic Atlas backend between the public atlas and Theseus.
- Intent: keep PostGIS as spatial truth, RustyRed as tenant-scoped hot graph/geotemporal acceleration, and Theseus as an upstream knowledge service.
- Summary of work: Phase 0 creates the service skeleton and migration boundary; Phase 1 adds RustyRed geotemporal composition behind a public `spacetime-atlas` endpoint; Phase 2 adds ReconstructionSpec and PostGIS truth tables; Phase 3 ships the Carriage Town pilot through reviewed specs and generated assets.

## Naming Decision

The public Phase 1 endpoint/proto/service is `spacetime-atlas`, implemented as
`proto/civic_atlas/v1/spacetime_atlas.proto` with `SpacetimeAtlasService`.
The internal RustyRed primitive remains `thg-geotemporal` because it is the
lower-level composition of H3 space and node time intervals.

## Current Condition

| Surface | Current state | Evidence |
|---|---|---|
| Public atlas frontend | Next.js app on `Open-Flint-Atlas-main-release`; browser GraphQL exists but is not yet the consumed path. | `src/lib/api/graphql/client.ts`, `docs/notes/session-2026-05-17-graphql-and-3d-buildings.open.md` |
| Theseus | Django service has a scaffolded Strawberry endpoint that should move behind the Civic Atlas backend. | `Index-API/apps/open_flint_atlas_graphql/` |
| RustyRed | `theseus_native/crates/thg-core` already has H3 `SpatialIndex`, tenant-aware server routes, and `GraphStore`. | `theseus_native/crates/thg-core/src/spatial.rs`, `graph_store.rs` |
| Backend repo | New workspace created here. | `Cargo.toml`, `proto/`, `crates/`, `apps/graphql-server/` |

## UI Visual Milestone

| Gate | Requirement | Evidence/validator | Status |
|---|---|---|---|
| Runtime complete | New route stack responds and existing atlas route remains usable. | cargo/npm checks, route smoke. | planned |
| Product complete | Carriage Town scene is equal-or-better than existing deck.gl/Lost Flint baseline. | Desktop/mobile screenshots, Do Not Downgrade review. | planned |
| Vision complete | Full tenant-scoped reconstruction pipeline works from spec approval to rendered per-part confidence. | Final gate report. | planned |
| Baseline capture | Current `/open-flint-atlas` baseline and Lost Flint route state captured. | Existing visual evidence plus fresh screenshots before UI replacement. | planned |
| Do Not Downgrade | MapLibre/deck.gl base stays primary; R3F is selective overlay only. | Visual gate before primary path switch. | planned |
| Reversible boundary | GraphQL feature flag and route-specific Carriage Town surface preserve rollback. | Env flag, old endpoint remains alive. | planned |

## Checklist

| ID | Task | Grounding | Route | Acceptance criteria | Validation | Risk | Status |
|---|---|---|---|---|---|---|---|
| OCA-BE-P0-001 | Create backend workspace and Phase 0 proto contracts. | User spec Phase 0 | execute | Cargo workspace contains Civic Atlas and Theseus bridge proto generation. | `cargo check --workspace` | Contract drift before services exist. | partial |
| OCA-BE-P0-002 | Implement Axum + tonic server skeleton. | `crates/civic-atlas-server` | execute | `Health`, `ListPlaces`, `GetPlace`, `GetNode`, `GetDossier`, and `ResolveTenant` enforce tenant context. | unit tests, curl smoke | Browser path bypasses tenant enforcement. | partial |
| OCA-BE-P0-003 | Add Node GraphQL sidecar. | `apps/graphql-server` | execute | `placesList` resolver calls the backend service boundary and uses DataLoader per request. | `npm run typecheck` | Browser contract changes by accident. | partial |
| OCA-BE-P0-004 | Add tenant PostGIS migration and CLI provisioning. | `migrations/0001_tenants_rls.sql`, `civic-atlas-cli` | execute | Tenant rows and RustyRed namespace rows are provisioned in one transaction. | SQL review, CLI compile | Second tenant leaks Flint data. | partial |
| OCA-BE-P0-005 | Add Theseus bridge sidecar skeleton in Index-API. | `apps/notebook/grpc/bridge_server.py` | execute | Bridge process boundary wraps existing endpoints without making Theseus the atlas backend. | import/check smoke | Theseus remains accidental atlas backend. | planned |
| OCA-BE-P0-006 | Re-point frontend GraphQL URL behind a feature flag. | `Open-Flint-Atlas-main-release/src/lib/api/graphql/client.ts` | execute | Browser-facing GraphQL client URL can switch to Node sidecar; old endpoint remains default. | `npm run typecheck` | Cutover breaks current public route. | planned |
| OCA-BE-P1-001 | Extend RustyRed `GraphStore` with node intervals. | `theseus_native/crates/thg-core` | execute | Standard `t_start_ms` and `t_end_ms` properties parse consistently. | cargo tests | Time leaks into H3 index. | partial |
| OCA-BE-P1-002 | Add `thg-geotemporal` crate. | `theseus_native/crates/thg-geotemporal` | execute | Tenant-scoped query composes H3 spatial result with node intervals. | cargo tests | Cross-tenant spatial leakage. | partial |
| OCA-BE-P1-003 | Add Civic Atlas `spacetime-atlas` endpoint/service implementation. | backend proto/server | execute | `proto/civic_atlas/v1/spacetime_atlas.proto` exposes `SpacetimeAtlasService` with `GetViewportAtTime`, `GetBlockSubgraph`, `GetParcelHistory`, and `GetNearbyArtifacts`; implementation composes RustyRed traversal. | integration tests | Query shape diverges from frontend needs. | partial |
| OCA-BE-P2-001 | Add ReconstructionSpec proto and generated type gates. | backend proto + TS/Python generation | execute | Rust proto generation includes ReconstructionSpec and ReconstructionService; checked-in TypeScript (`ts-proto`) and Python (`grpcio-tools`) artifacts regenerate cleanly and stale output fails CI. | `cargo test --workspace`, `npm run proto:check`, `npm run proto:shape` | Spec versions diverge. | done |
| OCA-BE-P2-002 | Add PostGIS reconstruction truth schema. | migrations | execute | Immutable approved specs, building parts, artifacts, anchors, generated assets, corrections, and RLS exist. | migration tests | RustyRed originates truth. | done |
| OCA-BE-P2-003 | Implement approval projection job. | backend jobs | execute | Approved spec writes PostGIS parts first, then idempotently projects summary to RustyRed. | replay tests | Partial projection corrupts graph state. | partial |
| OCA-BE-P2-004 | Implement procedural reconstruction engine. | algorithm spec + backend crate | execute | Eight stages are represented as typed contracts: evidence bundle, direct extraction, block subgraph, spacetime embeddings, Pairformer-ready prior inference, direct-wins merge, Scene Foundry manifest, and PostGIS persistence handoff. | `cargo test -p civic-atlas-reconstruction-engine` | Algorithm remains a handwritten seed path only. | partial |
| OCA-BE-P2-005 | Add tenant-scoped reconstruction job queue. | worker + migration | execute | `reconstruction_jobs` can run the full engine from parcel/time slice, write an in-review spec, and optionally auto-approve into parts plus projection outbox. | worker compile, migration test | Pipeline cannot run outside a human curator call. | partial |
| OCA-BE-P3-001 | Create Blender primitive library repo. | new repo | execute | Eight parameterized archetypes exist and are addressed by spec fields. | asset metadata validation | Assets become hand-authored one-offs. | planned |
| OCA-BE-P3-002 | Add Modal Scene Foundry renderer. | Modal app | execute | `render_spec_to_glb` uploads deterministic GLB asset path by tenant/spec/version/hash. | Modal smoke | Asset generation lacks replayability. | planned |
| OCA-BE-P3-003 | Add Carriage Town frontend route. | public atlas route | execute | Route fetches 20 specs through GraphQL and renders GLBs over MapLibre/deck.gl with per-part confidence. | browser screenshots | R3F replaces the map base. | planned |

## Phase Report Requirements

After each phase, publish an Orchestrate Report with:

- phase gate verification evidence
- schema drift check
- multi-tenancy probe
- explicit deviations from this spec

## Recovery Evidence

Latest validation after the `spacetime-atlas` endpoint rename:

| Check | Result |
|---|---|
| Backend Rust generation/check | `cargo check --workspace` passed. |
| Backend Rust tests | `cargo test --workspace` passed. |
| Node sidecar typecheck | `npm run typecheck` passed in `apps/graphql-server`. |
| Frontend typecheck | `npm run typecheck` passed in `Open-Flint-Atlas-main-release`. |
| RustyRed geotemporal tests | `cargo test -p thg-geotemporal` passed. |
| Live route smoke | Local Axum JSON bridge returned 222 Flint places from the checked-in atlas fixture. |
| `spacetime-atlas` smoke | `/spacetime-atlas/v1/GetViewportAtTime` returned Flint objects for the Flint tenant, 0 objects for `test-city`, 401 for missing tenant, and 0 objects for a time slice outside fixture intervals. |
| Multi-tenancy probe | Same route returned 0 places for `test-city`; missing tenant returned 401. |

Known validation note: frontend `npm run lint` exits successfully but reports
the existing unused `SimpleMeshLayer` warning in `AtlasMap.tsx`.

## Phase 2 Orchestrate Report

| Requirement | Evidence |
|---|---|
| Schema drift check | `npm run proto:check` regenerates checked-in TypeScript and Python proto output, imports the Python modules, and fails on stale generated diffs. `npm run proto:shape` validates the ReconstructionSpec and ReconstructionService contract shape. |
| Rust/proto round trip | `cargo test --workspace` passes, including `reconstruction_spec_round_trips_part_provenance`. |
| PostGIS truth schema | `migrations/0002_reconstruction_truth_schema.sql` creates `building_parts`, `artifacts`, `artifact_anchors`, `reconstruction_specs`, `generated_assets`, `corrections`, and `reconstruction_projection_outbox`. Static migration tests pass. |
| Multi-tenancy probe | Every new Phase 2 table has `tenant_id`, RLS enabled, and a `current_setting('app.tenant_id', true)` policy asserted by `reconstruction_truth_schema.rs`. |
| Approval ordering | `ReconstructionService.ApproveSpec` writes part-level `building_parts`, updates the spec to approved, then inserts an idempotent projection outbox intent in the same transaction. |
| CLI | `civic-atlas spec validate <file>` validates tenant/spec/version and part-level confidence. `civic-atlas spec submit <file>` writes an in-review spec into PostGIS. |

## Procedural Reconstruction Algorithm Addendum

| Stage | Runtime surface | Current implementation |
|---|---|---|
| 1. Evidence Assembly | `civic-atlas-reconstruction-engine::assemble_evidence` | Reads parcel history, direct artifacts, adjacent artifacts, and temporal predecessor/successor through an `EvidenceRepository` port. PostGIS and in-memory adapters exist. |
| 2. Direct Field Extraction | `extract_direct` | Deterministic Sanborn/photo/directory/text extraction populates directly observed fields with part provenance. |
| 3. Block Subgraph Construction | `build_block_subgraph` | Wraps a `BlockSubgraphRepository` port and hydrates focus node direct extraction. |
| 4. Spacetime Embedding Hydration | `hydrate_embeddings` | Calls `GetBatchSpacetimeEmbeddings` when `THESEUS_BRIDGE_URL` is set; otherwise uses explicit zero embeddings with `missing_embedding=true`. |
| 5. Block-Coherent Prior Inference | `PairformerCivicPriorModel` | Stage contract is Pairformer-ready: node features combine spacetime embeddings and direct-field counts; edge features preserve relation, distance, time distance, shared-wall/setback slots; publishable edge confidence records are emitted. The current model is a deterministic fallback until the civic Pairformer weights ship. |
| 6. Evidence-Prior Merge | `merge_evidence_prior` | Direct extraction wins; low-confidence direct values that disagree with priors become explicit merge conflicts. |
| 7. Geometry + Asset Generation | `SceneFoundryManifestGenerator` | Emits a queued Scene Foundry manifest and asset slot. Blender/Modal execution remains a downstream renderer integration. |
| 8. Spec Persistence + Public Surface | `civic-atlas-outbox-worker` | `reconstruction_jobs` persist generated specs to PostGIS and can auto-approve to `building_parts`, `generated_assets`, and projection outbox. |

Validation evidence from this slice:

| Check | Result |
|---|---|
| `npm run proto:check` | passed |
| `npm run proto:shape` | passed |
| `npm run typecheck` | passed |
| `cargo fmt --all --check` | passed |
| `cargo test --workspace` | passed |
| `cargo clippy --locked -p civic-atlas-cli -- -D warnings` | passed |
| `cargo clippy --locked -p civic-atlas-server -- -D warnings` | passed after containing the tonic `Status` large-error lint at the reconstruction module boundary. |

Explicit deviations:

| Deviation | Reason | Follow-up |
|---|---|---|
| RustyRed projection worker is an outbox intent, not a live RustyRed write. | This repo does not yet include the RustyRed projection client/job runner. The outbox preserves the required replay/idempotency boundary and keeps PostGIS as truth. | Implement the worker that drains `reconstruction_projection_outbox` and writes `BuildingPresence` summaries to RustyRed. |
| Phase 2 gate data was not loaded into a live PostGIS instance. | No live `DATABASE_URL`/seed DB was provided in this run. | Run migrations, submit/approve the five Carriage Town specs, and capture SQL/gRPC evidence. |
| `GetBlockSubgraph` does not yet return approved reconstruction parts. | The current spacetime-atlas handler still uses fixture data, not the new reconstruction read model. | Connect `GetBlockSubgraph` to approved PostGIS reconstruction specs and projected RustyRed summaries. |
| Phase 3 remains blocked. | The Blender primitive repo, real Modal/S3 configuration, and frontend visual route/gate are outside this backend-only slice. | Build the primitive library, Modal renderer, backend render job queue, and frontend route with browser screenshots. |

## Explicit Non-Goals and Deferrals

| Item | Why deferred | Risk | Follow-up |
|---|---|---|---|
| Full Carriage Town UI replacement in Phase 0 | Backend contracts and approved specs do not exist yet. | Visual downgrade or fixture-only illusion. | Build after ReconstructionSpec and assets land. |
| RustyRed as canonical data store | PostGIS is the truth source by spec. | Cache becomes canon. | Keep projection idempotent and replayable. |
| Browser GraphQL rewrite | Browser contract must remain stable. | Public atlas breaks during migration. | Re-point endpoint behind feature flag only. |
