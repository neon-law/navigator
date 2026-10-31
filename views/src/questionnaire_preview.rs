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
//!
//! Degradation is loud, not silent: frontmatter that *declares* a
//! questionnaire but cannot be read into its chain is a
//! [`questionnaire_preview::ParseFailure`] the caller surfaces as a
//! structured warning and a visible page panel — only a template with no
//! `questionnaire:` block previews without a demo and without an error.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;

/// Why a declared questionnaire could not be previewed.
///
/// A template that *declares* a `questionnaire:` block never degrades
/// silently: malformed YAML, a chain that does not resolve linearly, or a
/// declaration that resolves to no steps reports this failure, and the
/// preview page renders it as a visible panel. Only frontmatter with no
/// `questionnaire:` block at all previews with no demo section and no
/// failure — that is a letter, not a break.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("questionnaire failed to parse: {message}")]
pub struct ParseFailure {
    /// What could not be read, and why — carried to the page's error panel
    /// and to the structured warning the preview logs.
    pub message: String,
}

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
/// preview.
///
/// Frontmatter with no `questionnaire:` block parses to an empty list — a
/// letter previews with no demo section and no failure. A template that
/// *declares* a questionnaire never degrades to a silently empty demo:
/// malformed YAML, a chain that does not resolve linearly, or a
/// declaration that resolves to no steps is a [`ParseFailure`], which the
/// preview carries to the page's visible panel and the structured warning.
///
/// # Errors
///
/// Returns a [`ParseFailure`] when a declared questionnaire cannot be read
/// into its linear `BEGIN → … → END` chain.
pub fn parse(frontmatter: &str) -> Result<Vec<PreviewQuestion>, ParseFailure> {
    let mut value: serde_yaml::Value =
        serde_yaml::from_str(frontmatter).map_err(|error| ParseFailure {
            message: error.to_string(),
        })?;
    if value.is_null() {
        // No frontmatter at all: a body-only document has no demo, not a
        // failure.
        return Ok(Vec::new());
    }
    let declared = value.get("questionnaire").is_some();
    rules::normalize_value(&mut value).map_err(|message| ParseFailure { message })?;
    let doc = serde_yaml::from_value::<QuestionnaireFrontmatter>(value).map_err(|error| {
        ParseFailure {
            message: error.to_string(),
        }
    })?;
    if !declared {
        return Ok(Vec::new());
    }
    let groups = group_index(&doc.questionnaire_groups);

    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    let mut current = BEGIN.to_string();
    loop {
        let Some(next) = doc
            .questionnaire
            .get(&current)
            .and_then(|transitions| transitions.get(UNCONDITIONAL))
        else {
            return Err(ParseFailure {
                message: format!(
                    "the declared chain stops at `{current}`, which declares no unconditional `_` transition"
                ),
            });
        };
        if next == END {
            break;
        }
        if !visited.insert(next.clone()) {
            return Err(ParseFailure {
                message: format!(
                    "the declared chain revisits `{next}` — a cycle, where the runtime walks each state once"
                ),
            });
        }
        ordered.push(question_for_state(next, &doc, groups.get(next).cloned()));
        current = next.clone();
    }
    if ordered.is_empty() {
        return Err(ParseFailure {
            message: "a questionnaire is declared but no steps resolve from `BEGIN`".to_string(),
        });
    }
    Ok(ordered)
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
        let questions =
            parse(ONBOARDING_LETTER_FRONTMATTER).expect("the onboarding questionnaire parses");
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
        let questions =
            parse(ONBOARDING_LETTER_FRONTMATTER).expect("the onboarding questionnaire parses");
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
        let questions =
            parse(ONBOARDING_LETTER_FRONTMATTER).expect("the onboarding questionnaire parses");
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
        let questions = parse(frontmatter).expect("a signature step parses");
        assert_eq!(questions.len(), 1);
        assert_eq!(
            questions[0].prompt,
            "This step sends the document to client for signature — nothing to type here."
        );
        assert!(!questions[0].is_interactive());
    }

    #[test]
    fn frontmatter_with_no_questionnaire_block_yields_no_questions_and_no_failure() {
        let questions =
            parse("title: Just a title\n").expect("no questionnaire block is not a failure");
        assert!(questions.is_empty());
    }

    #[test]
    fn empty_frontmatter_yields_no_questions_and_no_failure() {
        let questions = parse("").expect("no frontmatter at all is not a failure");
        assert!(questions.is_empty());
    }

    #[test]
    fn malformed_yaml_reports_a_parse_failure_rather_than_an_empty_demo() {
        let failure = parse("not: [valid, yaml: at all")
            .expect_err("malformed YAML is a parse failure, not a silent demo");
        assert!(!failure.message.is_empty(), "{failure:?}");
    }

    #[test]
    fn a_declared_questionnaire_that_resolves_to_no_steps_is_a_failure() {
        let frontmatter = r"
questionnaire:
  BEGIN:
    _: END
  END: {}
";
        let failure = parse(frontmatter)
            .expect_err("a declared questionnaire resolves to steps or fails loudly");
        assert!(failure.message.contains("no steps resolve"), "{failure:?}");
    }

    #[test]
    fn a_chain_that_walks_off_the_declared_states_is_a_failure() {
        let frontmatter = r"
questionnaire:
  BEGIN:
    _: person__client
";
        let failure = parse(frontmatter).expect_err("a dangling transition is a parse failure");
        assert!(failure.message.contains("person__client"), "{failure:?}");
    }

    #[test]
    fn a_shorthand_list_uses_the_bank_prompt_and_a_role_label() {
        let questions = parse("questionnaire:\n  - custom_usd__annual_fee\n")
            .expect("the shorthand form parses");
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
        )
        .expect("the heading-and-columns form parses");
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

    /// A cycle stops the walk — it never loops — and reports the failure
    /// instead of handing back a silently truncated demo.
    #[test]
    fn a_cyclic_chain_stops_instead_of_looping_and_reports_the_failure() {
        let frontmatter = r"
questionnaire:
  BEGIN:
    _: a
  a:
    _: b
  b:
    _: a
";
        let failure = parse(frontmatter).expect_err("a cycle is a parse failure");
        assert!(failure.message.contains("cycle"), "{failure:?}");
    }
}
