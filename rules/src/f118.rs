//! `N118` — every answer in a questionnaire routes, and every route
//! reaches `END`.
//!
//! The walker advances a questionnaire on the answer's own route
//! (LAW-209): a single-choice question state may name one edge per
//! declared `choices:` value, with an optional `_` fallback for answers
//! that declare no route; every other state routes by `_` alone. A
//! named route no answer can take, a declared choice with neither route
//! nor fallback, a question no route reaches, or a walk that stops or
//! cycles before `END` strands the respondent or lies about the step
//! total. This is the authoring-time / LSP mirror of
//! [`workflows::QuestionnaireSpec::validate`] and
//! [`workflows::QuestionnaireSpec::validate_routes`] — the same
//! invariants the parse gate enforces, hoisted to a red squiggle in the
//! editor. The
//! `route_classification_matches_questionnaire_spec_validation` drift
//! test below locks the two definitions together (`workflows` stays a
//! dev-dependency so the lint engine's build graph stays lean).
//!
//! Files without frontmatter or without a `questionnaire:` key are
//! silently skipped, like the other questionnaire-shape rules. Base
//! shape errors (missing `BEGIN`/`END`, dangling targets) are not
//! reported here — only the route findings are N118's to own.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::{frontmatter, line_byte_range, Rule, SourceFile, Violation};

pub struct F118QuestionnaireRoutes;

impl F118QuestionnaireRoutes {
    pub const CODE: &'static str = "N118";
}

#[derive(Debug, Deserialize)]
struct FrontmatterShape {
    #[serde(default)]
    questionnaire: Option<BTreeMap<String, BTreeMap<String, String>>>,
    #[serde(default)]
    choices: Option<BTreeMap<String, BTreeMap<String, String>>>,
}

impl Rule for F118QuestionnaireRoutes {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn description(&self) -> &'static str {
        crate::description_for_code(Self::CODE)
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let Some(fm) = frontmatter::extract(&file.contents) else {
            return Vec::new();
        };
        let mut value: serde_yaml::Value = match serde_yaml::from_str(fm) {
            Ok(value) => value,
            Err(_) => return Vec::new(),
        };
        if let Err(message) = crate::questionnaire::normalize_value(&mut value) {
            return vec![Violation {
                code: Self::CODE,
                path: file.path.clone(),
                line: 1,
                range: line_byte_range(&file.contents, 1),
                message,
            }];
        }
        let Ok(parsed) = serde_yaml::from_value::<FrontmatterShape>(value) else {
            return Vec::new();
        };
        let Some(questionnaire) = parsed.questionnaire else {
            return Vec::new();
        };
        let choices = parsed.choices.unwrap_or_default();

        // One violation per file: the first break in the walk usually
        // cascades (everything after a broken link looks off-route too).
        let Some((state, message)) = route_finding(&questionnaire, &choices) else {
            return Vec::new();
        };
        let line = questionnaire_state_line(&file.contents, &state);
        vec![Violation {
            code: Self::CODE,
            path: file.path.clone(),
            line,
            range: line_byte_range(&file.contents, line),
            message,
        }]
    }
}

/// The first route break in `questionnaire`, as
/// `(offending state, message)` — or `None` when every answer routes,
/// every route reaches `END`, **or** the block has a base-shape problem
/// (missing `BEGIN`/`END`, dangling target) that the parse gate owns
/// instead of this rule.
///
/// Mirrors `workflows::QuestionnaireSpec::validate` and
/// `validate_routes` exactly; the drift test pins the classification to
/// them.
fn route_finding(
    questionnaire: &BTreeMap<String, BTreeMap<String, String>>,
    choices: &BTreeMap<String, BTreeMap<String, String>>,
) -> Option<(String, String)> {
    if !questionnaire.contains_key("BEGIN") || !questionnaire.contains_key("END") {
        return None;
    }
    for transitions in questionnaire.values() {
        if transitions
            .values()
            .any(|to| !questionnaire.contains_key(to))
        {
            return None;
        }
    }

    // 1. A named condition is a branch route: only a state whose
    //    answer is one declared choice value may carry one.
    for (state, transitions) in questionnaire {
        for condition in transitions.keys() {
            if condition != "_" && !routes_by_answer(state) {
                return Some((
                    state.clone(),
                    format!(
                        "questionnaire state `{state}` has condition `{condition}` — only a \
                         single-choice question state can branch on its answer, and BEGIN and \
                         END hold no answer at all"
                    ),
                ));
            }
        }
    }

    // 2. Walk every route out of BEGIN (below). A cycle or a dead end
    //    strands the respondent; converging diamonds are ordinary.
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut on_path: BTreeSet<&str> = BTreeSet::new();
    if let Err(broken) = walk_routes(questionnaire, "BEGIN", &mut on_path, &mut visited) {
        return Some((broken.to_string(), walk_broken_message(broken)));
    }
    for state in questionnaire.keys() {
        if state != "BEGIN" && state != "END" && !visited.contains(state.as_str()) {
            return Some((
                state.clone(),
                format!(
                    "questionnaire state `{state}` is unreachable from BEGIN — no route reaches \
                     it, so the walker would never ask it and the rendered step total would lie"
                ),
            ));
        }
    }

    // 3. Route coverage where `choices:` are declared: every named
    //    route is an answer a respondent can give, and every declared
    //    choice reaches a route or shares the `_` fallback.
    for (state, transitions) in questionnaire {
        let named: Vec<&String> = transitions.keys().filter(|c| *c != "_").collect();
        if named.is_empty() {
            continue;
        }
        let declared = declared_choice_values(choices, state);
        for condition in named {
            if !declared.iter().any(|value| value == condition) {
                return Some((
                    state.clone(),
                    format!(
                        "questionnaire state `{state}` routes on `{condition}`, which is not one \
                         of the state's declared `choices:` values — the walker could never take \
                         that route"
                    ),
                ));
            }
        }
        if transitions.contains_key("_") {
            continue;
        }
        for choice in &declared {
            if !transitions.contains_key(choice) {
                return Some((
                    state.clone(),
                    format!(
                        "questionnaire state `{state}` declares choice `{choice}` but routes it \
                         nowhere — give `{choice}` its own route or the state a `_` fallback, or \
                         the walker strands a respondent who picks it"
                    ),
                ));
            }
        }
    }
    None
}

/// The question states a branch route may sit on — the single-choice
/// primitives. A multiple-choice answer selects a *set* and a free-text
/// answer carries no declared set, so neither names an edge. Mirrors
/// the question-registry type gate in `workflows::spec`
/// (`routes_by_answer` there); the drift test pins the two together.
fn routes_by_answer(state: &str) -> bool {
    let ty = state.split("__").next().unwrap_or(state);
    matches!(ty, "custom_single_choice" | "custom_yes_no")
}

/// The declared choice VALUES for a question `state` — keyed by the
/// state's role suffix (`custom_single_choice__path` → `path`), the
/// only metadata key a routable state resolves through, so this local
/// mirror agrees with `workflows::spec`'s alias-aware resolution for
/// every state the route checks apply to.
fn declared_choice_values(
    choices: &BTreeMap<String, BTreeMap<String, String>>,
    state: &str,
) -> Vec<String> {
    let role = state
        .split_once("__")
        .map_or(state, |(_, role)| role)
        .to_string();
    choices
        .get(&role)
        .map(|entries| entries.keys().cloned().collect())
        .unwrap_or_default()
}

/// Depth-first route walk out of `state`: the first cycle (a route
/// back into a state still on the current path) or dead end (a
/// reachable state with no outgoing route other than END). A route
/// into a state that was fully explored on an earlier route is a
/// converging diamond — END is the common join — and is not a cycle.
/// Mirrors `workflows::spec::walk_routes` exactly; the drift test pins
/// the two together.
fn walk_routes<'a>(
    questionnaire: &'a BTreeMap<String, BTreeMap<String, String>>,
    state: &'a str,
    on_path: &mut BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
) -> Result<&'a str, &'a str> {
    if visited.contains(&state) {
        return Ok(state);
    }
    if !on_path.insert(state) {
        return Err(state);
    }
    if let Some(transitions) = questionnaire.get(state) {
        if transitions.is_empty() && state != "END" {
            return Err(state);
        }
        for next in transitions.values() {
            walk_routes(questionnaire, next, on_path, visited)?;
        }
    }
    on_path.remove(&state);
    visited.insert(state);
    Ok(state)
}

fn walk_broken_message(state: &str) -> String {
    format!(
        "questionnaire walk from BEGIN stops or cycles at `{state}` before reaching END — \
         the walker would strand the respondent there"
    )
}

/// 1-based line of the indented questionnaire state key `state`, so the
/// squiggle lands on the offending state rather than the frontmatter
/// delimiter. Matches both the block form (`state:`) and the inline
/// form (`state: {}` — how a dead-end state is usually written, and the
/// dead end is exactly what the walk-broken finding flags). Falls back
/// to line 1 when the key can't be located.
fn questionnaire_state_line(contents: &str, state: &str) -> usize {
    let key = format!("{state}:");
    for (idx, raw) in contents.lines().enumerate() {
        let trimmed = raw.trim_start();
        if trimmed.len() < raw.len() && (trimmed == key || trimmed.starts_with(&format!("{key} ")))
        {
            return idx + 1;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::F118QuestionnaireRoutes;
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    fn file(body: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.md"),
            contents: body.to_string(),
        }
    }

    #[test]
    fn a_linear_chain_passes() {
        let body = "---\nquestionnaire:\n  BEGIN:\n    _: person__client\n  person__client:\n    _: project__engagement\n  project__engagement:\n    _: END\n  END: {}\n---\n";
        assert!(F118QuestionnaireRoutes.lint(&file(body)).is_empty());
    }

    #[test]
    fn a_named_condition_on_a_state_that_cannot_route_is_flagged_on_its_state_line() {
        // LAW-209: named conditions are branch routes now — this state
        // is a person question, so `skip` can never be an answer's
        // route and the squiggle lands on the offending state.
        let body = "---\nquestionnaire:\n  BEGIN:\n    _: person__client\n  person__client:\n    skip: END\n    _: END\n  END: {}\n---\n";
        let v = F118QuestionnaireRoutes.lint(&file(body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("skip"), "{}", v[0].message);
        assert_eq!(v[0].line, 5, "{v:?}");
    }

    #[test]
    fn a_question_no_route_reaches_is_flagged() {
        let body = "---\nquestionnaire:\n  BEGIN:\n    _: person__client\n  person__client:\n    _: END\n  entity__company:\n    _: END\n  END: {}\n---\n";
        let v = F118QuestionnaireRoutes.lint(&file(body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(
            v[0].message.contains("entity__company")
                && v[0].message.contains("unreachable from BEGIN"),
            "{}",
            v[0].message
        );
        assert_eq!(v[0].line, 7, "{v:?}");
    }

    #[test]
    fn a_branched_questionnaire_with_covered_choices_passes() {
        // LAW-209: the branched shape itself is legal — both choices
        // name their own route and every route reaches END.
        let body = "---\nchoices:\n  path:\n    litigation: Litigation\n    transactional: Transactional\nquestionnaire:\n  BEGIN:\n    _: custom_single_choice__path\n  custom_single_choice__path:\n    litigation: jurisdiction__venue\n    transactional: custom_datetime__closing_date\n  jurisdiction__venue:\n    _: END\n  custom_datetime__closing_date:\n    _: END\n  END: {}\n---\n";
        assert!(F118QuestionnaireRoutes.lint(&file(body)).is_empty());
    }

    #[test]
    fn a_choice_with_no_route_and_no_fallback_is_flagged() {
        // `mediation` is declared but routes nowhere and the state has
        // no `_` fallback — the compile-time answer dead-end.
        let body = "---\nchoices:\n  path:\n    litigation: Litigation\n    transactional: Transactional\n    mediation: Mediation\nquestionnaire:\n  BEGIN:\n    _: custom_single_choice__path\n  custom_single_choice__path:\n    litigation: jurisdiction__venue\n    transactional: custom_datetime__closing_date\n  jurisdiction__venue:\n    _: END\n  custom_datetime__closing_date:\n    _: END\n  END: {}\n---\n";
        let v = F118QuestionnaireRoutes.lint(&file(body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(
            v[0].message.contains("mediation") && v[0].message.contains("routes it nowhere"),
            "{}",
            v[0].message
        );
    }

    #[test]
    fn a_named_route_no_answer_can_take_is_flagged() {
        let body = "---\nchoices:\n  path:\n    litigation: Litigation\n    transactional: Transactional\nquestionnaire:\n  BEGIN:\n    _: custom_single_choice__path\n  custom_single_choice__path:\n    litigation: jurisdiction__venue\n    transactional: custom_datetime__closing_date\n    mediation: jurisdiction__venue\n  jurisdiction__venue:\n    _: END\n  custom_datetime__closing_date:\n    _: END\n  END: {}\n---\n";
        let v = F118QuestionnaireRoutes.lint(&file(body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(
            v[0].message.contains("mediation")
                && v[0].message.contains("could never take that route"),
            "{}",
            v[0].message
        );
    }

    #[test]
    fn a_fallback_covers_the_choices_that_name_no_route() {
        let body = "---\nchoices:\n  path:\n    litigation: Litigation\n    transactional: Transactional\nquestionnaire:\n  BEGIN:\n    _: custom_single_choice__path\n  custom_single_choice__path:\n    litigation: jurisdiction__venue\n    _: custom_datetime__closing_date\n  jurisdiction__venue:\n    _: END\n  custom_datetime__closing_date:\n    _: END\n  END: {}\n---\n";
        assert!(F118QuestionnaireRoutes.lint(&file(body)).is_empty());
    }

    #[test]
    fn a_walk_that_cycles_before_end_is_flagged() {
        let body = "---\nquestionnaire:\n  BEGIN:\n    _: person__client\n  person__client:\n    _: person__spouse\n  person__spouse:\n    _: person__client\n  END: {}\n---\n";
        let v = F118QuestionnaireRoutes.lint(&file(body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("stops or cycles"), "{}", v[0].message);
    }

    #[test]
    fn a_dead_end_state_is_flagged_on_its_inline_entry_line() {
        // A dead end is usually written inline (`state: {}`), which the
        // line lookup must still locate — the squiggle belongs on the
        // stranding state, not the frontmatter delimiter.
        let body = "---\nquestionnaire:\n  BEGIN:\n    _: person__client\n  person__client: {}\n  END: {}\n---\n";
        let v = F118QuestionnaireRoutes.lint(&file(body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("stops or cycles"), "{}", v[0].message);
        assert_eq!(v[0].line, 5, "{v:?}");
    }

    #[test]
    fn base_shape_failures_are_not_this_rules_findings() {
        // Missing END and dangling targets are parse-gate failures, not
        // route ones.
        let missing_end =
            "---\nquestionnaire:\n  BEGIN:\n    _: person__client\n  person__client: {}\n---\n";
        assert!(F118QuestionnaireRoutes.lint(&file(missing_end)).is_empty());
        let dangling = "---\nquestionnaire:\n  BEGIN:\n    _: person__missing\n  END: {}\n---\n";
        assert!(F118QuestionnaireRoutes.lint(&file(dangling)).is_empty());
    }

    #[test]
    fn no_frontmatter_or_questionnaire_means_no_violation() {
        assert!(F118QuestionnaireRoutes.lint(&file("just body")).is_empty());
        assert!(F118QuestionnaireRoutes
            .lint(&file("---\ntitle: T\n---\nbody\n"))
            .is_empty());
    }

    #[test]
    fn is_error_severity() {
        use crate::{severity_for_code, Severity};
        assert_eq!(severity_for_code("N118"), Severity::Error);
    }

    /// Drift lock: N118's route classification must stay identical to
    /// the parse gate's (`workflows::QuestionnaireSpec::validate` and
    /// `validate_routes`). For every corpus entry: rule finding ⇔ a
    /// gate error; base-shape parse errors and valid specs both stay
    /// unflagged.
    #[test]
    fn route_classification_matches_questionnaire_spec_validation() {
        use workflows::{QuestionnaireSpec, WorkflowSpecError};

        /// `(questionnaire yaml, choices yaml)` — the choices block is
        /// the same top-level `choices:` map the template carries.
        const CORPUS: &[(&str, Option<&str>)] = &[
            // linear (valid)
            (
                "BEGIN:\n  _: a\na:\n  _: END\nEND: {}\n",
                None,
            ),
            // empty-but-linear (valid)
            ("BEGIN:\n  _: END\nEND: {}\n", None),
            // named condition on a state that cannot route
            (
                "BEGIN:\n  _: a\na:\n  skip: END\n  _: END\nEND: {}\n",
                None,
            ),
            // unreachable question
            (
                "BEGIN:\n  _: a\na:\n  _: END\norphan:\n  _: END\nEND: {}\n",
                None,
            ),
            // dead end before END
            ("BEGIN:\n  _: a\na: {}\nEND: {}\n", None),
            // cycle before END
            (
                "BEGIN:\n  _: a\na:\n  _: b\nb:\n  _: a\nEND: {}\n",
                None,
            ),
            // converging diamond — both branches rejoin one follow-up
            // (valid: a revisit after full exploration is not a cycle)
            (
                "BEGIN:\n  _: custom_single_choice__p\ncustom_single_choice__p:\n  litigation: \
                 custom_datetime__close\n  transactional: custom_datetime__close\n\
                 custom_datetime__close:\n  _: END\nEND: {}\n",
                Some("path:\n  litigation: Litigation\n  transactional: Transactional\n"),
            ),
            // branched and covered (valid)
            (
                "BEGIN:\n  _: custom_single_choice__path\ncustom_single_choice__path:\n  \
                 litigation: jurisdiction__venue\n  transactional: custom_datetime__closing_date\n\
                 jurisdiction__venue:\n  _: END\ncustom_datetime__closing_date:\n  _: END\nEND: {}\n",
                Some("path:\n  litigation: Litigation\n  transactional: Transactional\n"),
            ),
            // branched with the `_` fallback covering the rest (valid)
            (
                "BEGIN:\n  _: custom_single_choice__path\ncustom_single_choice__path:\n  \
                 litigation: jurisdiction__venue\n  _: custom_datetime__closing_date\n\
                 jurisdiction__venue:\n  _: END\ncustom_datetime__closing_date:\n  _: END\nEND: {}\n",
                Some("path:\n  litigation: Litigation\n  transactional: Transactional\n"),
            ),
            // declared choice routed nowhere (the answer dead-end)
            (
                "BEGIN:\n  _: custom_single_choice__path\ncustom_single_choice__path:\n  \
                 litigation: jurisdiction__venue\n  transactional: custom_datetime__closing_date\n\
                 jurisdiction__venue:\n  _: END\ncustom_datetime__closing_date:\n  _: END\nEND: {}\n",
                Some("path:\n  litigation: Litigation\n  transactional: Transactional\n  mediation: Mediation\n"),
            ),
            // named route no answer can take
            (
                "BEGIN:\n  _: custom_single_choice__path\ncustom_single_choice__path:\n  \
                 litigation: jurisdiction__venue\n  transactional: custom_datetime__closing_date\n  \
                 mediation: jurisdiction__venue\njurisdiction__venue:\n  _: END\n\
                 custom_datetime__closing_date:\n  _: END\nEND: {}\n",
                Some("path:\n  litigation: Litigation\n  transactional: Transactional\n"),
            ),
            // base-shape: missing END (not N118's finding)
            ("BEGIN:\n  _: a\na: {}\n", None),
            // base-shape: dangling target (not N118's finding)
            ("BEGIN:\n  _: ghost\nEND: {}\n", None),
        ];

        for (yaml, choices) in CORPUS {
            let raw: std::collections::BTreeMap<
                String,
                std::collections::BTreeMap<String, String>,
            > = serde_yaml::from_str(yaml).expect("corpus yaml parses");
            let parsed_choices: std::collections::BTreeMap<
                String,
                std::collections::BTreeMap<String, String>,
            > = choices
                .map(|c| serde_yaml::from_str(c).expect("corpus choices parse"))
                .unwrap_or_default();
            let rule_flags = super::route_finding(&raw, &parsed_choices).is_some();
            // Only the route variants count — base-shape errors
            // (missing END, dangling) belong to the parse gate, not
            // N118; the choice-coverage half needs the choices map.
            let gate_flags = match QuestionnaireSpec::from_yaml(yaml) {
                Err(error) => matches!(
                    error,
                    WorkflowSpecError::QuestionnaireCondition { .. }
                        | WorkflowSpecError::QuestionnaireUnreachable { .. }
                        | WorkflowSpecError::QuestionnaireChainBroken { .. }
                ),
                Ok(spec) => spec.validate_routes(&parsed_choices).is_err(),
            };
            assert_eq!(
                rule_flags, gate_flags,
                "N118 drifted from QuestionnaireSpec::validate for:\n{yaml}"
            );
        }
    }
}
