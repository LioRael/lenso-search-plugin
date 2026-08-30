//! Authoritative source for the Search Index Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct UpsertDocumentRequest {
    pub scope_kind: String,
    pub scope_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub search_text: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct UpsertDocumentResponse {
    pub changed: bool,
    #[schemars(range(min = 0))]
    pub index_revision: i64,
}

#[derive(lenso::DomainError)]
pub enum UpsertDocumentError {
    InvalidDocument,
    Forbidden,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct DeleteDocumentRequest {
    pub scope_kind: String,
    pub scope_id: String,
    pub source_kind: String,
    pub source_id: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct DeleteDocumentResponse {
    pub changed: bool,
    #[schemars(range(min = 0))]
    pub index_revision: i64,
}

#[derive(lenso::DomainError)]
pub enum DeleteDocumentError {
    InvalidDocument,
    Forbidden,
}

#[lenso::capability(
    id = "lenso.search-index",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = true
)]
pub trait SearchIndex {
    async fn upsert_document(
        &self,
        context: lenso::Ctx<'_>,
        request: UpsertDocumentRequest,
    ) -> Result<UpsertDocumentResponse, UpsertDocumentError>;

    async fn delete_document(
        &self,
        context: lenso::Ctx<'_>,
        request: DeleteDocumentRequest,
    ) -> Result<DeleteDocumentResponse, DeleteDocumentError>;
}
