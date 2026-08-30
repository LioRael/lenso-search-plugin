//! Authoritative source for the Search Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SearchRequest {
    pub scope_kind: String,
    pub scope_id: String,
    pub query: String,
    pub source_kinds: Vec<String>,
    #[schemars(range(min = 1, max = 100))]
    pub limit: i64,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SearchResponseReferencesItem {
    pub source_kind: String,
    pub source_id: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SearchResponse {
    pub references: Vec<SearchResponseReferencesItem>,
    #[schemars(range(min = 0))]
    pub index_revision: i64,
}

#[derive(lenso::DomainError)]
pub enum SearchError {
    InvalidQuery,
    Forbidden,
}

#[lenso::capability(
    id = "lenso.search",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = true
)]
pub trait Search {
    async fn query_references(
        &self,
        context: lenso::Ctx<'_>,
        request: SearchRequest,
    ) -> Result<SearchResponse, SearchError>;
}
