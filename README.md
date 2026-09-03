# Lenso Search Plugin

This repository contains a removable vNext Search deletion boundary. Source
Plugins submit rebuildable text through `lenso.search-index@1`. Trusted query
Plugins receive only `{source_kind, source_id}` references from
`lenso.search@1`.

Search never returns indexed text, titles, snippets, or business objects. A
target Plugin must re-read every source reference and perform final
authorization before exposing anything to a user. Removing Search deletes an
index, not source-of-truth business state.

The private, stateless `lenso.search.agent-tools` adapter exposes only bounded
reference queries to an Agent. It does not expose indexing, indexed text, or a
shortcut around source re-read and final authorization.
