//! Agreement between authoring diagnostics and workflow execution.

use std::fmt::Write as _;

use rules::{workflow_steps::WORKFLOW_STEPS, Rule};

#[test]
fn catalog_covers_every_engine_prefix() {
    // The drift guard: every prefix the workflow engine routes must
    // be described here. A new step in workflows::step forces a
    // description in this catalog — no undocumented steps.
    for (prefix, _) in workflows::step::STEP_PREFIXES {
        assert!(
            WORKFLOW_STEPS.iter().any(|s| s.prefix == *prefix),
            "engine step prefix `{prefix}` has no entry in the workflow-step catalog",
        );
    }
}

#[test]
fn catalog_has_no_prefix_unknown_to_the_engine() {
    // And the reverse: the catalog must not describe a step the
    // engine doesn't route, or hover would lie.
    for step in WORKFLOW_STEPS {
        assert!(
            workflows::step::STEP_PREFIXES
                .iter()
                .any(|(p, _)| *p == step.prefix),
            "catalog prefix `{}` is not a real engine step",
            step.prefix,
        );
    }
}

#[test]
fn outbound_set_matches_workflows_guardrail() {
    // Drift lock: N116's outbound-submission classification must stay
    // identical to the runtime guardrail it mirrors. If the engine
    // adds or moves a submission prefix, this fails until N116 keeps
    // up — one definition of "outbound", two enforcement points.
    use workflows::guardrail::is_submission_state;
    use workflows::spec::StateName;
    for state in [
        "mailroom_send",
        "mailroom_send__notice",
        "mailroom_receive",
        "mailroom_receive__biometrics",
        "certified_mail",
        "certified_mail__demand",
        "e_filing",
        "e_filing__uscis",
        "filing__nv_sos",
        "generate_pdf__retainer_pdf",
        "sent_for_signature__pending",
        "sent_for_signature__settlement",
        "lawyer_review",
        "lawyer_review__for_grantor",
        "client_review",
        "document_drafts__estate",
        "intake_persisted__client",
        "BEGIN",
        "END",
    ] {
        assert_eq!(
            !rules::F116LawyerReviewGatesSubmission.lint(&rules::SourceFile {
                path: "templates/submission.md".into(),
                contents: format!("---\nworkflow:\n  BEGIN:\n    _: {state}\n---\n"),
            }).is_empty(),
            is_submission_state(&StateName::from(state)),
            "N116 outbound set drifted from workflows::guardrail::is_submission_state for `{state}`",
        );
    }
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
        let parsed_choices: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, String>,
        > = choices
            .map(|c| serde_yaml::from_str(c).expect("corpus choices parse"))
            .unwrap_or_default();
        let mut contents = String::from("---\nquestionnaire:\n");
        for line in yaml.lines() {
            writeln!(contents, "  {line}").unwrap();
        }
        if let Some(choices) = choices {
            contents.push_str("choices:\n");
            for line in choices.lines() {
                writeln!(contents, "  {line}").unwrap();
            }
        }
        contents.push_str("---\n");
        let rule_flags = !rules::F118QuestionnaireRoutes
            .lint(&rules::SourceFile {
                path: "templates/questionnaire.md".into(),
                contents,
            })
            .is_empty();
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
