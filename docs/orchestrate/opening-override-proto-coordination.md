---
mirror_note: XRL-A-001 coordination mirror for backend proto stabilization.
mirrored_from_repo: Open-Flint-Atlas-main-release
mirrored_from_path: docs/plans/lane-4-strategic-seams/opening-override-proto-coordination.md
mirrored_from_commit: 9febedc
source_head_at_mirror: eace782
mirrored_on: 2026-05-20
---

# Coordination Note: OpeningOverride Proto Addition

To: Codex, working in `our-civic-atlas-backend`
From: 2026-05-20 frontend session in `Open-Flint-Atlas-main-release`
Status: request pending Codex acceptance

## Request

Add a `repeated OpeningOverride opening_overrides` field to the `OpeningGrid` message in `proto/civic_atlas/v1/reconstruction.proto`. Add the corresponding `OpeningOverride` message in the same file.

## Why this matters

The Pascal-style node tree adapter in the frontend (`src/lib/atlas/reconstruction-node-tree.ts`) projects `ReconstructionSpec` rows into a tree where individual windows, doors, and ornamental elements are addressable nodes. The Anthropic-authored OpenUSD/PairFormer/Nodetree doc names this adapter as the keystone of Phase 4 community correction UX: a resident must be able to say "the second-floor center window on the McFarlan Hotel is a casement, not double-hung," and that correction must round-trip through the proto into the canonical USD publication without information loss.

The current `OpeningGrid` message expresses only the grid's default pattern. It cannot express a per-bay exception. The Anthropic doc supplies the minimal proto change:

```proto
message OpeningGrid {
  PartProvenance provenance = 1;
  uint32 bay_count = 2;
  uint32 floor_count = 3;
  string rhythm = 4;        // candidate for separate rename to window_pattern
  string opening_type = 5;  // candidate for merge into window_pattern
  map<string, string> attributes = 6;
  repeated OpeningOverride opening_overrides = 7;  // new
}

message OpeningOverride {
  uint32 bay_index = 1;
  string override_kind = 2;
  string override_pattern = 3;
  PartProvenance override_provenance = 4;
}
```

The migration is backward-compatible: existing specs have an empty `opening_overrides` array; their behavior does not change.

## Why this is a separate coordination note

This note is the OpeningOverride field only. The full proto-USD field-name rename (`rhythm` and `opening_type` collapse to `window_pattern`, multiple other renames) is the subject of a separate audit at `docs/design/proto-usd-field-parity-audit.md` and a separate cross-repo coordination plan that follows this catchup pass.

Adding `opening_overrides` does not require the rename to land first. It is additive. The rename is the larger PR; this is the smaller one.

## Acceptance criteria for Codex

The change is complete when:

1. `OpeningOverride` message is defined in `proto/civic_atlas/v1/reconstruction.proto` with the four fields above.
2. `OpeningGrid` has a `repeated OpeningOverride opening_overrides` field at field number 7.
3. `cargo check --workspace` and `cargo test` pass in `our-civic-atlas-backend`.
4. The corresponding ts-proto and Python bindings regenerate without manual intervention.
5. The Carriage Town seed migration at `migrations/0004_seed_carriage_town_specs.sql` is updated only if it now needs to populate any overrides; default-empty is the expected case for the pilot.
6. The PR description references this coordination note by path and confirms the additive field number.

## Downstream consumers in the frontend

Once this lands, the following frontend work becomes unblocked:

- Extend `src/lib/atlas/reconstruction-node-tree.ts` to generate `Opening[m]` child nodes under each `OpeningGrid`, with per-bay overrides reflected at the Opening level.
- Extend `scripts/validate-reconstruction-node-tree.mjs` to test round-trip integrity of a per-opening correction.
- Extend the Phase 4 community correction UI design proposal (CU-L3-001 Lost Flint UI brainstorm) to include the three confidence bands at the Opening level, not only the Part level.

None of these downstream items live inside CU-L1-001. They are separate slices in the cross-repo plan that follows the current catchup pass.

## What this note does not do

- Does not request the broader field-name rename. That belongs in the parity audit and a separate cross-repo plan.
- Does not request a new RPC. The proto change is data-only; existing services continue to work.
- Does not propose backend behavior on what to do with per-bay overrides during approval. That belongs in the corrections service implementation work that already has a TODO in `crates/civic-atlas-server/src/corrections.rs::approve_correction`.

## Reciprocal artifact

When Codex acts on this note, the matching artifact in this frontend repo is the validator extension and the adapter's Opening-child-generation extension. Both wait on the proto change landing; both are scoped in the next cross-repo plan.
