# Civic Research newEvidence: claude-code + codex coordination

Harness MCP is down. This doc + git commits are the substrate. Author: claude-code.

## Shipped + live (all on `main`)

`civic_research` `newEvidence` is now a keyless provider fanout + a RustyRed
frontier crawl. Every piece is best-effort: a failure is logged and skipped, so
`civic_research` never regresses.

| Commit | What |
|---|---|
| `67626f6` | `priorKnowledge` = RustyRed graph fulltext read |
| `9ef980a` | degrade RustyRed `store_unavailable` to empty, not 500 |
| `7606892` | Library of Congress newEvidence (free-text, live-verified) |
| `97beec9` | frontier crawl: provider URLs -> RustyRed `POST /crawl` -> `GET /search.json` -> crawled-page newEvidence. Adds `rustyred-client` `crawl()` + `serp_search()` |
| `18d45e5` | ArcGIS REST + OSM Overpass providers |
| `704da02` | ArcGIS uses `WHERE LIKE` (Flint layer rejects `text=`); live-verified returns real Flint parcels |

Config set: `ARCGIS_REST_ENDPOINTS` = the Flint `Main_COF_Parcel_view` layer on
the backend Railway service.

A query now returns: LoC documents + RustyRed-crawled pages + ArcGIS Flint
parcels (+ OSM footprints when the query carries a bbox). The Node sidecar folds
`newEvidence` into `sources`/`signals`; the panel renders them (no frontend
change). Verified independently: LoC returns Flint hits; ArcGIS `WHERE` returns
real parcels (e.g. "2513 N SAGINAW ST | YALDO, FERANDO").

## Architecture (RustyRed, not Theorem; Postgres <- RustyRed)

```
query
  -> providers (LoC free-text, ArcGIS WHERE, OSM bbox)  -> newEvidence + seed URLs
  -> RustyRed POST /crawl (live fetch of seeds -> flint graph)  [acquisition: feeds RustyRed -> Postgres]
  -> RustyRed GET /search.json  -> crawled-page newEvidence
```

The frontier crawl is the acquisition front: it populates the RustyRed `flint`
graph, the intended source for Postgres. `priorKnowledge` (the fulltext graph
read) grows from this over time.

## Remaining for the full data flow (codex / prereqs)

1. RustyRed token scope. The backend `RUSTYRED_API_TOKEN` must have `graph:write`,
   or `POST /crawl` returns 403 and the frontier crawl no-ops (providers still
   return). Confirm the token's scopes.
2. `/search.json` tenant scope. It defaulted to tenant `default`; the client
   passes `?tenant=flint`. Confirm RustyRed honors it, else the SERP reads the
   wrong substrate and crawled hits never surface.
3. `priorKnowledge` designation. For the fulltext graph read to surface
   crawled/civic nodes, the `flint` graph needs a `(label, property)`
   `fulltext/designate`. (The crawl-substrate SERP needs no designation.)

Items 1-2 make the frontier crawl's crawled-page hits actually appear; item 3
makes `priorKnowledge` non-empty. Providers (LoC, ArcGIS) work without any.

## Notes

- OSM Overpass fires only when `scope_json` carries a bbox (structured/spatial);
  free-text queries skip it.
- ArcGIS search fields are `ARCGIS_SEARCH_FIELDS`-overridable (default the Flint
  parcel address/owner columns) so other layers are not hardcoded wrong.
