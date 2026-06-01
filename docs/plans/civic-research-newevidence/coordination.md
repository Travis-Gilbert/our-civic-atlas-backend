# Civic Research newEvidence: claude-code + codex coordination

Harness MCP is down. This doc is the substrate. Author: claude-code. 2026-05-31.

## Goal

`civic_research` must return live web + API results (`newEvidence`), not just the
RustyRed graph read (`priorKnowledge`). Ship the whole thing.

## Shipped this session (claude-code, on `main`)

- `67626f6`: `civic_research` reads RustyRed graph fulltext into `priorKnowledge`.
  RustyRed is empty for `flint` (0 nodes) so it returns nothing yet.
- `9ef980a`: `civic_research` degrades RustyRed `store_unavailable` to an honest
  empty state instead of a 500.

## Key finding: RustyRed has a live web crawler (the bump)

Verified live on `rustyred-production`:

- `POST /crawl` (scope `graph:write`). Body `CrawlRouteBody`:
  `{ tenant?, run_id?, seeds: [url], budget?, scope? }`. `run_live_crawl()` fetches
  the seeds LIVE (`fetch_cascade`, UA `RustyWeb/0.2 live`) and commits crawled
  `Page`/`ContentSnapshot`/`Domain` nodes into the TENANT graph. `budget.max_pages`
  default 25. Returns `{ ok, tenant, receipt, transaction, federation }`.
- `GET /search.json?q=` runs `search_substrate` over the crawled-page graph.
- `/crawl` takes SEED URLS, not a free-text query.

Implication: crawl writes into the tenant graph, so a later fulltext read finds the
crawled pages. `priorKnowledge` and `newEvidence` converge in one store.

## Architecture (RustyRed, not Theorem)

```
query
  -> seed discovery (keyless providers / frontier) -> seed URLs
  -> RustyRed POST /crawl (live fetch -> flint tenant graph)
  -> RustyRed GET /search.json (read)
  -> map -> newEvidence
```

Keyless providers (ArcGIS REST, OSM Overpass, Library of Congress) are the seed
source: they turn a civic query into URLs/records. Keyless means the Axum resolver
can call them directly in Rust (no service-tier auth, no cross-project hop).

## Split

### claude-code owns `our-civic-atlas-backend`

- `rustyred-client`: add `crawl(tenant, seeds, budget)` (POST `/crawl`) and
  `serp_search(q)` (GET `/search.json`).
- `civic_research` `newEvidence`:
  - Slice 1 (fast, ship first): call keyless providers directly in Rust ->
    `newEvidence`. Real civic data per query, no crawl needed.
  - Slice 2: use provider URLs as seeds -> RustyRed `/crawl` -> `/search.json` ->
    deeper `newEvidence`.
- Frontend: render `newEvidence` in the dossier/panel.

### codex owns `RustyRed-Graph-Database`

- Confirm `/crawl` live-fetch + robots/URL guard are right for civic seeds.
- Civic graph ingestion into the `flint` tenant so `priorKnowledge` returns hits:
  load PostGIS civic objects (buildings/parcels/artifacts, `name` property) into the
  tenant graph + `fulltext/designate (label, name)`. This is what makes the read I
  already shipped return data.
- Optional but high-value: a `query -> seeds` frontier endpoint. If RustyRed accepts
  a query and discovers seeds itself, `civic_research` calls ONE endpoint instead of
  provider + crawl + search. Decide with claude-code who owns query->URLs.
- RustyWeb commons federation (`bd1ec7a`).

## Open decisions

1. Sync vs async crawl. Live crawl of 25 pages is slow for a per-query response.
   Async crawl (background) + read what is there, or sync with a tiny budget?
2. Tenant scope. `/crawl` + `/search.json` defaulted to tenant `default` at root.
   Civic must crawl into `flint` (pass `tenant: "flint"`). Confirm the SERP read is
   tenant-scoped.
3. Seed-discovery owner: providers-in-Axum (claude-code, Slice 1) vs a
   query->seeds frontier in RustyRed (codex). Pick one.

## Next (claude-code)

Starting Slice 1: `rustyred-client` crawl/serp methods + `civic_research` provider
fanout. Committing incrementally on `main`.
