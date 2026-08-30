# vNext Search Plugin card

## Owner and deletion boundary

`lenso-search-postgres-plugin` owns only a rebuildable PostgreSQL full-text
index and per-scope index revision. Source Plugins retain all business state.

## Roles and authority

- `lenso.search-index@1` accepts upserts and deletes only from exact configured
  indexer Instance keys.
- `lenso.search@1` accepts queries only from exact configured target Plugins and
  returns opaque references without indexed text.
- Requires `lenso.secrets@1` for the private database URL.

## First observable behavior

An indexer upserts stable source references and searchable text. Material
changes advance the scope revision transactionally. A query uses PostgreSQL's
`simple` full-text configuration and returns ranked references. The caller must
re-read each source and remains final authority; Search is never a policy
enforcement point.
