//! List the shared, compiled Project Skill catalog.

use serde::Deserialize;
use serde_json::{json, Value};

use super::ToolError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {}

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "project_skill_list",
        "description": "List Project Skills by jurisdiction and practice area, with each playbook's name and version. Use project_skill_show to read a playbook.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
    })
}

#[allow(clippy::unused_async)]
pub async fn call(arguments: &Value) -> Result<Value, ToolError> {
    let _: Args = super::decode_args(arguments)?;
    let entries: Vec<Value> = rules::project_skill_catalog::catalog()
        .iter()
        .map(|entry| {
            json!({
                "jurisdiction": entry.jurisdiction,
                "practice_area": entry.practice_area,
                "name": entry.name,
                "version": entry.version,
            })
        })
        .collect();
    Ok(json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&entries).map_err(|_| ToolError::Internal("serialize Project Skills".into()))? }],
        "structuredContent": { "skills": entries, "count": entries.len() }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn listing_contains_every_skill_in_catalog_order() {
        let result = call(&json!({})).await.unwrap();
        let entries = rules::project_skill_catalog::catalog();
        let skills = result["structuredContent"]["skills"].as_array().unwrap();
        assert_eq!(skills.len(), entries.len());
        assert!(!skills.is_empty());
        for (skill, expected) in skills.iter().zip(entries) {
            assert_eq!(
                skill,
                &json!({
                    "jurisdiction": expected.jurisdiction,
                    "practice_area": expected.practice_area,
                    "name": expected.name,
                    "version": expected.version,
                })
            );
        }
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
            json!(skills)
        );
    }

    #[tokio::test]
    async fn listing_refuses_unexpected_arguments() {
        assert!(call(&json!({"project": "unused"})).await.is_err());
    }
}
