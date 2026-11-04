//! The saved questionnaire answers file: one `state: value` YAML pair per
//! answered question, written by `--save-answers` and read by
//! `--answers-from` / `--answers-file`. A notation's interpolated fields are
//! a pure function of its filed answers, so a second notation prefilled from
//! a first one's file renders identical fields — the round trip
//! `--save-answers` and `--answers-from` close.
//!
//! Answers with no `state_name` (provenance-only rows the walker wrote for
//! questions outside the chain) have no state to key them and are skipped on
//! save; a state answered more than once keeps the last filed value.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

/// One filed answer, in the shape `GET /app/api/notations/:id/answers`
/// returns. A local mirror of `remote::NotationAnswer` so this module stays
/// pure and unit-testable without an HTTP door.
#[derive(Debug, serde::Deserialize, Serialize)]
pub struct FiledAnswer {
    pub state_name: Option<String>,
    pub value: serde_json::Value,
}

/// Serialize filed answers to the on-disk `state: value` YAML map.
///
/// # Errors
///
/// Answers with no `state_name` are skipped; a state answered more than once
/// keeps the last filed value.
pub fn answers_to_yaml(answers: &[FiledAnswer]) -> Result<String> {
    let mut map = BTreeMap::new();
    for answer in answers {
        let Some(state) = answer.state_name.as_deref().filter(|s| !s.is_empty()) else {
            continue;
        };
        let value = serde_yaml::to_value(&answer.value).map_err(|error| {
            anyhow::anyhow!("answer for `{state}` is not representable: {error}")
        })?;
        map.insert(state.to_string(), value);
    }
    serde_yaml::to_string(&map).context("serialize answers file")
}

/// Write filed answers to `path` as the `state: value` YAML map.
///
/// Returns how many states were saved.
///
/// # Errors
///
/// Serialization or file-write failures surface with the path named.
pub fn save_answers_file(path: &Path, answers: &[FiledAnswer]) -> Result<usize> {
    let yaml = answers_to_yaml(answers)?;
    std::fs::write(path, &yaml)
        .with_context(|| format!("write answers file {}", path.display()))?;
    Ok(answers
        .iter()
        .filter_map(|a| a.state_name.as_deref().filter(|s| !s.is_empty()))
        .count())
}

/// The parsed `state: value` map `--answers-from` / `--answers-file` reads.
pub type SavedAnswers = BTreeMap<String, serde_yaml::Value>;

/// Read and parse a saved answers file.
///
/// # Errors
///
/// A missing file, invalid YAML, or a document that is not a mapping fails
/// with the path and the expected shape named — a malformed file must stop
/// the prefill, never silently drop answers.
pub fn load_answers_file(path: &Path) -> Result<SavedAnswers> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read answers file {}", path.display()))?;
    let value: serde_yaml::Value = serde_yaml::from_str(&text)
        .with_context(|| format!("parse answers file {} as YAML", path.display()))?;
    let serde_yaml::Value::Mapping(map) = value else {
        anyhow::bail!(
            "answers file {} must be a `state: value` map (as `--save-answers` writes), got {}",
            path.display(),
            yaml_kind(&value),
        );
    };
    let mut saved = SavedAnswers::new();
    for (key, value) in map {
        let Some(state) = key.as_str() else {
            anyhow::bail!("answers file {} has a non-string state key; every key must be a question code like `entity__company`", path.display());
        };
        saved.insert(state.to_string(), value);
    }
    Ok(saved)
}

/// The scalar text of a saved answer, as the step route's `value` field
/// accepts. Non-scalar saved values (a `people_list` row set or a schedule
/// table) have no single form field and are refused — prefill walks scalar
/// questions; an aggregate question must be answered interactively.
///
/// # Errors
///
/// A saved value that is not a string, number, or boolean.
pub fn saved_scalar(value: &serde_yaml::Value) -> Result<String> {
    match value {
        serde_yaml::Value::String(text) => Ok(text.clone()),
        serde_yaml::Value::Number(number) => Ok(number.to_string()),
        serde_yaml::Value::Bool(flag) => Ok(flag.to_string()),
        other => Err(anyhow::anyhow!(
            "saved answer is a {}; the step route takes a scalar — answer this question interactively",
            yaml_kind(other),
        )),
    }
}

fn yaml_kind(value: &serde_yaml::Value) -> &'static str {
    match value {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "boolean",
        serde_yaml::Value::Number(_) => "number",
        serde_yaml::Value::String(_) => "string",
        serde_yaml::Value::Sequence(_) => "list",
        serde_yaml::Value::Mapping(_) | serde_yaml::Value::Tagged(_) => "map",
    }
}

#[cfg(test)]
mod tests {
    use super::{answers_to_yaml, load_answers_file, save_answers_file, saved_scalar};
    use serde_json::json;
    use std::path::Path;

    fn filed_answer(state: Option<&str>, value: serde_json::Value) -> super::FiledAnswer {
        super::FiledAnswer {
            state_name: state.map(str::to_string),
            value,
        }
    }

    #[test]
    fn answers_write_a_state_value_map_and_read_back_identical() {
        let answers = vec![
            filed_answer(Some("person__client"), json!("Libra Example")),
            filed_answer(Some("entity__company"), json!("Example Holdings LLC")),
            filed_answer(Some("usd__fee"), json!(2500)),
        ];
        let yaml = answers_to_yaml(&answers).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        let map = parsed.as_mapping().unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(
            saved_scalar(&map["person__client"]).unwrap(),
            "Libra Example"
        );
        assert_eq!(saved_scalar(&map["usd__fee"]).unwrap(), "2500");
    }

    #[test]
    fn a_state_answered_twice_keeps_the_last_filed_value() {
        let answers = vec![
            filed_answer(Some("person__client"), json!("Earlier")),
            filed_answer(Some("person__client"), json!("Later")),
        ];
        let yaml = answers_to_yaml(&answers).unwrap();
        assert!(yaml.contains("Later"));
        assert!(!yaml.contains("Earlier"));
    }

    #[test]
    fn answers_without_a_state_name_are_skipped() {
        let answers = vec![filed_answer(None, json!("orphan"))];
        let yaml = answers_to_yaml(&answers).unwrap();
        assert_eq!(yaml.trim(), "{}");
    }

    #[test]
    fn a_malformed_answers_file_names_the_path_and_the_expected_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("answers.yaml");
        std::fs::write(&path, "- just\n- a\n- list\n").unwrap();
        let error = load_answers_file(&path).unwrap_err().to_string();
        assert!(error.contains(path.display().to_string().as_str()));
        assert!(error.contains("state: value"));
    }

    #[test]
    fn an_unparseable_answers_file_names_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("answers.yaml");
        std::fs::write(&path, "state: [unclosed\n").unwrap();
        let error = load_answers_file(&path).unwrap_err().to_string();
        assert!(error.contains("parse answers file"));
    }

    #[test]
    fn a_missing_answers_file_names_the_path() {
        let error = load_answers_file(Path::new("/nonexistent/answers.yaml"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("read answers file"));
    }

    #[test]
    fn a_non_string_state_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("answers.yaml");
        std::fs::write(&path, "1: value\n").unwrap();
        let error = load_answers_file(&path).unwrap_err().to_string();
        assert!(error.contains("non-string state key"));
    }

    #[test]
    fn save_and_load_round_trip_the_same_map() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("answers.yaml");
        let answers = vec![
            filed_answer(Some("person__client"), json!("P001 Client")),
            filed_answer(Some("custom_text__notes"), json!("")),
        ];
        let saved_count = save_answers_file(&path, &answers).unwrap();
        assert_eq!(saved_count, 2);
        let loaded = load_answers_file(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(
            saved_scalar(&loaded["person__client"]).unwrap(),
            "P001 Client"
        );
        assert_eq!(saved_scalar(&loaded["custom_text__notes"]).unwrap(), "");
    }

    #[test]
    fn a_non_scalar_saved_value_is_refused_with_its_kind_named() {
        let value: serde_yaml::Value = serde_yaml::from_str("rows:\n  - one\n  - two").unwrap();
        let map = value.as_mapping().unwrap();
        let rows = map.get(serde_yaml::Value::from("rows")).unwrap();
        let error = saved_scalar(rows).unwrap_err().to_string();
        assert!(error.contains("list"));
    }
}
