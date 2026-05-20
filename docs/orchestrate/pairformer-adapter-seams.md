---
mirror_note: XRL-A-001 coordination mirror for backend and ingest Pairformer coordination.
mirrored_from_repo: Open-Flint-Atlas-main-release
mirrored_from_path: docs/plans/lane-4-strategic-seams/pairformer-adapter-seams.md
mirrored_from_commit: 9febedc
platform_correction_commit: 5e0ddb6
source_head_at_mirror: eace782
mirrored_on: 2026-05-20
---

# Coordination Note: Pairformer Adapter Seams for Graph-LoRA

To: Codex, working in `civic-atlas-ingest` (and consumers in `our-civic-atlas-backend` that call Pairformer inference)
From: 2026-05-20 frontend session in `Open-Flint-Atlas-main-release`
Status: forward-looking architectural guidance, not a code change request

## Platform note (2026-05-20 user correction)

The user retired the prior Modal-based ML infrastructure on 2026-05-20.
All ML work, including the Pairformer, now targets Ray
(`https://github.com/ray-project/ray`) on RunPod. The current
`civic-atlas-ingest/modal/` directory is legacy nomenclature; XRL-B-000
in `docs/plans/cross-repo-launch-plan-2026-05-20.md` covers the rename
and rewrite of the stubs as Ray entrypoints. The seams requested below
apply equally to either platform; they are framework-level concerns
about module separability, not platform-specific. References to
`civic-atlas-ingest/modal/` in this note refer to the current stub
locations, not the long-term home.

## Request

Before any Pairformer training code is written in `civic-atlas-ingest/modal/building_head_train.py` (current stub location; post-migration this becomes a Ray Train entrypoint) or its siblings, design the Pairformer with three adapter seams:

1. **Separable `PairUpdate` block.** The pair-update message-passing logic lives in its own module, distinct from the input encoder and the output heads.
2. **Separable `ConfidenceHead` block.** The per-part confidence head lives in its own module, distinct from the archetype head and the per-part prediction heads.
3. **Tenant-context parameter on the input encoder.** The input encoder accepts a `tenant_context` parameter (string slug, defaults to a sentinel value like `"_base"` for the base pre-trained model). The output heads are similarly tenant-aware. These are no-ops at V1 because there is only one tenant; they are the hooks that Graph-LoRA needs later.

## Why this matters

The Anthropic-authored OpenUSD/PairFormer/Nodetree doc proposes Graph-LoRA (KDD 2025) as the post-V1 path for per-tenant adaptation of the Pairformer. Pre-train one Pairformer on a Rust Belt corpus (Detroit, Buffalo, Cleveland, Pittsburgh, Toledo, Akron, Milwaukee, Saginaw, Bay City, Youngstown). Each tenant gets a small Graph-LoRA adapter that learns the tenant-specific residual distribution shift. The adapters compose at inference time:

```
Pre-trained Pairformer (frozen)
  + Tenant Graph-LoRA adapter (Flint)
  + Era/archetype Graph-LoRA sub-adapter (1925 wood-frame)
  → per-part priors for this specific building
```

Graph-LoRA's empirical claim is "effectiveness against fourteen baselines by tuning only 20% of parameters, even across disparate graph domains, particularly pronounced in the 10-shot setting." That last property matters for civic atlas: corrections accumulate slowly. A method that works at 10-shot is structurally suited to a system that gets a small number of corrections per archetype per month.

Graph-LoRA is post-V1 work. But the architectural seams that make it possible must be present in V1. Inserting them into not-yet-written code is cheap; retrofitting them into already-trained models is expensive and risks rework.

## Why this is a coordination note, not a frontend commit

The Pairformer architecture lives in `civic-atlas-ingest/modal/`. The frontend cannot land it. This note is the explicit artifact saying: when you write that code, write it with these seams in place.

The current state of `civic-atlas-ingest/modal/building_head_train.py` and `building_head_infer.py` is stub-only. The cheapest moment to insert the seams is now, before the training code is written. After training runs and produces a checkpoint, changing the architecture invalidates the checkpoint.

## Acceptance criteria for Codex

The seams are correctly inserted when:

1. The Pairformer's training entrypoint imports `PairUpdate` and `ConfidenceHead` as separate modules from a distinct `civic_atlas_ingest.pairformer.blocks` package or equivalent.
2. The input encoder's forward signature includes `tenant_context: str = "_base"` as a parameter.
3. The output heads accept the same `tenant_context` parameter and route appropriately (today a no-op; tomorrow the adapter dispatcher).
4. The model's checkpoint format records the `tenant_context` value used at training time, so later adapter loading can confirm compatibility.
5. A unit test in `civic-atlas-ingest/tests/` confirms that a forward pass with `tenant_context="flint"` produces the same output as `tenant_context="_base"` today (since adapters are not yet loaded), and that the parameter is not silently dropped.

## What this note does not do

- Does not write Graph-LoRA training code. Graph-LoRA is post-V1.
- Does not specify the adapter file format. That is a separate spec when Graph-LoRA actually lands.
- Does not specify the dispatcher logic for combining base + tenant + era/archetype adapters. That dispatcher is the adapter-time concern, not the V1 concern.
- Does not freeze the Pairformer architecture. The Pairformer can evolve; the constraint is only on the three seams above.

## Reciprocal artifact

The frontend's reciprocal artifact, when Graph-LoRA eventually ships, is the rendering UI for per-tenant adapter status. A future Lane 3 design brainstorm will scope that. None of it lives in CU-L1-001.

## Cross-reference to the Anthropic doc

The exact paragraph this note implements:

> "The single best move is to wait on Graph-LoRA until the V1 ships, but design the Pairformer with the adapter seam in place. Concrete architectural decisions to bake in now: The Pairformer's core PairUpdate and ConfidenceHead blocks live in a separate module from the input encoder and output heads. The input encoder accepts a 'tenant context' parameter (string, defaults to a sentinel value for the base model). The output head is similarly tenant-aware. These are no-ops today, but they're the hooks Graph-LoRA needs to inject the adapter graphs alongside the frozen core."

Source: `/Users/travisgilbert/Tech Dev Local/Flint.OurAtlast.org/Open USD, PairFormer +Nodetree.md` (lines 85-94 of the doc).
