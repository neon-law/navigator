//! `list_jurisdictions` MCP tool.
//!
//! Returns every row in the `jurisdiction` table — which lives in
//! `SurrealDB` since its slice of #1093 (ENG-20). The list is small and
//! bounded (US states + federal + a handful of foreign), so we don't
//! paginate — the model gets the complete enumeration in one response
//! and can pick the right code (`NV`, `CA`, `US`) without a follow-up
//! call. Sorted by `name` for stable output.

use serde_json::{json, Value};
use store::application_store::ApplicationStore;

use super::ToolError;

/// Tool descriptor advertised by `tools/list`.
#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "list_jurisdictions",
        "description": "List every jurisdiction Neon Law Navigator knows about \
                        (US states, federal, foreign), returning id, name, \
                        and short code (`NV`, `CA`, `US`). Use this when a \
                        user asks where an entity can be organized, what \
                        codes are valid, or to disambiguate a name to a code. \
                        Takes no arguments.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }
    })
}

/// Read every jurisdiction and return the MCP `result` payload.
pub async fn call(
    application_store: &ApplicationStore,
    _arguments: &Value,
) -> Result<Value, ToolError> {
    let rows = store::jurisdictions::list_all(application_store).await?;

    let jurisdictions: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "name": row.name,
                "code": row.code,
            })
        })
        .collect();

    let summary = if rows.is_empty() {
        "No jurisdictions in the database.".to_string()
    } else {
        let listed = rows
            .iter()
            .map(|r| format!("{} ({})", r.name, r.code))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} jurisdictions: {listed}.", rows.len())
    };

    Ok(json!({
        "content": [{ "type": "text", "text": summary }],
        "structuredContent": {
            "count": jurisdictions.len(),
            "jurisdictions": jurisdictions,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::{call, descriptor};
    use serde_json::json;

    async fn seed(
        application_store: &store::application_store::ApplicationStore,
        name: &str,
        code: &str,
    ) {
        store::jurisdictions::create(
            application_store,
            &store::jurisdictions::NewJurisdiction::new(name, code, "state"),
        )
        .await
        .unwrap();
    }

    #[test]
    fn descriptor_names_the_tool_under_tool_namespace() {
        let d = descriptor();
        assert_eq!(d["name"], "list_jurisdictions");
        // No required fields — caller passes `{}`.
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
        let props = d["inputSchema"]["properties"].as_object().unwrap();
        assert!(props.is_empty(), "tool takes no arguments");
    }

    #[tokio::test]
    async fn empty_database_returns_zero_count_not_an_error() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let result = call(&application_store, &json!({})).await.unwrap();
            assert_eq!(result["structuredContent"]["count"], 0);
            assert_eq!(
                result["structuredContent"]["jurisdictions"]
                    .as_array()
                    .unwrap()
                    .len(),
                0
            );
            let text = result["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("No jurisdictions"), "got: {text}");
        })
        .await;
    }

    #[tokio::test]
    async fn returns_every_seeded_jurisdiction_sorted_by_name() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            // Insert out of alphabetical order so we can prove the sort.
            seed(&application_store, "Nevada", "NV").await;
            seed(&application_store, "California", "CA").await;
            seed(&application_store, "Alabama", "AL").await;

            let result = call(&application_store, &json!({})).await.unwrap();
            assert_eq!(result["structuredContent"]["count"], 3);
            let names: Vec<&str> = result["structuredContent"]["jurisdictions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|j| j["name"].as_str().unwrap())
                .collect();
            assert_eq!(names, vec!["Alabama", "California", "Nevada"]);
        })
        .await;
    }

    #[tokio::test]
    async fn each_row_carries_id_name_and_code() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            seed(&application_store, "Nevada", "NV").await;
            let result = call(&application_store, &json!({})).await.unwrap();
            let row = &result["structuredContent"]["jurisdictions"][0];
            assert_eq!(row["name"], "Nevada");
            assert_eq!(row["code"], "NV");
            // `id` is a UUID hex string.
            let id = row["id"].as_str().expect("id present");
            uuid::Uuid::parse_str(id).expect("valid UUID");
        })
        .await;
    }

    #[tokio::test]
    async fn ignores_arguments_silently() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            seed(&application_store, "Nevada", "NV").await;
            let result = call(&application_store, &json!({ "garbage": 42 }))
                .await
                .unwrap();
            assert_eq!(result["structuredContent"]["count"], 1);
        })
        .await;
    }

    #[tokio::test]
    async fn summary_lists_name_and_code_pairs() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            seed(&application_store, "Nevada", "NV").await;
            seed(&application_store, "California", "CA").await;
            let result = call(&application_store, &json!({})).await.unwrap();
            let text = result["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("2 jurisdictions:"), "got: {text}");
            assert!(text.contains("Nevada (NV)"));
            assert!(text.contains("California (CA)"));
        })
        .await;
    }
}
