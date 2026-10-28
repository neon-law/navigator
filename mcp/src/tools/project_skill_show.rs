//! Read one playbook from the same compiled catalog as the CLI.

use serde::Deserialize;
use serde_json::{json, Value};

use super::ToolError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    jurisdiction: String,
    practice_area: String,
}

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "project_skill_show",
        "description": "Read a Project Skill's playbook and bundled Notation codes. Jurisdiction and practice area match project_skill_list, without case sensitivity.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "jurisdiction": { "type": "string" },
                "practice_area": { "type": "string" }
            },
            "required": ["jurisdiction", "practice_area"],
            "additionalProperties": false
        }
    })
}

#[allow(clippy::unused_async)]
pub async fn call(arguments: &Value) -> Result<Value, ToolError> {
    let args: Args = super::decode_args(arguments)?;
    let entries = rules::project_skill_catalog::catalog();
    let entry =
        rules::project_skill_catalog::find(&entries, &args.jurisdiction, &args.practice_area)
            .ok_or_else(|| {
                ToolError::NotFound(
                    "no Project Skill matches; call project_skill_list for available playbooks"
                        .into(),
                )
            })?;
    Ok(json!({
        "content": [{ "type": "text", "text": entry.body.trim_end() }],
        "structuredContent": {
            "jurisdiction": entry.jurisdiction,
            "practice_area": entry.practice_area,
            "name": entry.name,
            "version": entry.version,
            "body": entry.body,
            "notations": entry.notations,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lookup_matches_the_cli_without_case_sensitivity() {
        let entries = rules::project_skill_catalog::catalog();
        let entry = rules::project_skill_catalog::find(&entries, "NV", "estates").unwrap();
        let result = call(&json!({"jurisdiction": "nv", "practice_area": "ESTATES"}))
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], entry.body.trim_end());
        assert_eq!(result["structuredContent"]["body"], entry.body);
        assert_eq!(
            result["structuredContent"]["notations"],
            json!(entry.notations)
        );
    }

    #[tokio::test]
    async fn unknown_lookup_does_not_echo_input() {
        let error =
            call(&json!({"jurisdiction": "private-input", "practice_area": "other-private-input"}))
                .await
                .unwrap_err();
        let message = error.to_string();
        assert!(!message.contains("private-input"));
        assert!(message.contains("project_skill_list"));
    }

    #[tokio::test]
    async fn lookup_requires_exactly_the_two_arguments() {
        for arguments in [
            json!({}),
            json!({"jurisdiction": "nv"}),
            json!({"jurisdiction": "nv", "practice_area": "estates", "extra": true}),
        ] {
            assert!(call(&arguments).await.is_err());
        }
    }
}
