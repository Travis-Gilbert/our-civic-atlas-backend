---
mirror_note: XRL-A-001 coordination mirror for backend proto stabilization.
mirrored_from_repo: Open-Flint-Atlas-main-release
mirrored_from_path: docs/design/proto-usd-field-parity-audit.md
mirrored_from_commit: 9febedc
source_head_at_mirror: eace782
mirrored_on: 2026-05-20
---

# Proto / USD Schema Field Parity Audit

Generated 2026-05-20. Compares `our-civic-atlas-backend/proto/civic_atlas/v1/reconstruction.proto` against the `civicAtlasSchema` USD schema definitions in the Anthropic-authored OpenUSD/PairFormer/Nodetree document at `/Users/travisgilbert/Tech Dev Local/Flint.OurAtlast.org/Open USD, PairFormer +Nodetree.md`.

## Purpose

The Anthropic doc establishes OpenUSD as the canonical publication format for finalized scene reconstructions. The doc's central architectural claim: "when you define the ReconstructionSpec proto fields, make sure every field maps cleanly to a USD attribute name. That's the seam that lets the spec become USD without translation losses later."

Today, the proto field names diverge significantly from the USD schema attribute names. Every divergence is a future translation cost. This audit enumerates the divergences so they become a planning artifact rather than a discovery during USD adoption.

## Posture

- Spec is the floor. This audit does not silently defer any divergence; each is listed individually.
- This audit is a document, not a code change. The actual proto rename belongs to Codex in `our-civic-atlas-backend`. See `docs/plans/lane-4-strategic-seams/opening-override-proto-coordination.md` for the coordination shape.
- USD field-name parity is a now-decision per the theorize Theorem Brief on 2026-05-20.

## Divergence summary

| Message | Renames | Missing in proto | Missing in USD | Severity |
|---|---|---|---|---|
| ReconstructionSpec / CivicAtlasReconstruction | 4 | 7 | 0 | high |
| Mass / CivicAtlasMass | 2 | 3 | 3 | medium |
| Facade / CivicAtlasFacade | 2 | 2 | 2 | high (multiplicity divergence on opening_grids) |
| OpeningGrid / CivicAtlasOpeningGrid | 2 (collapse) | 3 | 2 | high (OpeningOverride missing entirely) |
| Opening / CivicAtlasOpening | n/a | entire message | n/a | medium (USD-only today) |
| Roof / CivicAtlasRoof | 2 | 2 | 2 | low |
| Ornament / CivicAtlasOrnament | 2 | 2 | 2 | medium |
| GroundFloor / CivicAtlasGroundFloor | 2 (collapse) | 2 | 3 | high (semantic collapse) |
| PartProvenance / CivicAtlasPartAPI | 3 | 8 | 1 | high (texture and correction provenance entirely missing) |

Counts: roughly 19 renames, 30 fields missing in proto, 15 fields missing in USD. Net direction: the proto needs additive growth toward USD, not just renames.

## Message-by-message divergences

### ReconstructionSpec vs CivicAtlasReconstruction

| Proto field | USD attribute | Action |
|---|---|---|
| `tenant_context.tenant_id` | `civicAtlas:tenant` | Map at converter; nested vs flat. |
| `spec_id` | `civicAtlas:specId` | Rename to `spec_id` stays; case-insensitive at USD layer. |
| `version` | `civicAtlas:specVersion` | Rename proto field to `spec_version` for parity. |
| `parcel_id` | `civicAtlas:parcelId` | Already matches. |
| (missing) | `civicAtlas:tStartMs` | Add proto field `t_start_ms` (validity start, not created_at). |
| (missing) | `civicAtlas:tEndMs` | Add proto field `t_end_ms` (validity end). |
| (missing) | `civicAtlas:archetype` | Add proto field `archetype_classification` (Pairformer head output). |
| (missing) | `civicAtlas:gnnVersion` | Already exists on `PartProvenance.gnn_version`; promote to top level for the reconstruction's primary GNN. |
| (missing) | `civicAtlas:publishedAt` | Add proto field `published_at_ms` (distinct from approved_at). |
| (missing) | `civicAtlas:license` | Add proto field `license` (SPDX identifier). |
| (missing) | `civicAtlas:summaryConfidence` | Derived at publication; do not add to proto. |

### Mass vs CivicAtlasMass

| Proto field | USD attribute | Action |
|---|---|---|
| `story_count` | `civicAtlas:stories` | Rename proto field to `stories` for parity. |
| `height` (DimensionRange) | `civicAtlas:heightMeters` | Resolve range to scalar at converter; keep DimensionRange in proto for fuzzy authorship. |
| (missing) | `civicAtlas:footprintGeometryId` | Add proto field `footprint_geometry_id` (FK to PostGIS footprints). |
| (missing) | `civicAtlas:partKind` | Derive at converter; do not add to proto. |
| (missing) | `civicAtlas:partId` | Add proto field `part_id` if Mass should be addressable; otherwise derive. |
| `form` | (missing in USD) | USD has no Mass form; document this as a USD gap or extend the USD schema. Recommend extending USD. |
| `width`, `depth` (DimensionRange) | (missing in USD) | USD relies on footprintGeometryId + extrusion. Document that the proto's width/depth become hints during synthesis, not USD attributes. |
| `attributes` (map) | (missing in USD) | USD uses applied schemas. Document as proto-only fuzzy metadata. |

### Facade vs CivicAtlasFacade

| Proto field | USD attribute | Action |
|---|---|---|
| `orientation` | `civicAtlas:facadeSide` | Rename proto field to `facade_side` AND constrain to USD's allowed-tokens enum (primary, secondary, side_left, side_right, rear). |
| `material` | `civicAtlas:primaryMaterial` | Rename proto field to `primary_material` AND constrain to USD's allowed-tokens enum. |
| `opening_grids` (repeated) | `civicAtlas:openingGrid` (rel, singular) | Resolve. The doc's USD model assumes one grid per facade. Two options: (a) change proto to singular, (b) extend USD to repeated. Recommend extending USD: a complex facade can have a primary grid plus a secondary grid for a service entry. |
| (missing) | `civicAtlas:partKind` | Derive at converter. |
| (missing) | `civicAtlas:partId` | Add proto field `part_id`. |
| `color` | (missing in USD) | USD relies on texture provenance (`CivicAtlasTextureAPI`). Document as a synthesis-time hint that informs ControlNet but is not stored on the facade prim itself. |
| `attributes` (map) | (missing in USD) | Same as Mass: proto-only. |

### OpeningGrid vs CivicAtlasOpeningGrid

| Proto field | USD attribute | Action |
|---|---|---|
| `bay_count` | `civicAtlas:bayCount` | Already matches modulo case. |
| `rhythm` + `opening_type` | `civicAtlas:windowPattern` | Collapse the two proto fields into one `window_pattern` field with USD's allowed-tokens enum (three_over_one, six_over_six, casement, double_hung_sash, fixed_pane, storefront_plate, transom_over_sash, round_arch, segmental_arch, unknown). |
| (missing) | `civicAtlas:hasStorefrontGround` | Add proto field `has_storefront_ground`. |
| (missing) | `civicAtlas:partKind`, `civicAtlas:partId` | Derive partKind; add part_id. |
| (missing) | (no equivalent — OpeningOverride proposed addition) | Add `repeated OpeningOverride opening_overrides` field. See `docs/plans/lane-4-strategic-seams/opening-override-proto-coordination.md`. |
| `floor_count` | (missing in USD) | USD models floors via Level prims in the scene tree, not on the grid. Document that floor_count becomes a Level prim count at converter time. |
| `attributes` (map) | (missing in USD) | Same: proto-only. |

### Opening (entirely USD-only today)

USD defines `CivicAtlasOpening` with `civicAtlas:partKind`="opening", `civicAtlas:partId`, `civicAtlas:openingKind`, `civicAtlas:bayIndex`, `civicAtlas:overridePattern`. Proto has no Opening message. Per-bay overrides live in the proposed `OpeningOverride` message; full per-opening provenance via Opening prims is USD-only.

Action: this is acceptable as a USD-only construct. The proto carries grid + overrides; USD's converter generates Opening prims per bay using the override pattern when present and the grid default when absent. No proto change needed.

### Roof vs CivicAtlasRoof

| Proto field | USD attribute | Action |
|---|---|---|
| `form` | `civicAtlas:roofType` | Rename proto field to `roof_type` AND constrain to USD enum (gable, hip, flat, flat_parapet, mansard, gambrel, shed, saltbox, pyramid, unknown). |
| `material` | `civicAtlas:roofMaterial` | Rename proto field to `roof_material` AND constrain to USD enum. |
| `pitch_degrees` | (missing in USD) | Extend USD to add `civicAtlas:pitchDegrees`. Pitch is genuinely useful for accurate reconstruction. |
| `attributes` (map) | (missing in USD) | Same: proto-only. |

### Ornament vs CivicAtlasOrnament

| Proto field | USD attribute | Action |
|---|---|---|
| `kind` | `civicAtlas:ornamentKind` | Rename proto field to `ornament_kind` AND constrain to USD enum (cornice, trim, signage, frieze, pilaster, balustrade, string_course, quoin). |
| `material` | `civicAtlas:ornamentMaterial` | Rename to `ornament_material` AND constrain to USD enum. |
| `ornament_id` | (`civicAtlas:partId` via parent class) | Already matches semantically. |
| `location` | (missing in USD) | USD relies on prim hierarchy. Document that proto.location becomes a hint at synthesis time but does not land as a USD attribute. |
| (missing) | `civicAtlas:ornamentStyle` | Add proto field `ornament_style` (free-form style description). |
| `attributes` (map) | (missing in USD) | Same: proto-only. |

### GroundFloor vs CivicAtlasGroundFloor

| Proto field | USD attribute | Action |
|---|---|---|
| `use_type` + `storefront_type` + `entry_location` | `civicAtlas:treatment` | Significant semantic collapse. USD models ground floor as a single enum (storefront, residential_entry, industrial, civic_entry, loading_dock, garage, mixed, unknown). The proto's three fields encode more nuance. Two options: (a) collapse the proto to match USD (information loss), (b) extend USD to keep all three. Recommend (b): extend USD because residential_entry can still have a storefront_type=none, and entry_location is independently useful. |
| `has_awning` | `civicAtlas:hasCanopy` | Rename to `has_canopy` for parity; resolve the awning vs canopy terminology one-way. |
| (missing) | `civicAtlas:partKind`, `civicAtlas:partId` | Same as elsewhere. |
| `attributes` (map) | (missing in USD) | Same: proto-only. |

### PartProvenance vs CivicAtlasPartAPI + CivicAtlasCorrectionAPI + CivicAtlasTextureAPI

The biggest divergence. USD splits provenance across three applied schemas; proto has one message.

| Proto field | USD attribute | Schema | Action |
|---|---|---|---|
| `confidence` | `civicAtlas:partConfidence` | PartAPI | Rename proto to `part_confidence` for parity. |
| `from_gnn_prior` | `civicAtlas:fromGnnPrior` | PartAPI | Already matches modulo case. |
| `gnn_version` | `civicAtlas:gnnVersion` | PartAPI | Already matches modulo case. |
| `sources` (repeated ReconstructionSource) | `civicAtlas:sources` (rel) | PartAPI | Embed in proto; relate by reference in USD via the per-tenant artifact library. Converter handles the embed-to-reference step. |
| `reviewer_note` | `civicAtlas:moderatorNotes` | CorrectionAPI | Rename proto to `moderator_notes` AND consider moving out of PartProvenance into a separate ProvenanceCorrection message. |
| `coverage_quality` | (missing in USD CivicAtlasPartAPI) | PartAPI | Extend USD schema to add `civicAtlas:coverageQuality`. The doc's schema does not have this; the Anthropic doc's CivicAtlasPartAPI predates the Phase 5 addition. |
| (missing) | `civicAtlas:perSourceConfidences` | PartAPI | Add proto field `repeated double per_source_confidences` (parallel to sources). |
| (missing) | `civicAtlas:moderatorOverridden`, `civicAtlas:moderatorOverriddenAt` | PartAPI | Add to proto as `moderator_overridden` bool + `moderator_overridden_at_ms` int64. |
| (missing) | `civicAtlas:hasSourceConflict` | PartAPI | Add proto field `has_source_conflict` bool. |
| (missing) | `civicAtlas:correctionId`, `correctionType`, `correctionReasoning`, `correctionApprovedAt` | CorrectionAPI | Add to proto as a new optional `Correction` sub-message on PartProvenance. |
| (missing) | `civicAtlas:textureSource`, `loraArchetype`, `loraWeight`, `controlnetConditioningSource`, `textureConfidence` | TextureAPI | Add to proto as a new optional `TextureProvenance` sub-message on each Part type that can carry textures (Facade, Roof, Ornament, GroundFloor). |

## Recommended migration shape

This audit recommends the rename as one bounded backend PR, not piecemeal. Reasoning: every consumer (Rust server, TypeScript codegen, Python ingestion) regenerates against the proto. A piecemeal rename forces N regenerations and N coordination windows; a single rename PR forces one.

Proposed PR shape on `our-civic-atlas-backend`:

1. Rename the 19 fields enumerated above. Each becomes a snake_case version of the USD camelCase attribute, minus the `civicAtlas:` prefix.
2. Add the 30 missing-in-proto fields. Cluster by message; each addition is a new field number, never a reuse.
3. Add the proposed `OpeningOverride` message (see coordination note).
4. Regenerate ts-proto, grpc-py, and tonic bindings. Verify every consumer compiles.
5. Update existing fixtures (`migrations/0004_seed_carriage_town_specs.sql` and any test fixtures) to use the new field names.
6. Update this audit file to mark each item complete with a date.

Items left as "extend USD" recommendations (not proto changes): USD missing `pitch_degrees`, USD missing `coverage_quality`, USD's GroundFloor `treatment` collapsing three proto fields. These belong on the USD-side schema authoring task, which lands in `civic-atlas-ingest` when the USD converter ships.

## What this audit does not do

- Does not propose a USD-side schema change yet. The Anthropic doc's `civicAtlasSchema` is taken as-is for now; the small extensions noted above land on the USD-authoring task in `civic-atlas-ingest`.
- Does not propose a converter (`civic_atlas/usd/converter.py`). That is a Phase 8+ deliverable per the Anthropic doc; it depends on this rename landing first.
- Does not coordinate the rename with Codex directly. The coordination shape is named in `docs/plans/lane-4-strategic-seams/opening-override-proto-coordination.md`; the full-rename coordination note belongs in the cross-repo follow-up plan.

## Decisions captured

- USD field-name parity is a now-decision (theorize Theorem Brief, 2026-05-20).
- Proto rename strategy is a single bounded PR, not piecemeal.
- Per-source confidences, moderator override metadata, has-source-conflict, correction metadata, and texture provenance all extend the proto via additive fields or sub-messages, never via reuse of existing field numbers.
- USD extensions (pitch_degrees, coverage_quality, GroundFloor field expansion) defer to the USD-authoring task in `civic-atlas-ingest`.
