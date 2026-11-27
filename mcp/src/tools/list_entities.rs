//! `list_entities` MCP tool.
//!
//! Returns every row in the `entities` table with its resolved
//! `entity_type` and jurisdiction names — enough for the model to
//! pick an `entity_id` when calling `create_project`. The
//! entity set is bounded (firms, trusts, foundations a single law
//! practice manages) so we don't paginate. Sorted by `name`.

use serde_json::{json, Value};
use store::application_store::ApplicationStore;

use super::ToolError;

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "list_entities",
        "description": "List every legal Entity Neon Law Navigator knows about (LLCs, trusts, \
                        corporations, foundations, etc.), returning id, name, entity_type, \
                        and jurisdiction. Use this when a user wants to bind a Project to \
                        an existing Entity but only knows the name (e.g. \"the Shook family \
                        trust\"). Takes no arguments.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }
    })
}

pub async fn call(
    application_store: &ApplicationStore,
    _arguments: &Value,
) -> Result<Value, ToolError> {
    let rows = store::entities::all(application_store).await?;
    // Both reference tables live in the same engine as `entity` now.
    let types = store::entity_types::list(application_store, &[]).await?;
    let jurs = store::jurisdictions::list_all(application_store).await?;

    let by_type = |id: uuid::Uuid| {
        types
            .iter()
            .find(|t| t.id == id)
            .map_or("(unknown)", |t| t.name.as_str())
    };
    let by_jur = |id: uuid::Uuid| {
        jurs.iter()
            .find(|j| j.id == id)
            .map_or("(unknown)", |j| j.name.as_str())
    };

    let entities: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "name": row.name,
                "entity_type": by_type(row.entity_type_id),
                "jurisdiction": by_jur(row.jurisdiction_id),
            })
        })
        .collect();

    let summary = if rows.is_empty() {
        "No entities in the database.".to_string()
    } else {
        let listed = rows
            .iter()
            .map(|r| {
                format!(
                    "{} ({}, {})",
                    r.name,
                    by_type(r.entity_type_id),
                    by_jur(r.jurisdiction_id)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} entities: {listed}.", rows.len())
    };

    Ok(json!({
        "content": [{ "type": "text", "text": summary }],
        "structuredContent": {
            "count": entities.len(),
            "entities": entities,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::{call, descriptor};
    use serde_json::json;
    use uuid::Uuid;

    async fn seed_jurisdiction(
        application_store: &store::application_store::ApplicationStore,
        name: &str,
        code: &str,
    ) -> Uuid {
        store::jurisdictions::create(
            application_store,
            &store::jurisdictions::NewJurisdiction::new(name, code, "state"),
        )
        .await
        .unwrap()
        .id
    }

    async fn seed_entity_type(
        application_store: &store::application_store::ApplicationStore,
        name: &str,
    ) -> Uuid {
        store::entity_types::create(application_store, name)
            .await
            .unwrap()
            .id
    }

    async fn seed(
        application_store: &store::application_store::ApplicationStore,
        name: &str,
        et_id: Uuid,
        jur_id: Uuid,
    ) -> Uuid {
        store::entities::create(
            application_store,
            &store::entities::NewEntity {
                name: name.into(),
                entity_type_id: et_id,
                jurisdiction_id: jur_id,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    #[test]
    fn descriptor_names_the_tool_and_takes_no_arguments() {
        let d = descriptor();
        assert_eq!(d["name"], "list_entities");
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
        let props = d["inputSchema"]["properties"].as_object().unwrap();
        assert!(props.is_empty());
    }

    #[tokio::test]
    async fn empty_database_returns_zero_count_not_an_error() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let r = call(&application_store, &json!({})).await.unwrap();
            assert_eq!(r["structuredContent"]["count"], 0);
            let text = r["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("No entities"));
        })
        .await;
    }

    #[tokio::test]
    async fn returns_seeded_entities_sorted_by_name() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let jur = seed_jurisdiction(&application_store, "Nevada", "NV").await;
            let llc = seed_entity_type(&application_store, "Multi Member LLC").await;
            let trust = seed_entity_type(&application_store, "Family Trust").await;
            seed(&application_store, "Zeta Holdings", llc, jur).await;
            seed(&application_store, "Alpha Trust", trust, jur).await;
            let r = call(&application_store, &json!({})).await.unwrap();
            let names: Vec<&str> = r["structuredContent"]["entities"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["name"].as_str().unwrap())
                .collect();
            assert_eq!(names, vec!["Alpha Trust", "Zeta Holdings"]);
        })
        .await;
    }

    #[tokio::test]
    async fn each_row_carries_id_name_type_and_jurisdiction() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let jur = seed_jurisdiction(&application_store, "Nevada", "NV").await;
            let trust = seed_entity_type(&application_store, "Family Trust").await;
            seed(
                &application_store,
                crate::test_fixtures::ORGANIZATION.as_str(),
                trust,
                jur,
            )
            .await;
            let r = call(&application_store, &json!({})).await.unwrap();
            let row = &r["structuredContent"]["entities"][0];
            assert_eq!(row["name"], crate::test_fixtures::ORGANIZATION.as_str());
            assert_eq!(row["entity_type"], "Family Trust");
            assert_eq!(row["jurisdiction"], "Nevada");
            let id = row["id"].as_str().unwrap();
            uuid::Uuid::parse_str(id).expect("valid UUID");
        })
        .await;
    }

    #[tokio::test]
    async fn summary_lists_each_entity_with_type_and_jurisdiction() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let jur = seed_jurisdiction(&application_store, "Nevada", "NV").await;
            let trust = seed_entity_type(&application_store, "Family Trust").await;
            seed(
                &application_store,
                crate::test_fixtures::ORGANIZATION.as_str(),
                trust,
                jur,
            )
            .await;
            let r = call(&application_store, &json!({})).await.unwrap();
            let text = r["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("1 entities:"));
            assert!(text.contains(&format!(
                "{} (Family Trust, Nevada)",
                *crate::test_fixtures::ORGANIZATION
            )));
        })
        .await;
    }

    #[tokio::test]
    async fn ignores_arguments_silently() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let jur = seed_jurisdiction(&application_store, "Nevada", "NV").await;
            let trust = seed_entity_type(&application_store, "Family Trust").await;
            seed(
                &application_store,
                crate::test_fixtures::ORGANIZATION.as_str(),
                trust,
                jur,
            )
            .await;
            let r = call(&application_store, &json!({ "garbage": 42 }))
                .await
                .unwrap();
            assert_eq!(r["structuredContent"]["count"], 1);
        })
        .await;
    }
}
