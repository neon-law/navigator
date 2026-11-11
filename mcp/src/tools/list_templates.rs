//! Read-only discovery of public, bundled notation Templates.

use serde::Deserialize;
use serde_json::{json, Value};

use super::ToolError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    intent: Option<String>,
}

/// Clarify a bare request to create Work In Progress before suggesting a Template.
#[must_use]
pub fn clarify_intent(intent: &str) -> Option<&'static str> {
    let normalized = intent
        .trim()
        .trim_end_matches(['.', '?', '!'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "create a work in progress" | "create work in progress" | "create a wip" | "create wip"
    )
    .then_some(
        "Do you want a Work In Progress summary for a matter, or do you want to record time or a cost?",
    )
}

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "list_templates",
        "description": "List public Templates from Navigator's bundled shared catalog. For a natural-language \
                        request to create a document or Work In Progress, pass the user's wording as intent. \
                        Ask any returned clarification before suggesting a Template or taking action. \
                        Use the titles and descriptions to match clear requests, and ask when several fit.",
        "annotations": { "readOnlyHint": true },
        "inputSchema": {
            "type": "object",
            "properties": { "intent": { "type": "string" } },
            "additionalProperties": false
        }
    })
}

pub fn call(arguments: &Value) -> Result<Value, ToolError> {
    let arguments: Arguments = serde_json::from_value(arguments.clone())
        .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
    if let Some(question) = arguments.intent.as_deref().and_then(clarify_intent) {
        return Ok(json!({
            "content": [{ "type": "text", "text": question }],
            "structuredContent": { "clarification": question }
        }));
    }

    let templates = store::seed::public_seeded_templates()
        .map_err(|error| ToolError::Internal(error.to_string()))?;
    let text = templates
        .iter()
        .map(|template| match &template.description {
            Some(description) => format!("{} ({}) — {description}", template.title, template.code),
            None => format!("{} ({})", template.title, template.code),
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": { "templates": templates }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_returns_public_metadata_or_a_clarification() {
        assert!(super::super::is_advertised("list_templates"));
        assert!(!super::super::is_side_effecting("list_templates"));

        let result = call(&json!({})).unwrap();
        let templates = result["structuredContent"]["templates"].as_array().unwrap();
        assert!(templates.iter().any(|row| row["code"] == "mutual_nda"));
        assert!(templates
            .iter()
            .all(|row| row["code"] != "onboarding__letter"));
        assert!(templates
            .iter()
            .all(|row| row.as_object().unwrap().len() == 3));

        let result = call(&json!({ "intent": "Create a work in progress!" })).unwrap();
        assert!(result["structuredContent"]["clarification"].is_string());
        assert!(result["structuredContent"].get("templates").is_none());
    }
}
