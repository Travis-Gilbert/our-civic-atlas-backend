# Civic Research newEvidence: claude-code + codex coordination

Harness MCP is down. This doc + git commits are the substrate. Author: claude-code.

## Status

| Piece | State |
|---|---|
| `priorKnowledge` (RustyRed graph read) | SHIPPED `67626f6`. Returns nothing: `flint` graph is empty (0 nodes). |
| Degrade RustyRed `store_unavailable` to empty, not 500 | SHIPPED `9ef980a`. |
| `newEvidence` provider: Library of Congress | SHIPPED + LIVE `7606892` (deploy `02617554` SUCCESS). Real Flint hits per query. |
| Frontend renders it | WORKS, no change needed. Node sidecar folds `newEvidence` -> `sources`/`signals` (`apps/graphql-server/src/schema.ts:361,406,422`); panel renders both. |

So a query already returns real Library of Congress sources (HABS/HAER, Sanborn, photos), even though the RustyRed graph is empty. Verified: `loc.gov/search/?q=Flint+Michigan&fo=json` returns real results.

## RustyRed has a live web crawler (the bump), verified on rustyred-production

- `POST /crawl` (scope `graph:write`): `{ tenant?, run_id?, seeds: [url], budget?, scope? }`. `run_live_crawl()` fetches the SEED URLS live and commits crawled pages into the TENANT graph. `/crawl` returned HTTP 400 on empty seeds (route live).
- `GET /search.json?q=` searches the crawled substrate. Returned `200 {ok, search:{hits:[]}}` (empty, nothing crawled).
- `graph/stats` for `flint`: 0 nodes, 0 edges, 0 designations, version 0.

Key constraint: `/crawl` takes SEED URLS, not a free-text query. RustyRed has no `query -> open-web-URLs` discovery. That is the blocker below.

## The one real blocker: seed discovery (`query -> open-web URLs`)

The open-web crawl half needs a source that turns a query into seed URLs. Options:

1. **Codex builds a `query -> seeds` frontier in RustyRed** (a web-search step, server-side). Then `civic_research` calls one RustyRed endpoint: query in, crawled `newEvidence` out. Cleanest; keeps it all in RustyRed (matches the "RustyRed not Theorem" call).
2. **A web-search key in the Axum resolver** (Tavily/Brave/Google) turns query -> URLs; `civic_research` seeds RustyRed `/crawl`. Adds a service-tier key (must stay server-side).

Until one lands, the open-web crawl can only crawl URLs we already have (e.g. the LoC result URLs), which adds little beyond LoC.

## Split

### claude-code owns `our-civic-atlas-backend`

- DONE: LoC `newEvidence`, threaded through every return path.
- NEXT (after seed discovery exists): seed RustyRed `/crawl` from discovered URLs, fold `/search.json` hits into `newEvidence`. `rustyred-client` gets `crawl()` + `serp_search()` then.
- Provider roles: LoC fits free-text research (DONE). ArcGIS REST (parcels by WHERE/spatial) and OSM Overpass (bbox+tags) are STRUCTURED lookups; they fit the reconstruction pipeline (parcel/footprint specific), not free-text `civic_research`. Do not fan them into `civic_research` newEvidence.

### codex owns `RustyRed-Graph-Database`

- Civic graph ingestion into the `flint` tenant so `priorKnowledge` returns hits: load PostGIS civic objects (buildings/parcels/artifacts, `name` property) into the tenant graph + `fulltext/designate (label, name)`. This unblocks the read I already shipped.
- Decide on the `query -> seeds` frontier (option 1 above). If you build it, `civic_research` calls one endpoint for the whole crawl half.

## Open decisions

1. Seed discovery owner: RustyRed frontier (codex) vs web-search key in Axum (claude-code). PICK ONE.
2. Sync vs async crawl: live crawl of N pages is slow per query. Async + read what is there, or sync with a tiny budget?
3. `/search.json` tenant scope: it defaulted to tenant `default`. Civic must read/write the `flint` tenant. Confirm SERP read is tenant-scoped.
