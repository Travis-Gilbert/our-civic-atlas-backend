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
| OCA-BE-P2-001 | Add ReconstructionSpec proto and generated type gates. | backend proto + TS/Python generation | execute | Rust, TypeScript, and Python generated outputs fail CI when stale. | schema drift check | Spec versions diverge. | planned |
| OCA-BE-P2-002 | Add PostGIS reconstruction truth schema. | migrations | execute | Immutable approved specs, building parts, artifacts, anchors, generated assets, corrections, and RLS exist. | migration tests | RustyRed originates truth. | planned |
| OCA-BE-P2-003 | Implement approval projection job. | backend jobs | execute | Approved spec writes PostGIS parts first, then idempotently projects summary to RustyRed. | replay tests | Partial projection corrupts graph state. | planned |
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

## Explicit Non-Goals and Deferrals

| Item | Why deferred | Risk | Follow-up |
|---|---|---|---|
| Full Carriage Town UI replacement in Phase 0 | Backend contracts and approved specs do not exist yet. | Visual downgrade or fixture-only illusion. | Build after ReconstructionSpec and assets land. |
| RustyRed as canonical data store | PostGIS is the truth source by spec. | Cache becomes canon. | Keep projection idempotent and replayable. |
| Browser GraphQL rewrite | Browser contract must remain stable. | Public atlas breaks during migration. | Re-point endpoint behind feature flag only. |
