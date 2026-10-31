//! Authoring forms for a questionnaire, normalized to the runtime map.
//!
//! The runtime reads one map: `BEGIN` walks to `END`, following the `_`
//! edge on a linear state and the answer's own named edge on a branch
//! state — a single-choice question whose `choices:` values name routes
//! (LAW-209). Authors may also write a linear list, put a prompt on the
//! state it labels, group states under a heading, and declare the columns of
//! a `schedule__*` state. Those forms compile into that map. A schedule answer
//! is rows of text, number, or date cells, not a document body.
//!
//! A state may also carry `completed_by: signer`: it is a Signer Box, a
//! custom primitive the signer completes by hand on the printed document.
//! The marker is lifted onto the document as `signer_boxes:`, in walk
//! order, so the renders and the rules read one list.

use std::collections::BTreeSet;

use serde::de::DeserializeOwned;
use serde_yaml::{Mapping, Value};

/// Rewrite `questionnaire:` and lift inline prompts, groups, and schedule
/// columns onto the document. A document with no questionnaire is unchanged.
///
/// # Errors
///
/// Returns a message when the authoring form cannot become one state map
/// or a schedule column is not a text, number, or date cell. Route
/// validation is not done here — the workflow gate
/// (`workflows::spec::QuestionnaireSpec::validate` plus
/// `validate_routes`) refuses unreachable questions, cycles, and
/// uncovered choices when the frontmatter is assembled.
pub fn normalize_value(doc: &mut Value) -> Result<(), String> {
    let Some(questionnaire) = doc.get("questionnaire").cloned() else {
        return Ok(());
    };
    let mut prompts = doc
        .get("prompts")
        .cloned()
        .unwrap_or(Value::Mapping(Mapping::new()));
    if !prompts.is_mapping() {
        return Err("`prompts:` must be a map of role to wording".to_string());
    }
    let mut groups = Vec::new();
    let mut columns = Mapping::new();
    let mut boxes = Vec::new();
    let states = match questionnaire {
        Value::Sequence(items) => {
            compile_sequence(&items, &mut prompts, &mut groups, &mut columns, &mut boxes)?
        }
        Value::Mapping(map) => compile_mapping(&map, &mut prompts, &mut columns, &mut boxes)?,
        _ => return Err("`questionnaire:` must be a list or a state map".to_string()),
    };
    let Some(root) = doc.as_mapping_mut() else {
        return Err("frontmatter must be a map".to_string());
    };
    root.insert(Value::String("questionnaire".to_string()), states);
    if prompts.as_mapping().is_some_and(|map| !map.is_empty()) {
        root.insert(Value::String("prompts".to_string()), prompts);
    }
    if !groups.is_empty() {
        root.insert(
            Value::String("questionnaire_groups".to_string()),
            Value::Sequence(groups),
        );
    }
    if !columns.is_empty() {
        root.insert(
            Value::String("schedule_columns".to_string()),
            Value::Mapping(columns),
        );
    }
    if !boxes.is_empty() {
        root.insert(
            Value::String(SIGNER_BOXES_KEY.to_string()),
            Value::Sequence(boxes.into_iter().map(Value::String).collect()),
        );
    }
    crate::clauses::merge_questions(doc)?;
    Ok(())
}

/// The lifted key naming every Signer Box state, in chain order.
pub const SIGNER_BOXES_KEY: &str = "signer_boxes";

/// The state attribute that declares a Signer Box, and its one value.
const COMPLETED_BY: &str = "completed_by";
const SIGNER: &str = "signer";

/// The `<type>` tokens a Signer Box may use: the primitives a person can
/// write on a line by hand. A record or reference answer is a row the
/// questionnaire creates or selects, never a blank on paper.
const SIGNER_BOX_TYPES: [&str; 4] = [
    "custom_text",
    "custom_datetime",
    "custom_usd",
    "custom_phone",
];

/// The Signer Box states a template's frontmatter declares, in chain order.
///
/// # Errors
///
/// Returns the YAML or normalization error.
pub fn signer_boxes(yaml: &str) -> Result<Vec<String>, String> {
    let mut value: Value = serde_yaml::from_str(yaml).map_err(|error| error.to_string())?;
    normalize_value(&mut value)?;
    Ok(value
        .get(SIGNER_BOXES_KEY)
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect())
}

/// Record `state` as a Signer Box when its map says `completed_by: signer`.
fn note_signer_box(state: &str, map: &Mapping, boxes: &mut Vec<String>) -> Result<(), String> {
    let Some(raw) = mapping_get(map, COMPLETED_BY) else {
        return Ok(());
    };
    if state == "BEGIN" || state == "END" {
        return Err(format!(
            "`{state}` is written by the chain and cannot be completed by anyone"
        ));
    }
    if raw.as_str() != Some(SIGNER) {
        return Err(format!(
            "`{state}` `completed_by:` must be `signer`; a state the respondent answers carries no \
             `completed_by:`"
        ));
    }
    let type_token = state.split("__").next().unwrap_or(state);
    if !SIGNER_BOX_TYPES.contains(&type_token) {
        return Err(format!(
            "`{state}` is `completed_by: signer`, but only a `custom_text`, `custom_datetime`, \
             `custom_usd`, or `custom_phone` state prints as a box the signer completes by hand"
        ));
    }
    boxes.push(state.to_string());
    Ok(())
}

/// Parse frontmatter after [`normalize_value`].
///
/// # Errors
///
/// Returns the YAML or normalization error.
pub fn deserialize_frontmatter<T: DeserializeOwned>(yaml: &str) -> Result<T, String> {
    let mut value: Value = serde_yaml::from_str(yaml).map_err(|error| error.to_string())?;
    normalize_value(&mut value)?;
    serde_yaml::from_value(value).map_err(|error| error.to_string())
}

fn compile_sequence(
    items: &[Value],
    prompts: &mut Value,
    groups: &mut Vec<Value>,
    columns: &mut Mapping,
    boxes: &mut Vec<String>,
) -> Result<Value, String> {
    if items.is_empty() {
        return Err("questionnaire list is empty".to_string());
    }
    let mut order = Vec::new();
    for item in items {
        match item {
            Value::String(state) => push_state(&mut order, state)?,
            Value::Mapping(map) if mapping_get(map, "heading").is_some() => {
                let heading = required_string(map, "heading")?;
                let questions = mapping_get(map, "questions")
                    .and_then(Value::as_sequence)
                    .ok_or_else(|| format!("heading `{heading}` needs a `questions:` list"))?;
                let mut members = Vec::new();
                for question in questions {
                    let state = compile_question(question, prompts, columns, boxes)?;
                    push_state(&mut order, &state)?;
                    members.push(Value::String(state));
                }
                if members.is_empty() {
                    return Err(format!("heading `{heading}` has no questions"));
                }
                let mut group = Mapping::new();
                group.insert(Value::String("heading".to_string()), Value::String(heading));
                group.insert(
                    Value::String("states".to_string()),
                    Value::Sequence(members),
                );
                groups.push(Value::Mapping(group));
            }
            Value::Mapping(_) => {
                let state = compile_question(item, prompts, columns, boxes)?;
                push_state(&mut order, &state)?;
            }
            _ => return Err("a questionnaire list item must be a state or a heading".to_string()),
        }
    }
    Ok(chain(&order))
}

fn compile_mapping(
    map: &Mapping,
    prompts: &mut Value,
    columns: &mut Mapping,
    boxes: &mut Vec<String>,
) -> Result<Value, String> {
    let mut out = Mapping::new();
    for (key, value) in map {
        let Some(state) = key.as_str() else {
            return Err("questionnaire state names must be strings".to_string());
        };
        let transitions = strip_authoring(state, value, prompts, columns, boxes)?;
        out.insert(key.clone(), transitions);
    }
    Ok(Value::Mapping(out))
}

fn compile_question(
    value: &Value,
    prompts: &mut Value,
    columns: &mut Mapping,
    boxes: &mut Vec<String>,
) -> Result<String, String> {
    match value {
        Value::String(state) => {
            require_schedule_columns(state, columns)?;
            Ok(state.clone())
        }
        Value::Mapping(map) => {
            let state = required_string(map, "state")?;
            note_signer_box(&state, map, boxes)?;
            if let Some(prompt) = mapping_get(map, "prompt") {
                let prompt = prompt
                    .as_str()
                    .ok_or_else(|| format!("`{state}` prompt must be a string"))?;
                set_prompt(prompts, &role_of(&state), prompt)?;
            }
            if let Some(raw) = mapping_get(map, "columns") {
                let parsed = parse_columns(&state, raw)?;
                columns.insert(Value::String(role_of(&state)), parsed);
            }
            require_schedule_columns(&state, columns)?;
            Ok(state)
        }
        _ => Err("a question must be a state name or a state map".to_string()),
    }
}

fn strip_authoring(
    state: &str,
    value: &Value,
    prompts: &mut Value,
    columns: &mut Mapping,
    boxes: &mut Vec<String>,
) -> Result<Value, String> {
    if state == "END" && (value.is_null() || value.as_mapping().is_some_and(Mapping::is_empty)) {
        return Ok(Value::Mapping(Mapping::new()));
    }
    let Some(map) = value.as_mapping() else {
        return Err(format!("questionnaire state `{state}` must be a map"));
    };
    note_signer_box(state, map, boxes)?;
    if let Some(prompt) = mapping_get(map, "prompt") {
        let prompt = prompt
            .as_str()
            .ok_or_else(|| format!("`{state}` prompt must be a string"))?;
        if state != "BEGIN" && state != "END" {
            set_prompt(prompts, &role_of(state), prompt)?;
        }
    }
    if let Some(raw) = mapping_get(map, "columns") {
        let parsed = parse_columns(state, raw)?;
        columns.insert(Value::String(role_of(state)), parsed);
    }
    require_schedule_columns(state, columns)?;
    let mut transitions = Mapping::new();
    for (key, target) in map {
        let Some(condition) = key.as_str() else {
            continue;
        };
        if matches!(
            condition,
            "prompt" | "columns" | "heading" | "questions" | "state" | COMPLETED_BY
        ) {
            continue;
        }
        let Some(target) = target.as_str() else {
            return Err(format!(
                "questionnaire transition `{state}.{condition}` must be a state"
            ));
        };
        transitions.insert(key.clone(), Value::String(target.to_string()));
    }
    Ok(Value::Mapping(transitions))
}

fn parse_columns(state: &str, value: &Value) -> Result<Value, String> {
    if !is_schedule(state) {
        return Err(format!(
            "`{state}` declares columns, but only a `schedule__*` state collects repeating rows"
        ));
    }
    let Some(items) = value.as_sequence() else {
        return Err(format!("`{state}` columns must be a list"));
    };
    if items.is_empty() {
        return Err(format!("`{state}` needs at least one column"));
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for item in items {
        let Some(map) = item.as_mapping() else {
            return Err(format!("`{state}` column must be a map"));
        };
        let name = required_string(map, "name")?;
        if !is_column_name(&name) || !seen.insert(name.clone()) {
            return Err(format!(
                "`{state}` column `{name}` must be a unique snake_case name"
            ));
        }
        let label = mapping_get(map, "label")
            .and_then(Value::as_str)
            .map_or_else(|| name.replace('_', " "), str::to_string);
        let value_type = required_string(map, "type")?;
        if !matches!(value_type.as_str(), "text" | "number" | "date") {
            return Err(format!(
                "`{state}` column `{name}` type `{value_type}` is not text, number, or date; a \
                 schedule cell is not a document body"
            ));
        }
        let mut column = Mapping::new();
        column.insert(Value::String("name".to_string()), Value::String(name));
        column.insert(Value::String("label".to_string()), Value::String(label));
        column.insert(Value::String("type".to_string()), Value::String(value_type));
        out.push(Value::Mapping(column));
    }
    Ok(Value::Sequence(out))
}

fn require_schedule_columns(state: &str, columns: &Mapping) -> Result<(), String> {
    if is_schedule(state) && !columns.contains_key(Value::String(role_of(state))) {
        return Err(format!(
            "`{state}` needs `columns:`; a schedule is repeating typed rows, not a document body"
        ));
    }
    Ok(())
}

fn is_schedule(state: &str) -> bool {
    state.split("__").next() == Some("schedule")
}

fn is_column_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn role_of(state: &str) -> String {
    state
        .split_once("__")
        .map_or(state, |(_, role)| role)
        .to_string()
}

fn set_prompt(prompts: &mut Value, role: &str, prompt: &str) -> Result<(), String> {
    let Some(map) = prompts.as_mapping_mut() else {
        return Err("`prompts:` must be a map of role to wording".to_string());
    };
    map.insert(
        Value::String(role.to_string()),
        Value::String(prompt.to_string()),
    );
    Ok(())
}

fn push_state(order: &mut Vec<String>, state: &str) -> Result<(), String> {
    if state == "BEGIN" || state == "END" {
        return Err(format!(
            "`{state}` is written by the chain, not listed as a question"
        ));
    }
    if order.iter().any(|seen| seen == state) {
        return Err(format!("duplicate questionnaire state `{state}`"));
    }
    order.push(state.to_string());
    Ok(())
}

fn chain(order: &[String]) -> Value {
    let mut map = Mapping::new();
    let mut previous = "BEGIN".to_string();
    for state in order {
        let mut edge = Mapping::new();
        edge.insert(Value::String("_".to_string()), Value::String(state.clone()));
        let next = std::mem::replace(&mut previous, state.clone());
        map.insert(Value::String(next), Value::Mapping(edge));
    }
    let mut end_edge = Mapping::new();
    end_edge.insert(
        Value::String("_".to_string()),
        Value::String("END".to_string()),
    );
    map.insert(Value::String(previous), Value::Mapping(end_edge));
    map.insert(
        Value::String("END".to_string()),
        Value::Mapping(Mapping::new()),
    );
    Value::Mapping(map)
}

fn mapping_get<'a>(map: &'a Mapping, key: &str) -> Option<&'a Value> {
    map.get(Value::String(key.to_string()))
}

fn required_string(map: &Mapping, key: &str) -> Result<String, String> {
    mapping_get(map, key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("`{key}:` must be a string"))
}

#[cfg(test)]
mod tests {
    use super::{deserialize_frontmatter, normalize_value};
    use serde::Deserialize;
    use serde_yaml::Value;
    use std::collections::BTreeMap;

    #[derive(Debug, Deserialize)]
    struct Doc {
        questionnaire: BTreeMap<String, BTreeMap<String, String>>,
        #[serde(default)]
        prompts: BTreeMap<String, String>,
    }

    fn normalized(yaml: &str) -> Value {
        let mut value: Value = serde_yaml::from_str(yaml).expect("yaml");
        normalize_value(&mut value).unwrap_or_else(|error| panic!("{error}"));
        value
    }

    #[test]
    fn a_linear_list_compiles_to_the_underscore_chain() {
        let doc: Doc = deserialize_frontmatter(
            "questionnaire:\n  - entity__company\n  - person__first_party\n",
        )
        .expect("shorthand");
        assert_eq!(doc.questionnaire["BEGIN"]["_"], "entity__company");
        assert_eq!(
            doc.questionnaire["entity__company"]["_"],
            "person__first_party"
        );
        assert_eq!(doc.questionnaire["person__first_party"]["_"], "END");
        assert!(doc.questionnaire["END"].is_empty());
    }

    #[test]
    fn an_inline_prompt_is_stored_on_the_role() {
        let doc: Doc = deserialize_frontmatter(
            "questionnaire:\n  - state: custom_datetime__effective_date\n    prompt: When does this take effect?\n",
        )
        .expect("inline prompt");
        assert_eq!(
            doc.prompts.get("effective_date").map(String::as_str),
            Some("When does this take effect?")
        );
        assert!(doc.questionnaire["custom_datetime__effective_date"].contains_key("_"));
        assert!(!doc.questionnaire["custom_datetime__effective_date"].contains_key("prompt"));
    }

    #[test]
    fn a_heading_groups_states_without_leaving_the_chain() {
        let value = normalized(
            "questionnaire:\n  - heading: The company\n    questions:\n      - entity__company\n      - address__office\n",
        );
        let doc: Doc = serde_yaml::from_value(value.clone()).expect("chain");
        assert_eq!(doc.questionnaire["entity__company"]["_"], "address__office");
        let heading = value["questionnaire_groups"][0]["heading"].as_str();
        assert_eq!(heading, Some("The company"));
        assert_eq!(
            value["questionnaire_groups"][0]["states"][1].as_str(),
            Some("address__office")
        );
    }

    #[test]
    fn a_state_map_keeps_an_inline_prompt() {
        let doc: Doc = deserialize_frontmatter(
            "prompts:\n  company: Which company?\nquestionnaire:\n  BEGIN: { _: entity__company }\n  entity__company:\n    _: END\n    prompt: Which company is this?\n  END: {}\n",
        )
        .expect("state map");
        assert_eq!(doc.questionnaire["BEGIN"]["_"], "entity__company");
        assert_eq!(doc.questionnaire["entity__company"]["_"], "END");
        assert_eq!(doc.prompts["company"], "Which company is this?");
    }

    #[test]
    fn a_schedule_compiles_to_typed_columns() {
        let value = normalized(
            "questionnaire:\n  - state: schedule__licenses\n    prompt: List each license.\n    columns:\n      - name: product\n        label: Product\n        type: text\n      - name: seats\n        label: Seats\n        type: number\n",
        );
        assert_eq!(
            value["schedule_columns"]["licenses"][0]["type"].as_str(),
            Some("text")
        );
        assert_eq!(
            value["schedule_columns"]["licenses"][1]["name"].as_str(),
            Some("seats")
        );
        let error = normalize_value(&mut serde_yaml::from_str(
            "questionnaire:\n  - state: schedule__licenses\n    columns:\n      - name: body\n        label: Body\n        type: markdown\n",
        ).expect("yaml")).expect_err("markdown column");
        assert!(error.contains("not a document body"), "{error}");
    }

    /// LAW-141: `completed_by: signer` marks a Signer Box in either authoring
    /// form; the marker leaves the chain and lands on `signer_boxes:` in chain
    /// order, where `signer_boxes` reads it.
    #[test]
    fn a_signer_box_is_lifted_out_of_the_chain_in_both_forms() {
        let list = "questionnaire:\n  - person__client\n  - state: custom_text__initials_l\n    \
                    prompt: Initial to grant (L).\n    completed_by: signer\n  - state: \
                    custom_datetime__signed_on\n    prompt: Date signed.\n    completed_by: signer\n";
        let map = "questionnaire:\n  BEGIN: { _: custom_text__initials_l }\n  \
                   custom_text__initials_l:\n    _: END\n    completed_by: signer\n  END: {}\n";
        for (yaml, expected) in [
            (
                list,
                vec!["custom_text__initials_l", "custom_datetime__signed_on"],
            ),
            (map, vec!["custom_text__initials_l"]),
        ] {
            let doc: Doc = deserialize_frontmatter(yaml).expect("signer box");
            assert!(!doc.questionnaire["custom_text__initials_l"]["_"].is_empty());
            assert!(!doc.questionnaire["custom_text__initials_l"].contains_key("completed_by"));
            assert_eq!(super::signer_boxes(yaml).expect("boxes"), expected);
        }
        assert_eq!(
            super::signer_boxes("questionnaire:\n  - person__client\n").expect("none"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_signer_box_must_be_a_hand_written_primitive_completed_by_the_signer() {
        for (yaml, needle) in [
            (
                "questionnaire:\n  - state: person__client\n    completed_by: signer\n",
                "only a `custom_text`",
            ),
            (
                "questionnaire:\n  - state: custom_text__shares\n    completed_by: client\n",
                "must be `signer`",
            ),
            (
                "questionnaire:\n  BEGIN: { _: END, completed_by: signer }\n  END: {}\n",
                "written by the chain",
            ),
        ] {
            let error = super::signer_boxes(yaml).expect_err(yaml);
            assert!(error.contains(needle), "{yaml}: {error}");
        }
    }

    #[test]
    fn a_duplicate_state_is_rejected() {
        let mut value: Value =
            serde_yaml::from_str("questionnaire:\n  - entity__company\n  - entity__company\n")
                .expect("yaml");
        let error = normalize_value(&mut value).expect_err("duplicate");
        assert!(error.contains("duplicate"), "{error}");
    }

    /// LAW-209: a state-map questionnaire may branch on a choice answer —
    /// the named edges survive normalization beside the `_` fallback, so
    /// the route gate and the walker read one map.
    #[test]
    fn a_branch_state_keeps_its_named_routes_alongside_the_fallback() {
        let doc: Doc = deserialize_frontmatter(
            "questionnaire:\n  BEGIN:\n    _: custom_single_choice__path\n  \
             custom_single_choice__path:\n    option_a: custom_datetime__a_start\n    \
             option_b: person__b_counterparty\n    _: END\n  \
             custom_datetime__a_start:\n    _: END\n  \
             person__b_counterparty:\n    _: END\n  END: {}\n",
        )
        .expect("branched state map");
        assert_eq!(
            doc.questionnaire["BEGIN"]["_"],
            "custom_single_choice__path"
        );
        assert_eq!(
            doc.questionnaire["custom_single_choice__path"]["option_a"],
            "custom_datetime__a_start"
        );
        assert_eq!(
            doc.questionnaire["custom_single_choice__path"]["option_b"],
            "person__b_counterparty"
        );
        assert_eq!(doc.questionnaire["custom_single_choice__path"]["_"], "END");
        assert_eq!(doc.questionnaire["custom_datetime__a_start"]["_"], "END");
    }
}
