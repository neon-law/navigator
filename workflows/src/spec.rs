//! Parsed workflow spec — the shape that lives in a template's
//! YAML frontmatter under `workflow:`.
//!
//! Wire shape:
//!
//! ```yaml
//! workflow:
//!   BEGIN:
//!     created: lawyer_review__for_grantor
//!   lawyer_review__for_grantor:
//!     approve: notarization__for_grantor
//!     reject:  END
//!   notarization__for_grantor:
//!     signed:  mailroom_send__to_signer
//!     refused: END
//!   mailroom_send__to_signer:
//!     sent:    mailroom_receive__signed_copy
//!   mailroom_receive__signed_copy:
//!     received: END
//!   END: {}
//! ```
//!
//! State names use `<prefix>__<discriminator>` form; the prefix
//! selects the [`crate::StepKind`] (system / lawyer_review /
//! notarization / mailroom_send / mailroom_receive).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A workflow state name, e.g., `lawyer_review__for_trustee`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StateName(pub String);

impl StateName {
    pub const BEGIN: &'static str = "BEGIN";
    pub const END: &'static str = "END";

    #[must_use]
    pub fn begin() -> Self {
        Self(Self::BEGIN.to_string())
    }

    #[must_use]
    pub fn end() -> Self {
        Self(Self::END.to_string())
    }

    /// Prefix used by [`crate::step_kind_for`] to pick the step
    /// type. For `lawyer_review__for_trustee` returns
    /// `"lawyer_review"`; for `BEGIN` or `END` returns the whole
    /// name verbatim.
    #[must_use]
    pub fn prefix(&self) -> &str {
        self.0.split_once("__").map_or(self.0.as_str(), |(p, _)| p)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for StateName {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Transitions out of a state: `condition -> next state`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransitionMap(pub BTreeMap<String, StateName>);

impl TransitionMap {
    #[must_use]
    pub fn lookup(&self, condition: &str) -> Option<&StateName> {
        self.0.get(condition)
    }

    pub fn conditions(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Actor class allowed to transition out of a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorClass {
    /// Driven by the durable runtime — no human in the loop.
    System,
    /// A lawyer triggers the transition.
    Lawyer,
    /// The respondent (the person/entity the notation is for)
    /// triggers the transition.
    Respondent,
}

/// Which of a Notation's two state machines a runtime call targets.
///
/// A Notation runs *two* state machines back-to-back: a
/// [`QuestionnaireSpec`] walks the respondent through the
/// declared questions, then a [`WorkflowSpec`] drives the
/// resulting document to final disposition. Both share the same
/// runtime surface; this enum is the key partitioner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MachineKind {
    /// The questionnaire walker — asks one question per signal.
    Questionnaire,
    /// The post-intake workflow — drives lawyer review, signing,
    /// mailroom, etc.
    Workflow,
}

impl MachineKind {
    /// Stable lowercase token used in Restate handler URLs and
    /// glossary copy.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Questionnaire => "questionnaire",
            Self::Workflow => "workflow",
        }
    }
}

/// Full workflow spec parsed from frontmatter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowSpec {
    pub states: BTreeMap<StateName, TransitionMap>,
}

#[derive(Debug, Error)]
pub enum WorkflowSpecError {
    #[error("missing required state `BEGIN`")]
    MissingBegin,
    #[error("missing required state `END`")]
    MissingEnd,
    #[error("state `{from}` has transition `{condition}` to unknown state `{to}`")]
    DanglingTransition {
        from: String,
        condition: String,
        to: String,
    },
    #[error(
        "questionnaire state `{state}` has condition `{condition}`, which cannot route — only a \
         single-choice question state can branch on its answer, and BEGIN and END hold no \
         answer at all"
    )]
    QuestionnaireCondition { state: String, condition: String },
    #[error(
        "questionnaire state `{state}` is unreachable from BEGIN — no route reaches it, so the \
         walker would never ask it and the rendered step total would lie"
    )]
    QuestionnaireUnreachable { state: String },
    #[error(
        "questionnaire walk from BEGIN stops or cycles at `{state}` before reaching END — \
         the walker would strand the respondent there"
    )]
    QuestionnaireChainBroken { state: String },
    #[error(
        "questionnaire state `{state}` declares choice `{choice}` but routes it nowhere — give \
         `{choice}` its own route or the state a `_` fallback, or the walker strands a \
         respondent who picks it"
    )]
    QuestionnaireRouteUncovered { state: String, choice: String },
    #[error(
        "questionnaire state `{state}` routes on `{condition}`, which is not one of the state's \
         declared `choices:` values — the walker could never take that route"
    )]
    QuestionnaireRouteUnknown { state: String, condition: String },
    #[error(
        "questionnaire answer at `{state}` routes nowhere — no choice edge matches and no `_` \
         fallback exists"
    )]
    RouteDeadEnd { state: String },
    #[error("yaml parse error: {0}")]
    Yaml(String),
}

impl WorkflowSpec {
    /// Parse from a YAML document. Validates `BEGIN`/`END` presence
    /// and that every transition target exists.
    pub fn from_yaml(yaml: &str) -> Result<Self, WorkflowSpecError> {
        let spec: Self =
            serde_yaml::from_str(yaml).map_err(|e| WorkflowSpecError::Yaml(e.to_string()))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<(), WorkflowSpecError> {
        if !self.states.contains_key(&StateName::begin()) {
            return Err(WorkflowSpecError::MissingBegin);
        }
        if !self.states.contains_key(&StateName::end()) {
            return Err(WorkflowSpecError::MissingEnd);
        }
        for (from, transitions) in &self.states {
            for (condition, target) in &transitions.0 {
                if !self.states.contains_key(target) {
                    return Err(WorkflowSpecError::DanglingTransition {
                        from: from.0.clone(),
                        condition: condition.clone(),
                        to: target.0.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Transitions out of `state`, or `None` if no such state.
    #[must_use]
    pub fn transitions_from(&self, state: &StateName) -> Option<&TransitionMap> {
        self.states.get(state)
    }

    #[must_use]
    pub fn is_terminal(&self, state: &StateName) -> bool {
        self.states.get(state).is_some_and(TransitionMap::is_empty)
    }
}

/// Parsed `questionnaire:` block from a notation template's
/// frontmatter. Same wire shape as [`WorkflowSpec`] — a graph of
/// named states keyed by transition condition — but distinct at
/// the type level so the application can't accidentally hand a
/// questionnaire spec to a workflow runtime call (or vice versa).
///
/// Wire shape (matches the retainer template's
/// [`templates/notations/neon_law/onboarding__letter.md`](../../../templates/notations/neon_law/onboarding__letter.md)
/// `questionnaire:` block):
///
/// ```yaml
/// questionnaire:
///   BEGIN:
///     _: client_name
///   client_name:
///     _: client_email
///   client_email:
///     _: END
///   END: {}
/// ```
///
/// State names use `<question-code>__<discriminator>` form (bare
/// question codes where the questionnaire asks one respondent), and
/// transitions route the walk that follows an answer:
///
/// - a linear state has one `_` edge — the single next question
///   whoever answers;
/// - a **branch state** (a single-choice question: `custom_single_choice`
///   or `custom_yes_no`) may additionally name one edge per declared
///   `choices:` value, taking the walk down that answer's route, with
///   an optional `_` fallback for values that declare no route;
///
/// so a questionnaire is a directed walk from `BEGIN` to `END` in
/// which every declared state is reachable and every answer either
/// names a route or has a fallback. [`QuestionnaireSpec::validate`]
/// enforces that shape at parse time, and
/// [`QuestionnaireSpec::validate_routes`] adds the choice-coverage
/// half where the template's `choices:` map is at hand, so
/// unreachable questions, cycles, and answer dead-ends are
/// compile-time gate errors, never runtime surprises.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QuestionnaireSpec(pub WorkflowSpec);

impl QuestionnaireSpec {
    /// Parse from a YAML document. Applies [`WorkflowSpec`]'s
    /// validation (`BEGIN`/`END` required, every transition target
    /// resolves) plus questionnaire shape — see
    /// [`QuestionnaireSpec::validate`].
    ///
    /// Choice-coverage checks need the `choices:` map, which this
    /// shape alone doesn't carry; assemble the walk definition with
    /// [`crate::notation_session::QuestionnaireDefinition`] and call
    /// [`QuestionnaireSpec::validate_routes`] there.
    pub fn from_yaml(yaml: &str) -> Result<Self, WorkflowSpecError> {
        let spec = Self(WorkflowSpec::from_yaml(yaml)?);
        spec.validate()?;
        Ok(spec)
    }

    /// Validate the questionnaire shape on top of the base
    /// [`WorkflowSpec::validate`] checks:
    ///
    /// 1. a named condition may sit only on a single-choice question
    ///    state — every other state routes by `_` alone;
    /// 2. every route out of `BEGIN` terminates at `END` (no dead
    ///    end, no cycle);
    /// 3. every declared state except `BEGIN`/`END` sits on some
    ///    route from `BEGIN`.
    ///
    /// Deserializing the transparent serde shape bypasses this, so
    /// every constructor that accepts authored YAML must call it.
    ///
    /// # Errors
    /// The `Questionnaire*` variants of [`WorkflowSpecError`], plus
    /// anything [`WorkflowSpec::validate`] returns.
    pub fn validate(&self) -> Result<(), WorkflowSpecError> {
        self.0.validate()?;
        for (state, transitions) in &self.0.states {
            let routable = routes_by_answer(state.as_str());
            for condition in transitions.conditions() {
                if condition != "_" && !routable {
                    return Err(WorkflowSpecError::QuestionnaireCondition {
                        state: state.as_str().to_string(),
                        condition: condition.to_string(),
                    });
                }
            }
        }

        // Walk every route out of BEGIN (below): a cycle or a dead
        // end on any route strands the respondent; converging diamonds
        // are ordinary questionnaires.
        let mut visited: BTreeSet<StateName> = BTreeSet::new();
        let mut on_path: BTreeSet<StateName> = BTreeSet::new();
        walk_routes(self, StateName::begin(), &mut on_path, &mut visited)?;

        for state in self.0.states.keys() {
            if *state == StateName::begin() || *state == StateName::end() {
                continue;
            }
            if !visited.contains(state) {
                return Err(WorkflowSpecError::QuestionnaireUnreachable {
                    state: state.as_str().to_string(),
                });
            }
        }
        Ok(())
    }

    /// The question state the walk lands on after `value` is
    /// recorded at `state`, following the branch semantics: the edge
    /// named by the answer's choice value, else the `_` fallback.
    /// Returns `None` when `state` has no outgoing transitions
    /// (already at `END`) or the answer matches no route and no
    /// fallback — the caller reports the dead end rather than
    /// guessing a next step.
    #[must_use]
    pub fn route_from(&self, state: &StateName, value: Option<&str>) -> Option<StateName> {
        let transitions = self.0.transitions_from(state)?;
        if let Some(value) = value {
            if let Some(next) = transitions.lookup(value) {
                return Some(next.clone());
            }
        }
        transitions.lookup("_").cloned()
    }

    /// The questions the walk will actually ask, in walk order, for a
    /// set of already-recorded answers (`taken`: state name → recorded
    /// value). Deterministic: ties between branches broken by BTree
    /// order so the rendered "step N of M" is stable for a given set
    /// of answers. `taken` empty walks the full fan-out from `BEGIN`,
    /// which for a linear questionnaire is exactly the old chain.
    ///
    /// Skips states whose recorded value routes to nothing a fallback
    /// can answer (a dead end the gate refuses to compile) by treating
    /// them as route-complete — the walk stops fanning at that state.
    #[must_use]
    pub fn question_order(&self, taken: &BTreeMap<String, String>) -> Vec<StateName> {
        let mut order = Vec::new();
        let mut queue: VecDeque<StateName> = VecDeque::from([StateName::begin()]);
        while let Some(here) = queue.pop_front() {
            if here == StateName::begin() || here == StateName::end() {
                if let Some(transitions) = self.0.transitions_from(&here) {
                    for next in transitions.0.values() {
                        queue.push_back(next.clone());
                    }
                }
                continue;
            }
            if order.contains(&here) {
                continue;
            }
            order.push(here.clone());
            let Some(transitions) = self.0.transitions_from(&here) else {
                continue;
            };
            match taken
                .get(here.as_str())
                .and_then(|value| transitions.lookup(value))
                .or_else(|| transitions.lookup("_"))
            {
                Some(next) => queue.push_back(next.clone()),
                None => continue,
            }
        }
        order
    }

    /// The choice-coverage half of the questionnaire gate, run where
    /// the template's `choices:` map is at hand: every named route on
    /// a branch state must be a declared choice value, and every
    /// declared value must have its own route or share the `_`
    /// fallback. Together with [`QuestionnaireSpec::validate`] this
    /// makes unreachable questions and answer dead-ends compile-time
    /// gate errors.
    ///
    /// # Errors
    /// [`WorkflowSpecError::QuestionnaireRouteUnknown`] for a route no
    /// answer can take; [`WorkflowSpecError::QuestionnaireRouteUncovered`]
    /// for a choice the walker strands.
    pub fn validate_routes(
        &self,
        choices: &BTreeMap<String, BTreeMap<String, String>>,
    ) -> Result<(), WorkflowSpecError> {
        for (state, transitions) in &self.0.states {
            let named: Vec<&str> = transitions
                .conditions()
                .into_iter()
                .filter(|c| *c != "_")
                .collect();
            if named.is_empty() {
                continue;
            }
            let declared = declared_choice_values(choices, state.as_str());
            for condition in &named {
                if !declared.iter().any(|value| value == condition) {
                    return Err(WorkflowSpecError::QuestionnaireRouteUnknown {
                        state: state.as_str().to_string(),
                        condition: (*condition).to_string(),
                    });
                }
            }
            if transitions.lookup("_").is_some() {
                continue;
            }
            for choice in &declared {
                if transitions.lookup(choice).is_none() {
                    return Err(WorkflowSpecError::QuestionnaireRouteUncovered {
                        state: state.as_str().to_string(),
                        choice: choice.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// The condition name the walk out of `state` follows for `value` —
    /// the answer's own choice edge when one matches, else `_`.
    /// Mirrors [`QuestionnaireSpec::route_from`]; the runtime signal
    /// carries the condition while the caller carries the destination.
    #[must_use]
    pub fn route_condition(&self, state: &StateName, value: Option<&str>) -> Option<String> {
        let transitions = self.0.transitions_from(state)?;
        if let Some(value) = value {
            if transitions.lookup(value).is_some() {
                return Some(value.to_string());
            }
        }
        transitions.lookup("_").map(|_| "_".to_string())
    }

    /// Borrow the underlying [`WorkflowSpec`] — useful when a
    /// runtime trait method takes the canonical machine spec.
    #[must_use]
    pub fn inner(&self) -> &WorkflowSpec {
        &self.0
    }

    /// Consume into the underlying [`WorkflowSpec`].
    #[must_use]
    pub fn into_inner(self) -> WorkflowSpec {
        self.0
    }

    /// Transitions out of `state`, or `None` if no such state.
    #[must_use]
    pub fn transitions_from(&self, state: &StateName) -> Option<&TransitionMap> {
        self.0.transitions_from(state)
    }

    /// Whether `state` is terminal (no outgoing transitions).
    #[must_use]
    pub fn is_terminal(&self, state: &StateName) -> bool {
        self.0.is_terminal(state)
    }
}

/// The state-name grammar helpers — the shared resolution of a question
/// state's code, role, and metadata keys, used by both the runtime answer
/// gate (`notation_session`) and the questionnaire route gate
/// (`validate_routes`) so neither can drift from the other. Mirrored in
/// `rules/src/f118.rs` only where the crate boundary forces it (rules
/// carries workflows as a dev-dependency); the N118 drift corpus pins
/// the two together.
pub(crate) fn question_code_for_state(state: &str) -> &str {
    state.split_once("__").map_or(state, |(code, _)| code)
}

pub(crate) fn role_key_for_state(state: &str) -> &str {
    state.split_once("__").map_or(state, |(_, role)| role)
}

pub(crate) fn metadata_keys_for_state(state: &str) -> Vec<&str> {
    let role = role_key_for_state(state);
    let ty = question_code_for_state(state);
    match (ty, role) {
        ("person", "client") => vec!["client", "client_name"],
        ("project", "engagement") => vec!["engagement", "project_name"],
        ("entity", "company") => vec!["company", "entity_name"],
        ("entity", "nonprofit") => vec!["nonprofit", "nonprofit_legal_name"],
        ("person", "worker") => vec!["worker", "worker_legal_name"],
        _ => vec![role],
    }
}

/// The declared choice VALUES for a question `state` — the keys the
/// walker renders and the only values a named route may match. Resolves
/// through the same metadata keys the runtime answer gate checks, so the
/// route gate and the answer gate close over one choice set.
fn declared_choice_values(
    choices: &BTreeMap<String, BTreeMap<String, String>>,
    state: &str,
) -> Vec<String> {
    metadata_keys_for_state(state)
        .into_iter()
        .find_map(|key| choices.get(key))
        .map(|entries| entries.keys().cloned().collect())
        .unwrap_or_default()
}

/// Depth-first route walk out of `state`: the first cycle (a route
/// back into a state still on the current path) or dead end (a
/// reachable state with no outgoing route other than END), or the set
/// of states the walk reached. A route into a state that was fully
/// explored on an earlier route is a converging diamond — END is the
/// common join — and is not a cycle.
fn walk_routes(
    questionnaire: &QuestionnaireSpec,
    state: StateName,
    on_path: &mut BTreeSet<StateName>,
    visited: &mut BTreeSet<StateName>,
) -> Result<(), WorkflowSpecError> {
    if visited.contains(&state) {
        return Ok(());
    }
    if !on_path.insert(state.clone()) {
        return Err(WorkflowSpecError::QuestionnaireChainBroken {
            state: state.as_str().to_string(),
        });
    }
    if let Some(transitions) = questionnaire.0.transitions_from(&state) {
        if transitions.is_empty() && state != StateName::end() {
            return Err(WorkflowSpecError::QuestionnaireChainBroken {
                state: state.as_str().to_string(),
            });
        }
        for next in transitions.0.values() {
            walk_routes(questionnaire, next.clone(), on_path, visited)?;
        }
    }
    on_path.remove(&state);
    visited.insert(state);
    Ok(())
}

/// Whether a question state's answer is one declared choice value a
/// route can match: the single-choice primitives. A multiple-choice
/// answer selects a *set*, a free-text answer carries no declared set,
/// and a record/reference answer is keyed to the store — none of them
/// name an edge, so `validate` refuses named routes on them.
fn routes_by_answer(state: &str) -> bool {
    use store::question_registry::QuestionType;
    matches!(
        QuestionType::from_state_name(state),
        Some(QuestionType::CustomSingleChoice | QuestionType::CustomYesNo)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        ActorClass, MachineKind, QuestionnaireSpec, StateName, WorkflowSpec, WorkflowSpecError,
    };

    const TRUST_WORKFLOW: &str = r"
BEGIN:
  created: lawyer_review__for_grantor
lawyer_review__for_grantor:
  approve: notarization__for_grantor
  reject: END
notarization__for_grantor:
  signed: mailroom_send__to_signer
  refused: END
mailroom_send__to_signer:
  sent: mailroom_receive__signed_copy
mailroom_receive__signed_copy:
  received: END
END: {}
";

    #[test]
    fn parses_a_realistic_trust_workflow() {
        let spec = WorkflowSpec::from_yaml(TRUST_WORKFLOW).expect("valid spec");
        assert_eq!(spec.states.len(), 6);
        let begin = spec.transitions_from(&StateName::begin()).unwrap();
        assert_eq!(
            begin.lookup("created").unwrap().as_str(),
            "lawyer_review__for_grantor"
        );
        assert!(spec.is_terminal(&StateName::end()));
    }

    #[test]
    fn rejects_spec_without_begin_state() {
        let err = WorkflowSpec::from_yaml("END: {}\n").unwrap_err();
        assert!(matches!(err, WorkflowSpecError::MissingBegin));
    }

    #[test]
    fn rejects_spec_without_end_state() {
        let err = WorkflowSpec::from_yaml("BEGIN: {created: somewhere}\n").unwrap_err();
        assert!(matches!(err, WorkflowSpecError::MissingEnd));
    }

    #[test]
    fn rejects_dangling_transition_target() {
        let err = WorkflowSpec::from_yaml("BEGIN: {go: nowhere}\nEND: {}\n").unwrap_err();
        assert!(matches!(
            err,
            WorkflowSpecError::DanglingTransition { to, .. } if to == "nowhere"
        ));
    }

    #[test]
    fn state_name_prefix_strips_double_underscore_discriminator() {
        assert_eq!(
            StateName::from("lawyer_review__for_trustee").prefix(),
            "lawyer_review"
        );
        assert_eq!(StateName::begin().prefix(), "BEGIN");
        assert_eq!(StateName::from("notarization").prefix(), "notarization");
    }

    #[test]
    fn actor_class_serialization_matches_yaml_lowercase() {
        let yaml = serde_yaml::to_string(&ActorClass::Lawyer).unwrap();
        assert_eq!(yaml.trim(), "lawyer");
        let back: ActorClass = serde_yaml::from_str("system").unwrap();
        assert_eq!(back, ActorClass::System);
    }

    #[test]
    fn yaml_parse_error_surfaces_as_workflow_spec_error_yaml_variant() {
        // Plain string at the top level — fails to deserialize into
        // the spec's BTreeMap shape before validation runs.
        let err = WorkflowSpec::from_yaml("just_a_scalar_string\n").unwrap_err();
        assert!(matches!(err, WorkflowSpecError::Yaml(_)), "got {err:?}");
    }

    const RETAINER_QUESTIONNAIRE: &str = r"
BEGIN:
  _: client_name
client_name:
  _: client_email
client_email:
  _: project_name
project_name:
  _: product_description
product_description:
  _: END
END: {}
";

    #[test]
    fn questionnaire_spec_parses_the_retainer_questionnaire_block() {
        let q = QuestionnaireSpec::from_yaml(RETAINER_QUESTIONNAIRE).expect("valid");
        let first = q
            .transitions_from(&StateName::begin())
            .and_then(|t| t.lookup("_"))
            .map(StateName::as_str);
        assert_eq!(first, Some("client_name"));
        assert!(q.is_terminal(&StateName::end()));
    }

    #[test]
    fn questionnaire_spec_reuses_workflow_spec_validation_for_missing_begin() {
        let err = QuestionnaireSpec::from_yaml("END: {}\n").unwrap_err();
        assert!(matches!(err, WorkflowSpecError::MissingBegin));
    }

    #[test]
    fn questionnaire_rejects_a_named_condition_on_a_state_that_cannot_route() {
        // LAW-209: a named condition is a branch route — only a
        // single-choice state's answer can take one. `client_name` is a
        // free-answer person question, so `skip` can never match.
        let err = QuestionnaireSpec::from_yaml(
            "
BEGIN:
  _: client_name
client_name:
  skip: END
  _: END
END: {}
",
        )
        .unwrap_err();
        assert!(
            matches!(
                &err,
                WorkflowSpecError::QuestionnaireCondition { state, condition }
                    if state == "client_name" && condition == "skip"
            ),
            "{err:?}"
        );
    }

    #[test]
    fn questionnaire_rejects_a_question_no_route_reaches() {
        // `orphan` is declared and dangling-free (it points at END) but
        // the walker would never ask it — the step total would lie.
        let err = QuestionnaireSpec::from_yaml(
            "
BEGIN:
  _: client_name
client_name:
  _: END
orphan:
  _: END
END: {}
",
        )
        .unwrap_err();
        assert!(
            matches!(
                &err,
                WorkflowSpecError::QuestionnaireUnreachable { state } if state == "orphan"
            ),
            "{err:?}"
        );
    }

    #[test]
    fn questionnaire_rejects_a_chain_that_dead_ends_before_end() {
        let err = QuestionnaireSpec::from_yaml(
            "
BEGIN:
  _: client_name
client_name: {}
END: {}
",
        )
        .unwrap_err();
        assert!(
            matches!(
                &err,
                WorkflowSpecError::QuestionnaireChainBroken { state } if state == "client_name"
            ),
            "{err:?}"
        );
    }

    #[test]
    fn questionnaire_rejects_a_chain_that_cycles_before_end() {
        let err = QuestionnaireSpec::from_yaml(
            "
BEGIN:
  _: client_name
client_name:
  _: client_email
client_email:
  _: client_name
END: {}
",
        )
        .unwrap_err();
        assert!(
            matches!(&err, WorkflowSpecError::QuestionnaireChainBroken { .. }),
            "{err:?}"
        );
    }

    /// LAW-209: the branched shape the issue asks for — a single-choice
    /// state names one edge per declared choice, both branches reach
    /// END, and the walk follows the answered route.
    const BRANCHED: &str = "\
BEGIN:\n  _: custom_single_choice__path\ncustom_single_choice__path:\n  litigation: \
             jurisdiction__venue\n  transactional: custom_datetime__closing_date\n\
             jurisdiction__venue:\n  _: END\ncustom_datetime__closing_date:\n  _: END\n\
             END: {}\n";

    #[test]
    fn a_branched_questionnaire_parses_and_walks_the_answered_route() {
        let q = QuestionnaireSpec::from_yaml(BRANCHED).expect("branched questionnaire is valid");
        let path = StateName::from("custom_single_choice__path");

        // The answer picks the edge; with no `_` fallback, an answer
        // that names no route is a dead end the caller reports.
        assert_eq!(
            q.route_from(&path, Some("transactional")),
            Some(StateName::from("custom_datetime__closing_date"))
        );
        assert_eq!(
            q.route_from(&path, Some("litigation")),
            Some(StateName::from("jurisdiction__venue"))
        );
        assert_eq!(q.route_from(&path, Some("mediation")), None);

        // With no answers the walk fans only what the route policy can
        // reach: the branch state itself — neither branch is taken yet.
        assert_eq!(q.question_order(&BTreeMap::new()), vec![path.clone()]);

        // The recorded answer prunes the not-taken branch, so the
        // rendered step total tracks the questions actually asked.
        let mut taken = BTreeMap::new();
        taken.insert(path.as_str().to_string(), "transactional".to_string());
        assert_eq!(
            q.question_order(&taken),
            vec![
                path.clone(),
                StateName::from("custom_datetime__closing_date")
            ]
        );
        taken.insert(path.as_str().to_string(), "litigation".to_string());
        assert_eq!(
            q.question_order(&taken),
            vec![path.clone(), StateName::from("jurisdiction__venue")]
        );
    }

    /// The choice-coverage half of the gate — the compile-time answer
    /// dead-end: every declared choice must reach a route (its own or
    /// the `_` fallback), and every named route must be an answer a
    /// respondent can give.
    #[test]
    fn a_choice_without_a_route_or_fallback_is_a_gate_error() {
        let spec = QuestionnaireSpec::from_yaml(BRANCHED).expect("shape is valid");
        let mut choices = BTreeMap::new();
        let mut options = BTreeMap::new();
        options.insert("litigation".to_string(), "Litigation".to_string());
        options.insert("transactional".to_string(), "Transactional".to_string());
        options.insert("mediation".to_string(), "Mediation".to_string());
        choices.insert("path".to_string(), options);
        let err = spec.validate_routes(&choices).unwrap_err();
        assert!(
            matches!(
                &err,
                WorkflowSpecError::QuestionnaireRouteUncovered { state, choice }
                    if state == "custom_single_choice__path" && choice == "mediation"
            ),
            "{err:?}"
        );
    }

    #[test]
    fn a_named_route_no_answer_can_take_is_a_gate_error() {
        let spec = QuestionnaireSpec::from_yaml(
            "
BEGIN:\n  _: custom_single_choice__path\ncustom_single_choice__path:\n  litigation: \
             jurisdiction__venue\n  transactional: custom_datetime__closing_date\n  \
             mediation: jurisdiction__venue\njurisdiction__venue:\n  _: END\n\
             custom_datetime__closing_date:\n  _: END\nEND: {}\n",
        )
        .expect("the shape is valid — route coverage needs the choices map");
        let mut choices = BTreeMap::new();
        let mut options = BTreeMap::new();
        options.insert("litigation".to_string(), "Litigation".to_string());
        options.insert("transactional".to_string(), "Transactional".to_string());
        choices.insert("path".to_string(), options);
        let err = spec.validate_routes(&choices).unwrap_err();
        assert!(
            matches!(
                &err,
                WorkflowSpecError::QuestionnaireRouteUnknown { state, condition }
                    if state == "custom_single_choice__path" && condition == "mediation"
            ),
            "{err:?}"
        );
    }

    #[test]
    fn a_diamond_that_rejoins_a_shared_state_is_valid() {
        // LAW-209: converging routes are ordinary branched shape — both
        // branches rejoin one follow-up question, then END. Not a cycle.
        let spec = QuestionnaireSpec::from_yaml(
            "BEGIN:\n  _: custom_single_choice__p\ncustom_single_choice__p:\n  litigation: \
             custom_datetime__close\n  transactional: custom_datetime__close\n\
             custom_datetime__close:\n  _: END\nEND: {}\n",
        )
        .expect("diamond is valid");
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn a_fallback_covers_the_choices_that_name_no_route() {
        let spec = QuestionnaireSpec::from_yaml(
            "
BEGIN:\n  _: custom_single_choice__path\ncustom_single_choice__path:\n  litigation: \
             jurisdiction__venue\n  _: custom_datetime__closing_date\n\
             jurisdiction__venue:\n  _: END\ncustom_datetime__closing_date:\n  _: END\n\
             END: {}\n",
        )
        .expect("the fallback covers every choice without its own route");
        let mut choices = BTreeMap::new();
        let mut options = BTreeMap::new();
        options.insert("litigation".to_string(), "Litigation".to_string());
        options.insert("transactional".to_string(), "Transactional".to_string());
        choices.insert("path".to_string(), options);
        spec.validate_routes(&choices)
            .expect("both choices covered");
    }

    #[test]
    fn an_empty_questionnaire_that_goes_straight_to_end_is_linear() {
        let q = QuestionnaireSpec::from_yaml("BEGIN:\n  _: END\nEND: {}\n").expect("valid");
        assert!(q.is_terminal(&StateName::end()));
    }

    #[test]
    fn questionnaire_spec_exposes_underlying_workflow_spec() {
        let q = QuestionnaireSpec::from_yaml(RETAINER_QUESTIONNAIRE).unwrap();
        // `inner()` borrows the same graph; `into_inner()` consumes.
        assert_eq!(q.inner().states.len(), 6);
        let unwrapped = q.into_inner();
        assert!(unwrapped.is_terminal(&StateName::end()));
    }

    #[test]
    fn machine_kind_serializes_as_lowercase_tokens() {
        let q = serde_yaml::to_string(&MachineKind::Questionnaire).unwrap();
        let w = serde_yaml::to_string(&MachineKind::Workflow).unwrap();
        assert_eq!(q.trim(), "questionnaire");
        assert_eq!(w.trim(), "workflow");
    }

    #[test]
    fn machine_kind_as_str_matches_serde_tokens() {
        assert_eq!(MachineKind::Questionnaire.as_str(), "questionnaire");
        assert_eq!(MachineKind::Workflow.as_str(), "workflow");
    }
}
