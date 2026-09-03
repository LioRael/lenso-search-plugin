//! Agent-facing reference queries over an explicitly bound Search capability.

use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as tool_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
};
use lenso_capability_search::{self as search, SearchRequest};
use lenso_kernel::RuntimeFailure;

pub const QUERY_REFERENCES_TOOL: &str = "search_query_references";

#[lenso::plugin]
#[derive(Clone, Debug)]
struct SearchAgentToolsPlugin {
    search: Port<search::SearchClient>,
}

#[lenso::provides(tool_contract::ToolProvider)]
impl SearchAgentToolsPlugin {
    fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> impl std::future::Future<Output = PluginResult<CatalogResponse, tool_contract::CatalogError>>
    {
        let _ = self;
        futures::future::ready(Ok(CatalogResponse {
            tools: vec![ToolDefinition {
                name: QUERY_REFERENCES_TOOL.to_owned(),
                description: "Find bounded source references in one authorized scope. Re-read every result through its source Plugin before presenting content.".to_owned(),
                input_schema_json: serde_json::from_str::<serde_json::Value>(include_str!(
                    "../../lenso-capability-search/schemas/query-references-request.schema.json"
                ))
                .expect("Search Tool schema must be valid JSON")
                .to_string()
                .try_into()
                .expect("Search Tool schema must remain valid JSON"),
                execution: ToolExecutionClass::ParallelSafe,
            }],
        }))
    }

    async fn execute(
        &self,
        context: Ctx,
        request: ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        if request.name != QUERY_REFERENCES_TOOL {
            return Err(PluginError::domain(ExecuteError::NotFound));
        }
        let arguments: SearchRequest = serde_json::from_str(request.arguments_json.as_str())
            .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))?;
        match self
            .search
            .query_references_with_context(context, arguments)
            .await
        {
            Ok(response) => {
                let output_text = serde_json::to_string_pretty(&response).map_err(|error| {
                    PluginError::runtime(RuntimeFailure::PluginFailure {
                        detail: format!("Search Tool could not serialize its response: {error}"),
                    })
                })?;
                Ok(ExecuteResponse {
                    content_blocks: None,
                    content: output_text,
                    content_type: ContentType::Text,
                    metadata_json: serde_json::json!({ "tool": QUERY_REFERENCES_TOOL, "references_only": true })
                        .to_string()
                        .try_into()
                        .expect("Search Tool metadata must be valid JSON"),
                })
            }
            Err(search::SearchInvocationError::Domain(error)) => {
                let mapped = match error {
                    search::QueryReferencesError::Forbidden => ExecuteError::PermissionDenied,
                    search::QueryReferencesError::InvalidQuery => ExecuteError::InvalidArguments,
                    search::QueryReferencesError::Unknown(_) => ExecuteError::ExecutionFailed {
                        payload: ExecutionFailedPayload {
                            reason_code: "unknown_domain_error".to_owned(),
                            message: "Search rejected the reference query.".to_owned(),
                            details_json:
                                serde_json::json!({ "domain_error": "unknown_domain_error" })
                                    .to_string()
                                    .try_into()
                                    .expect("Search Tool error metadata must be valid JSON"),
                        },
                    },
                };
                Err(PluginError::domain(mapped))
            }
            Err(search::SearchInvocationError::Runtime(error)) => Err(PluginError::runtime(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_and_catalog_are_reference_only() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["plugin_id"], "lenso.search.agent-tools");
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            "lenso.agent.tool-provider@2"
        );
        let required = descriptor["required_capabilities"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0]["capability_id"], "lenso.search@1");
    }

    #[test]
    fn request_schema_is_strict_and_bounded() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../lenso-capability-search/schemas/query-references-request.schema.json"
        ))
        .unwrap();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["limit"]["maximum"], 100);
    }
}
