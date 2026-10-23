//! `N104` — questionnaire states must reference valid question codes
//! and workflow states must compose known workflow step prefixes.
//!
//! Questionnaire state name shape: `<question_code>__<discriminator>`
//! (the `__<discriminator>` part is optional). The prefix before the
//! first `__` must appear in the configured valid-codes set. Workflow
//! state names use the same discriminator convention, but their prefix
//! must come from the reusable workflow-step catalog. The sentinel
//! states `BEGIN` and `END` are exempt.
//!
//! Both the `questionnaire:` and `workflow:` maps in frontmatter
//! are validated; both must declare a `BEGIN` state and reach
//! `END` from at least one transition.
//!
//! Omitting both keys is valid on every `kind` — not only `letter` and
//! `memo` — and means exactly the minimal pair: `BEGIN → END` for the
//! questionnaire, `BEGIN → lawyer_review → END` for the workflow. This
//! includes a body that carries a signature placeholder, a signature
//! heading, or a manual signing line: a manual signature line shows the
//! document gets signed, not that Navigator runs the signing, so it is
//! not by itself a reason to demand a questionnaire and workflow that
//! describe nothing that happens. `N107` still requires a signature
//! anchor whenever a *declared* workflow includes `sent_for_signature`
//! — the protection that matters stays keyed to the workflow the author
//! actually wrote, not inferred from the body.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;

use crate::{frontmatter, line_byte_range, Rule, SourceFile, Violation};

pub struct F104FlowQuestionCodes {
    valid_codes: HashSet<String>,
    /// Whether a `workflow:` block is required. True for a legal notation,
    /// whose document only becomes real by advancing through review,
    /// signature, and filing. False for a `kind: github` notation, which
    /// renders its answers into an issue or pull request body and stops —
    /// it has no workflow to run, and demanding an unexecuted one would
    /// put a vestigial state machine in every file on that shelf.
    workflow_required: bool,
}

impl F104FlowQuestionCodes {
    pub const CODE: &'static str = "N104";

    #[must_use]
    pub fn new<I, S>(codes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            valid_codes: codes.into_iter().map(Into::into).collect(),
            workflow_required: true,
        }
    }

    /// The same rule with the `workflow:` requirement lifted — every
    /// questionnaire check still runs. See [`Self::workflow_required`].
    #[must_use]
    pub fn questionnaire_only<I, S>(codes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            workflow_required: false,
            ..Self::new(codes)
        }
    }
}

#[derive(Debug, Deserialize)]
struct FrontmatterShape {
    #[serde(default)]
    questionnaire: Option<BTreeMap<String, BTreeMap<String, String>>>,
    #[serde(default)]
    prompts: BTreeMap<String, String>,
    #[serde(default)]
    choices: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    workflow: Option<BTreeMap<String, BTreeMap<String, String>>>,
}

/// The two custom types that carry a one-off option list; every other
/// `custom_*` primitive takes a free value and must not define `choices`.
const CHOICE_CUSTOM_TYPES: &[&str] = &["custom_single_choice", "custom_multiple_choice"];

impl Rule for F104FlowQuestionCodes {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let Some(fm) = frontmatter::extract(&file.contents) else {
            return Vec::new();
        };
        let Ok(parsed) = crate::questionnaire::deserialize_frontmatter::<FrontmatterShape>(fm)
        else {
            return Vec::new();
        };

        let mut violations = Vec::new();
        let Some(questionnaire) = parsed.questionnaire else {
            if parsed.workflow.is_none() {
                return violations;
            }
            violations.push(violation(file, "Missing required `questionnaire` key"));
            return violations;
        };
        let workflow = match parsed.workflow {
            Some(workflow) => Some(workflow),
            None if self.workflow_required => {
                violations.push(violation(file, "Missing required `workflow` key"));
                return violations;
            }
            None => None,
        };
        self.validate_questionnaire(
            file,
            &questionnaire,
            &parsed.prompts,
            &parsed.choices,
            &mut violations,
        );
        // A declared `workflow:` is validated whether or not it was
        // required, so a GitHub notation that grows one later is still held
        // to the same shape.
        if let Some(workflow) = workflow {
            Self::validate_workflow(file, &workflow, &mut violations);
        }
        violations
    }
}

impl F104FlowQuestionCodes {
    fn validate_common_shape(
        file: &SourceFile,
        map: &BTreeMap<String, BTreeMap<String, String>>,
        map_name: &str,
        violations: &mut Vec<Violation>,
    ) -> bool {
        if !map.contains_key("BEGIN") {
            violations.push(violation(
                file,
                format!("{map_name} is missing required BEGIN state"),
            ));
            return false;
        }
        let reaches_end = map.values().any(|t| t.values().any(|n| n == "END"));
        if !reaches_end {
            violations.push(violation(
                file,
                format!("{map_name} is missing required END state"),
            ));
            return false;
        }
        true
    }

    fn validate_questionnaire(
        &self,
        file: &SourceFile,
        map: &BTreeMap<String, BTreeMap<String, String>>,
        prompts: &BTreeMap<String, String>,
        choices: &BTreeMap<String, BTreeMap<String, String>>,
        violations: &mut Vec<Violation>,
    ) {
        if !Self::validate_common_shape(file, map, "questionnaire", violations) {
            return;
        }
        if self.valid_codes.is_empty() {
            // No registry provided — fall back to structural checks only
            // (BEGIN/END presence), matching the behavior callers get
            // when the default factory is used without supplying codes.
            return;
        }
        for state in map.keys() {
            if state == "BEGIN" || state == "END" {
                continue;
            }
            let prefix = state.split_once("__").map_or(state.as_str(), |(p, _)| p);
            if !self.valid_codes.contains(prefix) {
                violations.push(violation(
                    file,
                    format!("Invalid question code: `{prefix}` (from state `{state}`)"),
                ));
            }
            if prefix.starts_with("custom_") {
                Self::validate_custom_question(file, state, prefix, prompts, choices, violations);
            }
        }
    }

    /// A `custom_*` state gets its wording (and, for a choice type, its
    /// options) from `prompts.<prompt_key>` and `choices.<prompt_key>` — the bank
    /// supplies nothing for a one-off. Enforce that the entry exists and
    /// that its `choices` presence matches the type: choice types require
    /// options, every other custom primitive forbids them.
    fn validate_custom_question(
        file: &SourceFile,
        state: &str,
        prefix: &str,
        prompts: &BTreeMap<String, String>,
        choices: &BTreeMap<String, BTreeMap<String, String>>,
        violations: &mut Vec<Violation>,
    ) {
        let Some((_, prompt_key)) = state.split_once("__") else {
            violations.push(violation(
                file,
                format!(
                    "Custom question state `{state}` must use `custom_*__prompt_key` and define `prompts.<prompt_key>`"
                ),
            ));
            return;
        };
        let Some(prompt) = prompts.get(prompt_key) else {
            violations.push(violation(
                file,
                format!(
                    "Custom question state `{state}` is missing required `prompts.{prompt_key}`"
                ),
            ));
            return;
        };
        if prompt.trim().is_empty() {
            violations.push(violation(
                file,
                format!("Custom question `prompts.{prompt_key}` needs a non-empty `prompt`"),
            ));
        }
        let is_choice_type = CHOICE_CUSTOM_TYPES.contains(&prefix);
        if is_choice_type && choices.get(prompt_key).is_none_or(BTreeMap::is_empty) {
            violations.push(violation(
                file,
                format!(
                    "Custom question state `{state}` is a choice type and needs \
                     `choices.{prompt_key}`"
                ),
            ));
        } else if !is_choice_type && !choices.get(prompt_key).is_none_or(BTreeMap::is_empty) {
            violations.push(violation(
                file,
                format!(
                    "Custom question `prompts.{prompt_key}` (`{prefix}`) must not define \
                     `choices` — only custom_single_choice / custom_multiple_choice may"
                ),
            ));
        }
    }

    fn validate_workflow(
        file: &SourceFile,
        map: &BTreeMap<String, BTreeMap<String, String>>,
        violations: &mut Vec<Violation>,
    ) {
        if !Self::validate_common_shape(file, map, "workflow", violations) {
            return;
        }
        for state in map.keys() {
            if state == "BEGIN" || state == "END" {
                continue;
            }
            let prefix = state.split_once("__").map_or(state.as_str(), |(p, _)| p);
            if !valid_workflow_step_prefix(prefix) {
                violations.push(violation(
                    file,
                    format!("Invalid workflow step prefix: `{prefix}` (from state `{state}`)"),
                ));
            }
        }
    }
}

/// Whether `prefix` is an allowed workflow-step prefix. Delegates to the
/// single source of truth, the [`crate::workflow_steps`] catalog (which
/// also handles the `_signature` / `_signatures` suffix family), so the
/// allow-list and the hover descriptions can never drift apart.
#[must_use]
pub fn valid_workflow_step_prefix(prefix: &str) -> bool {
    crate::workflow_steps::is_allowed_prefix(prefix)
}

fn violation(file: &SourceFile, message: impl Into<String>) -> Violation {
    Violation {
        code: F104FlowQuestionCodes::CODE,
        path: file.path.clone(),
        line: 1,
        range: line_byte_range(&file.contents, 1),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{valid_workflow_step_prefix, F104FlowQuestionCodes};
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    fn file(body: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.md"),
            contents: body.to_string(),
        }
    }

    const VALID_CODES: &[&str] = &["trustee_name", "beneficiary_name"];

    fn rule() -> F104FlowQuestionCodes {
        F104FlowQuestionCodes::new(VALID_CODES.iter().copied())
    }

    #[test]
    fn passes_on_clean_questionnaire_and_workflow() {
        let body = "---
title: T
questionnaire:
  BEGIN:
    created: trustee_name
  trustee_name:
    answered: beneficiary_name
  beneficiary_name:
    answered: END
  END: {}
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---
";
        let violations = rule().lint(&file(body));
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn no_frontmatter_means_no_violation() {
        assert!(rule().lint(&file("just body")).is_empty());
    }

    #[test]
    fn every_kind_may_omit_both_machines() {
        // LAW-44: omission is valid on every `kind`, not only `letter` and
        // `memo` — a bespoke agreement or an attorney-drafted filing has as
        // much claim to being hand-drafted as a memo does.
        for kind in ["letter", "memo", "agreement", "onboarding", "filing"] {
            let source = file(&format!(
                "---\nkind: {kind}\n---\n\nAttorney-drafted body.\n"
            ));
            assert!(
                rule().lint(&source).is_empty(),
                "{kind} may omit questionnaire and workflow"
            );
        }
    }

    #[test]
    fn a_signable_document_may_omit_both_machines_regardless_of_kind() {
        // LAW-84: a document with real binding terms and a signature
        // block — the shipped Nevada engagement letter looks exactly
        // like this: numbered substantive sections, then a "Signatures"
        // heading with manual "By: ____  Date: ____" lines and no `{{ }}`
        // placeholder at all — is not thereby forced to carry a
        // questionnaire and workflow. A manual signature line shows the
        // document gets signed, not that Navigator runs the signing.
        for kind in ["letter", "memo", "agreement"] {
            let body = format!(
                "---\nkind: {kind}\n---\n\n## I. Terms\n\nThe Firm will represent you.\n\n\
                 ## IX. Signatures\n\nBy: ______________________________  Date: ____________\n"
            );
            assert!(
                rule().lint(&file(&body)).is_empty(),
                "{kind}: a signable body may omit both machines"
            );
        }
    }

    #[test]
    fn plain_and_structured_signature_lines_may_omit_both_machines_on_any_kind() {
        for marker in [
            "Please sign below.\n\nSignature: ____________________\n",
            "Attorney-drafted terms.\n\nSigned:\n",
            "Attorney-drafted terms.\n\nSignature\n",
            "Attorney-drafted terms.\n\n____________________________\n",
        ] {
            for kind in ["letter", "memo", "agreement", "onboarding", "filing"] {
                let source = file(&format!("---\nkind: {kind}\n---\n\n{marker}"));
                assert!(
                    rule().lint(&source).is_empty(),
                    "a signature line in a {kind} may omit both machines: {marker:?}"
                );
            }
        }
    }

    #[test]
    fn signature_labels_with_affordances_may_omit_both_machines_on_any_kind() {
        for marker in [
            "Client Signature: ____________________\n",
            "Attorney Signature:\n",
            "Authorized Signature:\n",
        ] {
            for kind in ["letter", "memo", "agreement", "onboarding", "filing"] {
                let source = file(&format!("---\nkind: {kind}\n---\n\n{marker}"));
                assert!(
                    rule().lint(&source).is_empty(),
                    "a signature label in a {kind} may omit both machines: {marker:?}"
                );
            }
        }
    }

    #[test]
    fn a_wet_ink_instrument_with_signature_witness_and_notary_lines_may_omit_both_machines() {
        // LAW-84: an attorney-drafted estate instrument that is printed
        // and signed in person — manual signature, witness, and notary
        // acknowledgment lines, none of it a `{{signer.field}}`
        // placeholder — never enters Navigator's signature flow, so it
        // has no more claim on a questionnaire/workflow pair than any
        // other prose-only blueprint.
        for kind in ["will", "trust", "directive", "agreement"] {
            let body = format!(
                "---\nkind: {kind}\n---\n\n## I. Terms\n\nThe undersigned executes this \
                 instrument as follows.\n\n## IX. Execution\n\nSignature: ____________________\n\n\
                 Witness Signature: ____________________\n\n\
                 State of Nevada\nCounty of Washoe\n\n\
                 Subscribed and sworn before me this ____ day of __________, 20__.\n\n\
                 Notary Signature: ____________________\n\
                 My Commission Expires: ____________________\n"
            );
            assert!(
                rule().lint(&file(&body)).is_empty(),
                "{kind}: manual signature, witness, and notary lines may omit both machines"
            );
        }
    }

    #[test]
    fn prose_mentioning_signing_words_may_still_omit_both_machines_on_any_kind() {
        for sentence in [
            "This letter must be signed by an authorized officer.",
            "Please date and sign the enclosed copy.",
            "No signatures are required for this memo.",
        ] {
            for kind in ["letter", "memo", "agreement", "onboarding", "filing"] {
                let source = file(&format!("---\nkind: {kind}\n---\n\n{sentence}\n"));
                assert!(
                    rule().lint(&source).is_empty(),
                    "a prose sentence mentioning a signature-related word in a {kind} must not require either machine: {sentence:?}"
                );
            }
        }
    }

    #[test]
    fn every_kind_must_omit_both_machines_together() {
        for kind in ["memo", "agreement"] {
            for machine in ["questionnaire", "workflow"] {
                let source = file(&format!(
                    "---\nkind: {kind}\n{machine}:\n  BEGIN:\n    _: END\n  END: {{}}\n---\n"
                ));
                let violations = rule().lint(&source);
                assert_eq!(violations.len(), 1, "{kind}/{machine}: {violations:?}");
                assert!(
                    violations[0]
                        .message
                        .contains(if machine == "questionnaire" {
                            "workflow"
                        } else {
                            "questionnaire"
                        }),
                    "{kind}/{machine}: {violations:?}"
                );
            }
        }
    }

    #[test]
    fn custom_states_read_top_level_prompts_and_choices() {
        let source = file("---\nquestionnaire:\n  BEGIN: { _: custom_single_choice__delivery }\n  custom_single_choice__delivery: { _: END }\n  END: {}\nprompts:\n  delivery: How should it arrive?\nchoices:\n  delivery:\n    email: By email\n    post: By post\n---\n");
        let findings =
            F104FlowQuestionCodes::questionnaire_only(["custom_single_choice"]).lint(&source);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn missing_questionnaire_key_is_a_violation() {
        let body = "---\nworkflow:\n  BEGIN:\n    a: END\n  END: {}\n---\n";
        let v = rule().lint(&file(body));
        assert_eq!(v.len(), 1);
        assert!(v[0].message.contains("Missing required `questionnaire`"));
    }

    #[test]
    fn missing_workflow_key_is_a_violation() {
        let body = "---\nquestionnaire:\n  BEGIN:\n    a: END\n  END: {}\n---\n";
        let v = rule().lint(&file(body));
        assert_eq!(v.len(), 1);
        assert!(v[0].message.contains("Missing required `workflow`"));
    }

    #[test]
    fn missing_begin_state_in_either_map_is_a_violation() {
        let body = "---
questionnaire:
  trustee_name:
    answered: END
  END: {}
workflow:
  BEGIN:
    a: END
  END: {}
---
";
        let v = rule().lint(&file(body));
        assert!(v
            .iter()
            .any(|x| x.message.contains("questionnaire") && x.message.contains("BEGIN")));
    }

    #[test]
    fn flags_state_referencing_unknown_question_code() {
        let body = "---
questionnaire:
  BEGIN:
    created: not_a_valid_code
  not_a_valid_code:
    answered: END
  END: {}
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---
";
        let v = rule().lint(&file(body));
        assert!(v.iter().any(|x| x.message.contains("Invalid question code")
            && x.message.contains("not_a_valid_code")));
    }

    #[test]
    fn double_underscore_suffix_is_stripped_for_code_lookup() {
        let body = "---
questionnaire:
  BEGIN:
    created: trustee_name__for_grantor
  trustee_name__for_grantor:
    answered: END
  END: {}
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---
";
        assert!(rule().lint(&file(body)).is_empty());
    }

    #[test]
    fn custom_question_states_require_a_prompt() {
        let body = "---
questionnaire:
  BEGIN:
    created: custom_text__fundraising_activities
  custom_text__fundraising_activities:
    answered: END
  END: {}
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---
";
        let v = F104FlowQuestionCodes::new(["custom_text"]).lint(&file(body));
        assert!(v.iter().any(|x| x
            .message
            .contains("missing required `prompts.fundraising_activities`")));
    }

    #[test]
    fn custom_question_states_accept_a_matching_prompt() {
        let body = "---
questionnaire:
  BEGIN:
    created: custom_text__fundraising_activities
  custom_text__fundraising_activities:
    answered: END
  END: {}
prompts:
  fundraising_activities: What are the fundraising activities?
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---
";
        assert!(F104FlowQuestionCodes::new(["custom_text"])
            .lint(&file(body))
            .is_empty());
    }

    #[test]
    fn custom_question_with_a_blank_prompt_is_flagged() {
        let body = "---
questionnaire:
  BEGIN:
    created: custom_text__note
  custom_text__note:
    answered: END
  END: {}
prompts:
  note: '   '
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---
";
        let v = F104FlowQuestionCodes::new(["custom_text"]).lint(&file(body));
        assert!(
            v.iter()
                .any(|x| x.message.contains("needs a non-empty `prompt`")),
            "got {v:?}"
        );
    }

    #[test]
    fn choice_custom_type_requires_choices() {
        let body = "---
questionnaire:
  BEGIN:
    created: custom_single_choice__basis
  custom_single_choice__basis:
    answered: END
  END: {}
prompts:
  basis: Which basis applies?
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---
";
        let v = F104FlowQuestionCodes::new(["custom_single_choice"]).lint(&file(body));
        assert!(
            v.iter()
                .any(|x| x.message.contains("is a choice type and needs")),
            "got {v:?}"
        );
    }

    #[test]
    fn non_choice_custom_type_forbids_choices() {
        let body = "---
questionnaire:
  BEGIN:
    created: custom_datetime__formation_date
  custom_datetime__formation_date:
    answered: END
  END: {}
prompts:
  formation_date: When was the formation date?
choices:
  formation_date:
    today: Today
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---
";
        let v = F104FlowQuestionCodes::new(["custom_datetime"]).lint(&file(body));
        assert!(
            v.iter().any(|x| x.message.contains("must not define")),
            "got {v:?}"
        );
    }

    #[test]
    fn choice_custom_type_accepts_choices() {
        let body = "---
questionnaire:
  BEGIN:
    created: custom_single_choice__basis
  custom_single_choice__basis:
    answered: END
  END: {}
prompts:
  basis: Which basis applies?
choices:
  basis:
    a: Option A
    b: Option B
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---
";
        assert!(F104FlowQuestionCodes::new(["custom_single_choice"])
            .lint(&file(body))
            .is_empty());
    }

    #[test]
    fn bare_custom_question_state_is_invalid_without_a_prompt_discriminator() {
        let body = "---
questionnaire:
  BEGIN:
    created: custom_text
  custom_text:
    answered: END
  END: {}
prompts:
  custom_text: What custom text?
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---
";
        let v = F104FlowQuestionCodes::new(["custom_text"]).lint(&file(body));
        assert!(v
            .iter()
            .any(|x| x.message.contains("must use `custom_*__prompt_key`")));
    }

    #[test]
    fn workflow_states_are_validated_against_step_prefixes_not_question_codes() {
        let body = "---
title: T
questionnaire:
  BEGIN:
    created: trustee_name
  trustee_name:
    answered: END
  END: {}
workflow:
  BEGIN:
    created: generate_pdf__trust_pdf
  generate_pdf__trust_pdf:
    persisted: sent_for_signature__pending
  sent_for_signature__pending:
    signature_received: END
  END: {}
---
";
        assert!(rule().lint(&file(body)).is_empty());
    }

    #[test]
    fn workflow_signature_suffixes_are_known_steps() {
        let body = "---
title: T
questionnaire:
  BEGIN:
    created: trustee_name
  trustee_name:
    answered: END
  END: {}
workflow:
  BEGIN:
    created: member_signatures
  member_signatures:
    signed: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---
";
        assert!(rule().lint(&file(body)).is_empty());
    }

    #[test]
    fn flags_workflow_states_outside_the_step_catalog() {
        let body = "---
title: T
questionnaire:
  BEGIN:
    created: trustee_name
  trustee_name:
    answered: END
  END: {}
workflow:
  BEGIN:
    created: bespoke_magic
  bespoke_magic:
    done: END
  END: {}
---
";
        let v = rule().lint(&file(body));
        assert!(v.iter().any(|x| x
            .message
            .contains("Invalid workflow step prefix: `bespoke_magic`")));
    }

    #[test]
    fn retired_document_open_prefix_is_rejected() {
        // `document_open` is a retired prefix with no alias (the step is
        // `generate_pdf`). A template that authors it must fail N104, so no
        // notation reaches the engine with a step the dispatcher does not
        // know.
        assert!(!valid_workflow_step_prefix("document_open"));
        let body = "---
title: T
questionnaire:
  BEGIN:
    created: trustee_name
  trustee_name:
    answered: END
  END: {}
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: document_open__trust_pdf
  document_open__trust_pdf:
    persisted: END
  END: {}
---
";
        let v = rule().lint(&file(body));
        assert!(
            v.iter().any(|x| x
                .message
                .contains("Invalid workflow step prefix: `document_open`")),
            "got {v:?}"
        );
    }

    #[test]
    fn n104_accepts_every_engine_step_prefix() {
        // N104's allow-list is the workflow_steps catalog, which the
        // catalog's own drift test pins to workflows::step::STEP_PREFIXES.
        // This guards the N104 entry point specifically, including the
        // `_signature` suffix family.
        for (prefix, _) in workflows::step::STEP_PREFIXES {
            if *prefix == "_signature" {
                assert!(
                    valid_workflow_step_prefix("member_signatures"),
                    "signature suffix family should be accepted by N104",
                );
                continue;
            }
            assert!(
                valid_workflow_step_prefix(prefix),
                "workflow engine prefix `{prefix}` is not accepted by N104",
            );
        }
    }
}
