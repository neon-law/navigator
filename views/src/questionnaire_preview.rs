//! Parse a template's declared questionnaire into an ordered preview list —
//! for the public "try answering this" demo on `/notations/{slug}`
//! (ENG-452), with no live Notation and no domain-crate dependency.
//!
//! The real questionnaire runtime lives in `workflows::notation_session`,
//! which `store` and `cloud` back it against a live Notation row. `neon` (and
//! every other brand crate) is forbidden from depending on `workflows` or
//! `store` at all — see `cli/tests/brand_crate_dependencies.rs` — so the
//! public preview page, which resolves entirely from `include_str!`'d
//! markdown with no database in the loop, needs its own small, pure reader of
//! the same `questionnaire:`/`prompts:`/`choices:` frontmatter shape. This
//! module owns exactly that: walk the linear `BEGIN → … → END` chain (already
//! guaranteed by the `N118` rule at authoring time — this reader stops rather
//! than loops on anything else) and pair each `custom_*` state with its own
//! declared prompt and choices.
//!
//! Person, Entity, Project, and Address questions collect display values in
//! the public preview without creating records. Signature and other service
//! steps remain explanatory; a preview cannot execute those operations.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;

/// One column of a schedule row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreviewColumn {
    /// Snake-case cell name, the key a row stores.
    pub name: String,
    /// The label shown beside the cell.
    pub label: String,
    /// `text`, `number`, or `date`.
    pub value_type: String,
}

/// One step of the demo, in declared order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreviewQuestion {
    /// The full state name, e.g. `custom_single_choice__governing_law`.
    pub code: String,
    /// The state's `<type>` prefix, e.g. `custom_single_choice` — or the
    /// whole code when it carries no `__role` suffix (`entity`).
    pub answer_type: String,
    /// Title-case role from the state name (`first_party` → `First party`).
    pub role_label: String,
    /// Heading of the questionnaire group this state sits in, when one was declared.
    pub group: String,
    /// The real prompt for an interactive state; a mechanically-built
    /// explanation for every record or reference state.
    pub prompt: String,
    /// Extra guidance. A schedule tells the reader that each cell is text, a number, or a date.
    pub help: String,
    /// `(value, label)` options — populated only for `custom_single_choice`.
    pub choices: Vec<(String, String)>,
    /// Typed cells for a `schedule` state.
    pub columns: Vec<PreviewColumn>,
}

impl PreviewQuestion {
    /// Whether this step can collect a preview value. Service steps remain
    /// explanatory; a schedule collects repeating rows.
    #[must_use]
    pub fn is_interactive(&self) -> bool {
        self.answer_type.starts_with("custom_")
            || matches!(
                self.answer_type.as_str(),
                "schedule" | "person" | "entity" | "project" | "address"
            )
    }
}

#[derive(Debug, Default, Deserialize)]
struct QuestionnaireFrontmatter {
    #[serde(default)]
    questionnaire: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    prompts: BTreeMap<String, String>,
    #[serde(default)]
    choices: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    questionnaire_groups: Vec<PreviewGroup>,
    #[serde(default)]
    schedule_columns: BTreeMap<String, Vec<PreviewColumnRaw>>,
}

#[derive(Debug, Default, Deserialize)]
struct PreviewGroup {
    heading: String,
    #[serde(default)]
    states: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PreviewColumnRaw {
    name: String,
    label: String,
    #[serde(rename = "type")]
    value_type: String,
}

const BEGIN: &str = "BEGIN";
const END: &str = "END";
const UNCONDITIONAL: &str = "_";

/// Parse a template's YAML frontmatter into its ordered questionnaire
/// preview. Returns an empty list for frontmatter with no `questionnaire:`
/// block, malformed YAML, or a chain that doesn't resolve linearly — a bundled
/// public page degrades to "no demo" rather than panicking on a shape it
/// doesn't recognize.
#[must_use]
pub fn parse(frontmatter: &str) -> Vec<PreviewQuestion> {
    let mut value: serde_yaml::Value = match serde_yaml::from_str(frontmatter) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    if rules::normalize_value(&mut value).is_err() {
        return Vec::new();
    }
    let Ok(doc) = serde_yaml::from_value::<QuestionnaireFrontmatter>(value) else {
        return Vec::new();
    };
    let groups = group_index(&doc.questionnaire_groups);

    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    let mut current = BEGIN.to_string();
    while let Some(next) = doc
        .questionnaire
        .get(&current)
        .and_then(|transitions| transitions.get(UNCONDITIONAL))
    {
        if next == END || !visited.insert(next.clone()) {
            break;
        }
        ordered.push(question_for_state(next, &doc, groups.get(next).cloned()));
        current = next.clone();
    }
    ordered
}

fn group_index(groups: &[PreviewGroup]) -> BTreeMap<String, String> {
    let mut index = BTreeMap::new();
    for group in groups {
        for state in &group.states {
            index.insert(state.clone(), group.heading.clone());
        }
    }
    index
}

/// Build one [`PreviewQuestion`] for `state`, pairing a `custom_*` state with
/// its declared prompt/choices and giving every other state a plain
/// explanation of what it does.
fn question_for_state(
    state: &str,
    doc: &QuestionnaireFrontmatter,
    group: Option<String>,
) -> PreviewQuestion {
    let (answer_type, role) = state
        .split_once("__")
        .map_or((state, state), |(prefix, role)| (prefix, role));
    let readable_role = role.replace('_', " ");
    let role_label = role_label(&readable_role);
    let interactive = answer_type.starts_with("custom_")
        || matches!(
            answer_type,
            "schedule" | "person" | "entity" | "project" | "address"
        );
    let prompt = if interactive {
        doc.prompts
            .get(role)
            .cloned()
            .or_else(|| rules::bank_prompt(answer_type).map(str::to_string))
            .map_or_else(
                || default_prompt(answer_type, &readable_role),
                |prompt| localize(&prompt, &role_label),
            )
    } else {
        default_prompt(answer_type, &readable_role)
    };
    let help = if answer_type == "schedule" {
        "Add one row per item. Each cell is text, a number, or a date.".to_string()
    } else if matches!(answer_type, "person" | "entity" | "project" | "address") {
        "Preview value only; this does not create a record.".to_string()
    } else {
        String::new()
    };
    let choices = doc.choices.get(role).map_or_else(Vec::new, |options| {
        options
            .iter()
            .map(|(value, label)| (value.clone(), label.clone()))
            .collect()
    });
    let columns = doc
        .schedule_columns
        .get(role)
        .map(|columns| {
            columns
                .iter()
                .map(|column| PreviewColumn {
                    name: column.name.clone(),
                    label: column.label.clone(),
                    value_type: column.value_type.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    PreviewQuestion {
        code: state.to_string(),
        answer_type: answer_type.to_string(),
        role_label,
        group: group.unwrap_or_default(),
        prompt,
        help,
        choices,
        columns,
    }
}

fn role_label(readable_role: &str) -> String {
    let mut chars = readable_role.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn localize(prompt: &str, role_label: &str) -> String {
    prompt
        .replace("{{for_label}}", role_label)
        .replace("{{label}}", role_label)
        .replace("{label}", role_label)
}

/// An explanation for a record/reference state, which answers by creating or
/// linking a database row rather than by typing a value.
fn default_prompt(answer_type: &str, readable_role: &str) -> String {
    match answer_type {
        "person" | "entity" | "project" => format!("What is the name of the {readable_role}?"),
        "address" => format!("What is the address of the {readable_role}?"),
        "signature" | "notarization" => format!(
            "This step sends the document to {readable_role} for {answer_type} — nothing to type here."
        ),
        _ => format!("This step records the {readable_role}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONBOARDING_LETTER_FRONTMATTER: &str = r"
prompts:
  engagement_scope: In a sentence or two, what is the minimum scope of this engagement.
  engagement_start_date: When does this engagement begin?
  governing_law: Which state's law governs this engagement?
choices:
  governing_law:
    nevada: Nevada
    california: California
    washington: Washington
questionnaire:
  BEGIN:
    _: entity
  entity:
    _: address__principal_office
  address__principal_office:
    _: person__client
  person__client:
    _: person__lawyer_dri
  person__lawyer_dri:
    _: project__engagement
  project__engagement:
    _: custom_datetime__engagement_start_date
  custom_datetime__engagement_start_date:
    _: custom_text__engagement_scope
  custom_text__engagement_scope:
    _: custom_single_choice__governing_law
  custom_single_choice__governing_law:
    _: END
  END: {}
";

    #[test]
    fn walks_the_linear_chain_in_declared_order() {
        let questions = parse(ONBOARDING_LETTER_FRONTMATTER);
        let codes: Vec<&str> = questions.iter().map(|q| q.code.as_str()).collect();
        assert_eq!(
            codes,
            vec![
                "entity",
                "address__principal_office",
                "person__client",
                "person__lawyer_dri",
                "project__engagement",
                "custom_datetime__engagement_start_date",
                "custom_text__engagement_scope",
                "custom_single_choice__governing_law",
            ]
        );
    }

    #[test]
    fn a_custom_state_carries_its_own_real_prompt_and_choices() {
        let questions = parse(ONBOARDING_LETTER_FRONTMATTER);
        let governing_law = questions
            .iter()
            .find(|q| q.code == "custom_single_choice__governing_law")
            .expect("the governing-law step is present");
        assert_eq!(governing_law.answer_type, "custom_single_choice");
        assert_eq!(
            governing_law.prompt,
            "Which state's law governs this engagement?"
        );
        assert_eq!(
            governing_law.choices,
            vec![
                ("california".to_string(), "California".to_string()),
                ("nevada".to_string(), "Nevada".to_string()),
                ("washington".to_string(), "Washington".to_string()),
            ]
        );
        assert!(governing_law.is_interactive());
    }

    #[test]
    fn a_record_state_collects_a_preview_name_without_creating_records() {
        let questions = parse(ONBOARDING_LETTER_FRONTMATTER);
        let client = questions
            .iter()
            .find(|q| q.code == "person__client")
            .expect("the client step is present");
        assert_eq!(client.answer_type, "person");
        assert_eq!(client.prompt, "Who is Client?");
        assert!(client.choices.is_empty());
        assert!(client.is_interactive());
    }

    #[test]
    fn a_signature_state_explains_the_hand_off_instead_of_asking_for_ink() {
        let frontmatter = r"
questionnaire:
  BEGIN:
    _: signature__client
  signature__client:
    _: END
  END: {}
";
        let questions = parse(frontmatter);
        assert_eq!(questions.len(), 1);
        assert_eq!(
            questions[0].prompt,
            "This step sends the document to client for signature — nothing to type here."
        );
        assert!(!questions[0].is_interactive());
    }

    #[test]
    fn frontmatter_with_no_questionnaire_block_yields_no_questions() {
        assert!(parse("title: Just a title\n").is_empty());
    }

    #[test]
    fn malformed_yaml_yields_no_questions_rather_than_panicking() {
        assert!(parse("not: [valid, yaml: at all").is_empty());
    }

    #[test]
    fn a_shorthand_list_uses_the_bank_prompt_and_a_role_label() {
        let questions = parse("questionnaire:\n  - custom_usd__annual_fee\n");
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].role_label, "Annual fee");
        assert_eq!(
            questions[0].prompt,
            "What dollar amount applies to Annual fee?"
        );
        assert!(questions[0].is_interactive());
    }

    #[test]
    fn a_heading_and_schedule_columns_reach_the_preview() {
        let questions = parse(
            "questionnaire:\n  - heading: Licenses\n    questions:\n      - state: schedule__licenses\n        prompt: List each license.\n        columns:\n          - name: product\n            label: Product\n            type: text\n          - name: seats\n            type: number\n",
        );
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].group, "Licenses");
        assert_eq!(questions[0].prompt, "List each license.");
        assert_eq!(questions[0].columns.len(), 2);
        assert_eq!(questions[0].columns[1].value_type, "number");
        assert_eq!(questions[0].columns[1].label, "seats");
        assert_eq!(
            questions[0].help,
            "Add one row per item. Each cell is text, a number, or a date."
        );
        assert!(questions[0].is_interactive());
    }

    #[test]
    fn a_cyclic_chain_stops_instead_of_looping_forever() {
        let frontmatter = r"
questionnaire:
  BEGIN:
    _: a
  a:
    _: b
  b:
    _: a
";
        let questions = parse(frontmatter);
        assert_eq!(
            questions.len(),
            2,
            "stops once a state repeats: {questions:?}"
        );
    }
}
