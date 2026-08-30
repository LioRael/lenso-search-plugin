//! Rebuildable full-text index that returns source references only.

mod operator;
mod schema;

use std::{cell::RefCell, fmt, rc::Rc, time::Duration};

use lenso::{ActivateContext, DeactivateContext, Lifecycle, Port, provides};
use lenso_capability_search as search;
use lenso_capability_search::{
    QueryReferencesError as SearchError, Search, SearchRequest, SearchResponse,
    SearchResponseReferencesItem,
};
use lenso_capability_search_index as index;
use lenso_capability_search_index::{
    DeleteDocumentError, DeleteDocumentRequest, DeleteDocumentResponse, SearchIndexDeleteDocument,
    SearchIndexUpsertDocument, UpsertDocumentError, UpsertDocumentRequest, UpsertDocumentResponse,
};
use lenso_capability_secrets as secrets;
use lenso_capability_secrets::{ResolveRequest, SecretsClient, SecretsInvocationError};
use lenso_kernel::{InvocationContext, NativeRequestFuture, RuntimeFailure};
use lenso_postgres_kit::OwnedPostgres;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::schema::schema_plan;

pub use operator::{SearchOperator, SearchOperatorError};

const DEPENDENCY_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_SEARCH_TEXT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    schema: String,
    database_url_secret: String,
    query_callers: Vec<String>,
    indexer_callers: Vec<String>,
}

impl SearchConfig {
    pub fn new(
        schema: impl Into<String>,
        database_url_secret: impl Into<String>,
        query_callers: Vec<String>,
        indexer_callers: Vec<String>,
    ) -> Result<Self, SearchConfigError> {
        let value = Self {
            schema: schema.into(),
            database_url_secret: database_url_secret.into(),
            query_callers,
            indexer_callers,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), SearchConfigError> {
        schema_plan(self.schema.clone()).map_err(|_| SearchConfigError::InvalidSchema)?;
        if !valid_secret_reference(&self.database_url_secret) {
            return Err(SearchConfigError::InvalidSecretReference);
        }
        if self.query_callers.is_empty()
            || self.query_callers.iter().any(|caller| !valid_name(caller))
        {
            return Err(SearchConfigError::InvalidQueryCallers);
        }
        if self.indexer_callers.is_empty()
            || self
                .indexer_callers
                .iter()
                .any(|caller| !valid_name(caller))
        {
            return Err(SearchConfigError::InvalidIndexerCallers);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SearchConfigError {
    #[error("invalid owned PostgreSQL schema")]
    InvalidSchema,
    #[error("invalid database URL secret reference")]
    InvalidSecretReference,
    #[error("at least one valid query caller is required")]
    InvalidQueryCallers,
    #[error("at least one valid indexer caller is required")]
    InvalidIndexerCallers,
}

fn validate_config(config: &SearchConfig) -> Result<(), RuntimeFailure> {
    config
        .validate()
        .map_err(|error| RuntimeFailure::InvalidResolvedPlan {
            detail: error.to_string(),
        })
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "configuration.schema.json",
    validate = validate_config
)]
#[derive(Clone)]
struct SearchPlugin {
    #[config]
    config: SearchConfig,
    secrets: Port<secrets::SecretsClient>,
    state: Rc<RefCell<Option<PreparedSearch>>>,
}

#[derive(Clone)]
struct PreparedSearch {
    postgres: OwnedPostgres,
}

impl fmt::Debug for PreparedSearch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSearch")
            .field("schema", &self.postgres.schema())
            .finish()
    }
}

impl fmt::Debug for SearchPlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SearchPlugin")
            .field("prepared", &self.state.borrow().is_some())
            .field("query_caller_count", &self.config.query_callers.len())
            .field("indexer_caller_count", &self.config.indexer_callers.len())
            .finish_non_exhaustive()
    }
}

#[provides(search::Search, index::SearchIndex)]
impl SearchPlugin {}

impl SearchPlugin {
    fn prepared(&self) -> Result<PreparedSearch, RuntimeFailure> {
        self.state
            .borrow()
            .clone()
            .ok_or(RuntimeFailure::PluginFailure {
                detail: "Search Plugin is not prepared".to_owned(),
            })
    }

    #[allow(clippy::needless_pass_by_value)]
    fn query_references(
        &self,
        context: InvocationContext,
        request: SearchRequest,
    ) -> NativeRequestFuture<Search> {
        let authorized = caller_allowed(&context, &self.config.query_callers);
        let prepared = self.prepared();
        Box::pin(async move {
            if !authorized {
                return Ok(Err(SearchError::Forbidden));
            }
            let query = request.query.trim();
            if !valid_dimension(&request.scope_kind)
                || !valid_name(&request.scope_id)
                || query.is_empty()
                || query.len() > 512
                || query.chars().any(char::is_control)
                || request.source_kinds.is_empty()
                || request.source_kinds.len() > 32
                || request
                    .source_kinds
                    .iter()
                    .any(|source_kind| !valid_dimension(source_kind))
                || !(1..=100).contains(&request.limit)
            {
                return Ok(Err(SearchError::InvalidQuery));
            }
            let prepared = prepared?;
            let mut transaction = prepared.postgres.pool().begin().await.map_err(|source| {
                runtime(SearchPluginError::Database {
                    operation: "begin search snapshot",
                    source,
                })
            })?;
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
                .execute(&mut *transaction)
                .await
                .map_err(|source| {
                    runtime(SearchPluginError::Database {
                        operation: "set search snapshot",
                        source,
                    })
                })?;
            let revision: Option<i64> = sqlx::query_scalar(
                "SELECT index_revision FROM search_scopes WHERE scope_kind=$1 AND scope_id=$2",
            )
            .bind(&request.scope_kind)
            .bind(&request.scope_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| {
                runtime(SearchPluginError::Database {
                    operation: "read search revision",
                    source,
                })
            })?;
            let rows = sqlx::query("SELECT source_kind,source_id FROM search_documents WHERE scope_kind=$1 AND scope_id=$2 AND source_kind=ANY($3) AND search_vector @@ plainto_tsquery('simple'::regconfig,$4) ORDER BY ts_rank_cd(search_vector,plainto_tsquery('simple'::regconfig,$4)) DESC,source_kind,source_id LIMIT $5")
                .bind(&request.scope_kind)
                .bind(&request.scope_id)
                .bind(&request.source_kinds)
                .bind(query)
                .bind(request.limit)
                .fetch_all(&mut *transaction)
                .await
                .map_err(|source| runtime(SearchPluginError::Database { operation: "query search index", source }))?;
            transaction.commit().await.map_err(|source| {
                runtime(SearchPluginError::Database {
                    operation: "commit search snapshot",
                    source,
                })
            })?;
            let mut references = Vec::with_capacity(rows.len());
            for row in rows {
                references.push(SearchResponseReferencesItem {
                    source_kind: decode(&row, "source_kind", "decode search source kind")?,
                    source_id: decode(&row, "source_id", "decode search source id")?,
                });
            }
            Ok(Ok(SearchResponse {
                references,
                index_revision: revision.unwrap_or(0),
            }))
        })
    }

    #[allow(clippy::needless_pass_by_value)]
    fn upsert_document(
        &self,
        context: InvocationContext,
        request: UpsertDocumentRequest,
    ) -> NativeRequestFuture<SearchIndexUpsertDocument> {
        let authorized = caller_allowed(&context, &self.config.indexer_callers);
        let prepared = self.prepared();
        Box::pin(async move {
            if !authorized {
                return Ok(Err(UpsertDocumentError::Forbidden));
            }
            let search_text = request.search_text.trim();
            if !valid_document_reference(
                &request.scope_kind,
                &request.scope_id,
                &request.source_kind,
                &request.source_id,
            ) || search_text.is_empty()
                || search_text.len() > MAX_SEARCH_TEXT_BYTES
                || search_text.contains('\0')
            {
                return Ok(Err(UpsertDocumentError::InvalidDocument));
            }
            let prepared = prepared?;
            let mut transaction = prepared.postgres.pool().begin().await.map_err(|source| {
                runtime(SearchPluginError::Database {
                    operation: "begin search upsert",
                    source,
                })
            })?;
            ensure_scope(&mut transaction, &request.scope_kind, &request.scope_id).await?;
            let revision =
                lock_revision(&mut transaction, &request.scope_kind, &request.scope_id).await?;
            let existing: Option<String> = sqlx::query_scalar("SELECT search_text FROM search_documents WHERE scope_kind=$1 AND scope_id=$2 AND source_kind=$3 AND source_id=$4")
                .bind(&request.scope_kind).bind(&request.scope_id).bind(&request.source_kind).bind(&request.source_id)
                .fetch_optional(&mut *transaction).await
                .map_err(|source| runtime(SearchPluginError::Database { operation: "read indexed document", source }))?;
            let changed = existing.as_deref() != Some(search_text);
            if changed {
                sqlx::query("INSERT INTO search_documents(scope_kind,scope_id,source_kind,source_id,search_text) VALUES($1,$2,$3,$4,$5) ON CONFLICT(scope_kind,scope_id,source_kind,source_id) DO UPDATE SET search_text=EXCLUDED.search_text,updated_at=transaction_timestamp()")
                    .bind(&request.scope_kind).bind(&request.scope_id).bind(&request.source_kind).bind(&request.source_id).bind(search_text)
                    .execute(&mut *transaction).await
                    .map_err(|source| runtime(SearchPluginError::Database { operation: "upsert indexed document", source }))?;
            }
            let index_revision = if changed {
                advance_revision(&mut transaction, &request.scope_kind, &request.scope_id).await?
            } else {
                revision
            };
            transaction.commit().await.map_err(|source| {
                runtime(SearchPluginError::Database {
                    operation: "commit search upsert",
                    source,
                })
            })?;
            Ok(Ok(UpsertDocumentResponse {
                changed,
                index_revision,
            }))
        })
    }

    #[allow(clippy::needless_pass_by_value)]
    fn delete_document(
        &self,
        context: InvocationContext,
        request: DeleteDocumentRequest,
    ) -> NativeRequestFuture<SearchIndexDeleteDocument> {
        let authorized = caller_allowed(&context, &self.config.indexer_callers);
        let prepared = self.prepared();
        Box::pin(async move {
            if !authorized {
                return Ok(Err(DeleteDocumentError::Forbidden));
            }
            if !valid_document_reference(
                &request.scope_kind,
                &request.scope_id,
                &request.source_kind,
                &request.source_id,
            ) {
                return Ok(Err(DeleteDocumentError::InvalidDocument));
            }
            let prepared = prepared?;
            let mut transaction = prepared.postgres.pool().begin().await.map_err(|source| {
                runtime(SearchPluginError::Database {
                    operation: "begin search delete",
                    source,
                })
            })?;
            let revision: Option<i64> = sqlx::query_scalar("SELECT index_revision FROM search_scopes WHERE scope_kind=$1 AND scope_id=$2 FOR UPDATE")
                .bind(&request.scope_kind).bind(&request.scope_id)
                .fetch_optional(&mut *transaction).await
                .map_err(|source| runtime(SearchPluginError::Database { operation: "lock search scope", source }))?;
            let Some(revision) = revision else {
                transaction.commit().await.map_err(|source| {
                    runtime(SearchPluginError::Database {
                        operation: "commit empty search delete",
                        source,
                    })
                })?;
                return Ok(Ok(DeleteDocumentResponse {
                    changed: false,
                    index_revision: 0,
                }));
            };
            let changed = sqlx::query("DELETE FROM search_documents WHERE scope_kind=$1 AND scope_id=$2 AND source_kind=$3 AND source_id=$4")
                .bind(&request.scope_kind).bind(&request.scope_id).bind(&request.source_kind).bind(&request.source_id)
                .execute(&mut *transaction).await
                .map_err(|source| runtime(SearchPluginError::Database { operation: "delete indexed document", source }))?
                .rows_affected() == 1;
            let index_revision = if changed {
                advance_revision(&mut transaction, &request.scope_kind, &request.scope_id).await?
            } else {
                revision
            };
            transaction.commit().await.map_err(|source| {
                runtime(SearchPluginError::Database {
                    operation: "commit search delete",
                    source,
                })
            })?;
            Ok(Ok(DeleteDocumentResponse {
                changed,
                index_revision,
            }))
        })
    }
}

impl Lifecycle for SearchPlugin {
    async fn activate(&self, context: ActivateContext) -> Result<(), RuntimeFailure> {
        let dependencies = context.dependencies().clone();
        let database_url = resolve_secret(
            &self.secrets,
            &dependencies,
            context.cancellation(),
            &self.config.database_url_secret,
        )
        .await?;
        let postgres = OwnedPostgres::prepare(
            &database_url,
            schema_plan(self.config.schema.clone()).map_err(|error| {
                RuntimeFailure::InvalidResolvedPlan {
                    detail: error.to_string(),
                }
            })?,
        )
        .await
        .map_err(|error| RuntimeFailure::PluginFailure {
            detail: error.to_string(),
        })?;
        self.state.replace(Some(PreparedSearch { postgres }));
        Ok(())
    }

    async fn deactivate(&self, _context: DeactivateContext) -> Result<(), RuntimeFailure> {
        let prepared = self.state.borrow_mut().take();
        if let Some(prepared) = prepared {
            prepared.postgres.pool().close().await;
        }
        Ok(())
    }
}

async fn ensure_scope(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope_kind: &str,
    scope_id: &str,
) -> Result<(), RuntimeFailure> {
    sqlx::query(
        "INSERT INTO search_scopes(scope_kind,scope_id) VALUES($1,$2) ON CONFLICT DO NOTHING",
    )
    .bind(scope_kind)
    .bind(scope_id)
    .execute(&mut **transaction)
    .await
    .map_err(|source| {
        runtime(SearchPluginError::Database {
            operation: "ensure search scope",
            source,
        })
    })?;
    Ok(())
}

async fn lock_revision(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope_kind: &str,
    scope_id: &str,
) -> Result<i64, RuntimeFailure> {
    sqlx::query_scalar(
        "SELECT index_revision FROM search_scopes WHERE scope_kind=$1 AND scope_id=$2 FOR UPDATE",
    )
    .bind(scope_kind)
    .bind(scope_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|source| {
        runtime(SearchPluginError::Database {
            operation: "lock search scope",
            source,
        })
    })
}

async fn advance_revision(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope_kind: &str,
    scope_id: &str,
) -> Result<i64, RuntimeFailure> {
    sqlx::query_scalar("UPDATE search_scopes SET index_revision=index_revision+1 WHERE scope_kind=$1 AND scope_id=$2 RETURNING index_revision")
        .bind(scope_kind).bind(scope_id).fetch_one(&mut **transaction).await
        .map_err(|source| runtime(SearchPluginError::Database { operation: "advance search revision", source }))
}

fn decode<T>(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
    operation: &'static str,
) -> Result<T, RuntimeFailure>
where
    for<'row> T: sqlx::Decode<'row, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(column)
        .map_err(|source| runtime(SearchPluginError::Database { operation, source }))
}

#[derive(Debug, Error)]
enum SearchPluginError {
    #[error("PostgreSQL operation `{operation}` failed")]
    Database {
        operation: &'static str,
        #[source]
        source: sqlx::Error,
    },
}

fn runtime(error: impl fmt::Display) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: error.to_string(),
    }
}

fn caller_allowed(context: &InvocationContext, allowed: &[String]) -> bool {
    context
        .caller_instance()
        .is_some_and(|caller| allowed.iter().any(|candidate| candidate == caller))
}

fn valid_document_reference(
    scope_kind: &str,
    scope_id: &str,
    source_kind: &str,
    source_id: &str,
) -> bool {
    valid_dimension(scope_kind)
        && valid_name(scope_id)
        && valid_dimension(source_kind)
        && valid_name(source_id)
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_dimension(value: &str) -> bool {
    valid_name(value) && !value.starts_with('.') && !value.ends_with('.')
}

fn valid_secret_reference(reference: &str) -> bool {
    !reference.is_empty()
        && reference.len() <= 256
        && !reference.starts_with('/')
        && !reference.ends_with('/')
        && !reference.contains("//")
        && reference
            .split('/')
            .all(|segment| segment != "." && segment != "..")
        && reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

async fn resolve_secret(
    secrets: &SecretsClient,
    dependencies: &lenso_kernel::PluginDependencies,
    cancellation: lenso_kernel::CancellationToken,
    reference: &str,
) -> Result<Zeroizing<String>, RuntimeFailure> {
    let context = dependencies.invocation_context_after(DEPENDENCY_TIMEOUT, cancellation)?;
    secrets
        .resolve_with_context(
            context,
            ResolveRequest {
                reference: reference.to_owned(),
            },
        )
        .await
        .map(|value| Zeroizing::new(value.value))
        .map_err(|error| match error {
            SecretsInvocationError::Domain(_) => RuntimeFailure::PluginFailure {
                detail: format!("secret `{reference}` was rejected"),
            },
            SecretsInvocationError::Runtime(error) => error,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_kernel::CancellationToken;
    use sqlx::{AssertSqlSafe, Executor};

    fn plugin() -> SearchPlugin {
        SearchPlugin {
            config: SearchConfig::new(
                "search",
                "search/database",
                vec!["work-service".to_owned()],
                vec!["document-indexer".to_owned()],
            )
            .unwrap(),
            secrets: Port::default(),
            state: Rc::new(RefCell::new(None)),
        }
    }

    #[test]
    fn configuration_requires_trusted_query_and_index_callers() {
        assert_eq!(
            SearchConfig::new(
                "search",
                "search/database",
                Vec::new(),
                vec!["indexer".to_owned()]
            )
            .unwrap_err(),
            SearchConfigError::InvalidQueryCallers
        );
    }

    #[tokio::test]
    async fn direct_untrusted_query_is_rejected_before_storage_access() {
        let context = InvocationContext::new(1, None, CancellationToken::new())
            .with_caller_instance("untrusted");
        let result = plugin()
            .query_references(
                context,
                SearchRequest {
                    scope_kind: "organization".to_owned(),
                    scope_id: "org_acme".to_owned(),
                    query: "quarterly report".to_owned(),
                    source_kinds: vec!["document".to_owned()],
                    limit: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(result, Err(SearchError::Forbidden));
    }

    #[tokio::test]
    async fn oversized_document_is_rejected_before_storage_access() {
        let context = InvocationContext::new(1, None, CancellationToken::new())
            .with_caller_instance("document-indexer");
        let result = plugin()
            .upsert_document(
                context,
                UpsertDocumentRequest {
                    scope_kind: "organization".to_owned(),
                    scope_id: "org_acme".to_owned(),
                    source_kind: "document".to_owned(),
                    source_id: "doc_1".to_owned(),
                    search_text: "x".repeat(MAX_SEARCH_TEXT_BYTES + 1),
                },
            )
            .await
            .unwrap();
        assert_eq!(result, Err(UpsertDocumentError::InvalidDocument));
    }

    #[tokio::test]
    async fn postgres_nul_document_is_rejected_as_a_domain_error() {
        let context = InvocationContext::new(1, None, CancellationToken::new())
            .with_caller_instance("document-indexer");
        let result = plugin()
            .upsert_document(
                context,
                UpsertDocumentRequest {
                    scope_kind: "organization".to_owned(),
                    scope_id: "org_acme".to_owned(),
                    source_kind: "document".to_owned(),
                    source_id: "doc_1".to_owned(),
                    search_text: "quarterly\0report".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(result, Err(UpsertDocumentError::InvalidDocument));
    }

    #[tokio::test]
    #[ignore = "requires LENSO_POSTGRES_TEST_URL"]
    async fn index_is_idempotent_rebuildable_and_reference_only() {
        let database_url =
            std::env::var("LENSO_POSTGRES_TEST_URL").expect("LENSO_POSTGRES_TEST_URL is required");
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let schema = format!("search_test_{}_{suffix}", std::process::id());
        SearchOperator::setup(&database_url, &schema).await.unwrap();
        let postgres = OwnedPostgres::prepare(&database_url, schema_plan(schema.clone()).unwrap())
            .await
            .unwrap();
        let plugin = plugin();
        plugin.state.replace(Some(PreparedSearch { postgres }));
        let indexer = InvocationContext::new(1, None, CancellationToken::new())
            .with_caller_instance("document-indexer");
        let document = UpsertDocumentRequest {
            scope_kind: "organization".to_owned(),
            scope_id: "org_acme".to_owned(),
            source_kind: "document".to_owned(),
            source_id: "doc_1".to_owned(),
            search_text: "Quarterly report finance".to_owned(),
        };
        let first = plugin
            .upsert_document(indexer.clone(), document.clone())
            .await
            .unwrap()
            .unwrap();
        assert!(first.changed);
        assert_eq!(first.index_revision, 1);
        let replay = plugin
            .upsert_document(indexer.clone(), document)
            .await
            .unwrap()
            .unwrap();
        assert!(!replay.changed);
        assert_eq!(replay.index_revision, 1);
        let query = InvocationContext::new(2, None, CancellationToken::new())
            .with_caller_instance("work-service");
        let found = plugin
            .query_references(
                query.clone(),
                SearchRequest {
                    scope_kind: "organization".to_owned(),
                    scope_id: "org_acme".to_owned(),
                    query: "quarterly".to_owned(),
                    source_kinds: vec!["document".to_owned()],
                    limit: 10,
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.index_revision, 1);
        assert_eq!(found.references.len(), 1);
        assert_eq!(found.references[0].source_id, "doc_1");
        let deleted = plugin
            .delete_document(
                indexer,
                DeleteDocumentRequest {
                    scope_kind: "organization".to_owned(),
                    scope_id: "org_acme".to_owned(),
                    source_kind: "document".to_owned(),
                    source_id: "doc_1".to_owned(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(deleted.changed);
        assert_eq!(deleted.index_revision, 2);
        let missing = plugin
            .query_references(
                query,
                SearchRequest {
                    scope_kind: "organization".to_owned(),
                    scope_id: "org_acme".to_owned(),
                    query: "quarterly".to_owned(),
                    source_kinds: vec!["document".to_owned()],
                    limit: 10,
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(missing.references.is_empty());
        assert_eq!(missing.index_revision, 2);

        let cleanup_pool = sqlx::PgPool::connect(&database_url).await.unwrap();
        cleanup_pool
            .execute(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE")))
            .await
            .unwrap();
        cleanup_pool.close().await;
    }
}
