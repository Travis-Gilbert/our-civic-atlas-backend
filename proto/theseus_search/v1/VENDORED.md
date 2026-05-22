# theseus_search vendored protos

Source: https://github.com/Travis-Gilbert/theorem-protos
Pinned to commit: `b64a414` (initial release)
Path in source: `theseus_search/v1/search.proto`

This is the canonical gRPC contract for Theseus's search orchestrator:
search, gap-walk, source-pair, provenance. The civic-atlas backend
consumes the client side via the `theseus-client` crate. The Theseus
side (Index-API/apps/notebook/grpc/) implements the service.

The proto package is `theseus_search.v1`. The contract is sibling to
`theorems_harness.v1` (harness) and `rustyred.v1` (graph DB). All three
are siblings, not parent-child. See theorem-protos README for the full
product split.

## Updating

When theorem-protos publishes a new commit:

1. `cd /Users/travisgilbert/Tech Dev Local/theorem-protos && git pull`
2. Note the new commit hash.
3. `cp .../theseus_search/v1/search.proto .../our-civic-atlas-backend/proto/theseus_search/v1/search.proto`
4. Update the pinned commit hash above.
5. `cargo check -p civic-atlas-types` to regenerate Rust bindings.
6. Mirror the same copy + regen in Index-API.

Do NOT edit `search.proto` in this repo directly. The source of truth
is theorem-protos. Edits must round-trip through that repo to keep
sibling consumers (Index-API, future RustyRed-GraphDB consumers, etc.)
in sync.
