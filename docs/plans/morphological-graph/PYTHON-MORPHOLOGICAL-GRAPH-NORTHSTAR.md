# Python Morphological Graph — North Star

A direction, not an execution handoff. It records where city2graph runs, why it runs there, what Civic Atlas consumes from it, and the boundary that keeps the interim from hardening into a permanent coupling. The adoption itself is small (import the library, call one function, project the result); this document exists for the cross-repo, multi-session intent around that small adoption, not for the adoption steps.

## The decision

city2graph runs in Theseus, as a method on the existing Python bridge sidecar, and Civic Atlas consumes its output over the gRPC bridge that already exists. This is both the interim home (until the Rust parity version lands; see RustyRed's `BURN-MORPHOLOGICAL-GRAPH-NORTHSTAR.md`) and the durable Theseus home, because Theseus has a permanent reason to run morphological graph construction.

## Why here

The bridge is already built. `crates/theseus-client/src/lib.rs` already dials a Python sidecar (`theseus_bridge.v1.TheseusBridge`, served by Index-API's `apps/notebook/grpc/bridge_server.py`) for embeddings and artifact ingest. A city2graph endpoint is the same shape: one more RPC on a sidecar that exists, called by a client that exists. The cost is adding a proto method and a handler, not standing up an integration.

Theseus is the right permanent home. city2graph pulls the full scientific-GIS stack (momepy, libpysal, osmnx, geopandas, shapely). There are two heavy-Python homes in this system (`civic-atlas-ingest` and Theseus). Putting it where there is a durable reason for it means not widening the ingest image for something whose permanent home is elsewhere. The interim placement and the long-term intent agree, which is the sign of a right call.

It unblocks the Pairformer immediately. city2graph's `morphological_graph` produces the typed civic relation graph the building-head Pairformer trains over (`adjacent_to`, `fronts_street`, `same_block_as`), from footprints and centerlines that already exist. It is the fastest path to a real civic graph feeding the model.

It is the oracle. When the Rust port begins, this running Python is its correctness check: same inputs in, compare typed edge sets out. So this path is worth taking even setting the Pairformer aside, because it is both the immediate unblock and the reference the Rust version is measured against.

## What city2graph produces

`morphological_graph` returns a heterogeneous typed graph with three edge classes:

- `("place", "touched_to", "place")`: enclosed-tessellation adjacency (cells partitioned around buildings, bounded by streets; contiguity, not a distance threshold).
- `("movement", "connected_to", "movement")`: street topology via the dual graph (segments to nodes, shared endpoints to edges).
- `("place", "faced_to", "movement")`: the building-to-street interface.

These are the typed relations the civic Pairformer's hand-enumerated relation set mirrors.

## What Civic Atlas consumes, and how

Civic Atlas consumes the graph as layer records, through the shipped layer contract. It does not couple to city2graph's API. The path:

- Theseus runs city2graph and exposes the typed graph over the bridge method.
- Civic Atlas calls it over the existing `theseus-client`, the same way it calls embeddings today.
- The typed edges project as a layer through the `model_run_id` arm of `LayerRecipeSourceRef` (shipped in `crates/civic-atlas-server/src/graphql/layer.rs`, the slice-1 contract). The morphological graph is a model-produced layer, the same conformance pattern traffic and reconstruction already follow: a producer function returning records plus a status, registered as a layer.

So the coupling is exactly the layer contract, nothing more. Civic Atlas sees civic relation edges as layer records; it does not import city2graph, does not depend on its internals, and does not depend on Theseus for anything beyond that one projected layer.

## The boundary (what this must not become)

The interim must not harden into Civic Atlas depending on Theseus permanently for civic geometry as an API coupling. The dependency is one projected layer through the contract, not a structural reliance on Theseus's internals. When the Rust parity version lands, the producer behind that layer swaps from the Theseus bridge call to the in-process Rust path, and the layer contract on the Civic Atlas side does not change. That swap-without-contract-change is the test that the boundary was held.

This is also why the layer projection matters: it is the seam that lets the producer move from Python-in-Theseus to Rust-in-RustyRed without Civic Atlas noticing. If Civic Atlas ever reaches past the layer contract into city2graph or the bridge directly, that seam is lost.

## Deployment caution

The bridge sidecar currently serves embeddings and artifact ingest, which are per-item hydration concerns. The morphological graph is heavier and blockier (tessellation over many footprints). Either confirm the sidecar can host that heavier call without starving embedding latency, or give city2graph its own method with its own timeout budget so a slow graph build does not block embedding hydration. This is a deployment-shape check, not a blocker.

## What does not need a spec

The adoption is small and does not get an execution handoff: add city2graph to the Theseus sidecar's image, write the bridge method that calls `morphological_graph` and returns the typed edges, and add the Civic Atlas producer that projects them as a layer through the existing pattern. That is a thin checklist the active work absorbs. This document is the why and the boundary around it, which is the part worth durable record.

## Grounding (verified)

- `crates/theseus-client/src/lib.rs` already dials the Python bridge sidecar over gRPC (`theseus_bridge.v1.TheseusBridge`, served by `Index-API/apps/notebook/grpc/bridge_server.py`) for embeddings and artifact ingest. A city2graph method is the same shape.
- `crates/civic-atlas-server/src/graphql/layer.rs` (slice-1 layer contract, shipped) has `LayerRecipeSourceRef` with a `model_run_id` arm and the producer-conformance pattern (traffic, reconstruction, event-surface as producer functions returning records plus a status). The morphological graph conforms as a model-produced layer.
- city2graph (BSD-3, Liverpool GDS Lab) produces the typed `touched_to` / `connected_to` / `faced_to` graph; hard deps are the full scientific-GIS stack.
- The Rust parity target and the oracle relationship are recorded in RustyRed's `BURN-MORPHOLOGICAL-GRAPH-NORTHSTAR.md` and in harness memory (the city2graph sequential decision).

## Status

This is the active path. The adoption is small and rides the existing bridge. This North Star is the durable record so a later session does not, without this context, let Civic Atlas couple to Theseus or to city2graph beyond the one projected layer, or forget that this Python is the oracle the Rust version is measured against.
