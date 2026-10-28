//! `N115` — template body data paths and iterators must resolve against
//! the questionnaire's typed states.
//!
//! Three body grammars are grounded here:
//!
//! - **Dotted data path** — `{{person__trustor.name}}` reads a field off a
//!   typed answer. The state before the dot must be a declared
//!   questionnaire state, and the field must belong to that type's shape (a
//!   custom primitive has no fields). Signature placeholders
//!   (`{{client.signature}}`) keep their `signer.field` shape and stay
//!   `N107`'s specialty — `N115` skips them.
//! - **Iterator** — `{{#for x in people__members}} … {{x.name}} … {{/for}}`
//!   walks an aggregate answer. The iterand must be a declared aggregate
//!   state; the loop variable's fields resolve against the aggregate's row
//!   shape; and every `#for` must close.
//! - **Conditional** — `{{#if custom_yes_no__approved}} … {{/if}}` and its
//!   `state=value` form name a declared state and must balance. Ordered
//!   `else if` alternatives name declared states; an optional `else` is a
//!   branch marker within the same conditional. Custom choice branches must
//!   cover every declared option or provide a fallback; unknown options refuse.
//!
//! Shares `f107`'s `{{ … }}` dotted-grammar scanning; the per-type shape is
//! the registry's (mirrored from `store::question_registry`).

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::{frontmatter, line_byte_range, Rule, SourceFile, Violation};

/// Signature field types — `N107`'s domain. A dotted path whose first
/// field is one of these is a signature placeholder, not questionnaire
/// data, regardless of the signer role.
const SIGNATURE_FIELDS: &[&str] = &["signature", "initials", "date"];

/// A person/`people` aggregate row's fields — the registry's
/// `PERSON_ROW_PARTS`.
const PERSON_FIELDS: &[&str] = &[
    "name", "email", "title", "phone", "street", "city", "state", "zip", "country",
];
/// An address row's fields.
const ADDRESS_FIELDS: &[&str] = &["street", "city", "state", "zip", "country"];
/// An entity's renderable field.
const ENTITY_FIELDS: &[&str] = &["name"];

/// The aggregate `<type>` tokens (mirror of
/// [`crate::AGGREGATE_QUESTION_TYPES`]) — the valid iterands of `#for`.
use crate::AGGREGATE_QUESTION_TYPES;

/// What fields a typed state exposes to a dotted path.
#[derive(Clone, Copy)]
enum Shape {
    /// A custom primitive: no dotted fields at all.
    Primitive,
    /// A known, closed field set.
    Fields(&'static [&'static str]),
    /// A type whose field set the lint does not enumerate — accept any
    /// field (the render evaluator resolves it at fill time).
    Any,
}

fn shape_for(type_token: &str) -> Shape {
    match type_token {
        "person" | "people" => Shape::Fields(PERSON_FIELDS),
        "address" | "addresses" => Shape::Fields(ADDRESS_FIELDS),
        "entity" | "entities" => Shape::Fields(ENTITY_FIELDS),
        t if t.starts_with("custom_") => Shape::Primitive,
        _ => Shape::Any,
    }
}

pub struct F115PathResolution;

impl F115PathResolution {
    pub const CODE: &'static str = "N115";
}

#[derive(Debug, Deserialize)]
struct FrontmatterShape {
    #[serde(default)]
    questionnaire: Option<BTreeMap<String, BTreeMap<String, String>>>,
    #[serde(default)]
    choices: BTreeMap<String, BTreeMap<String, String>>,
}

/// Coverage belongs to one balanced conditional, including its alternatives.
#[derive(Default)]
struct Conditional {
    fallback: bool,
    covered: BTreeMap<String, BTreeSet<String>>,
}

impl Conditional {
    fn record(
        &mut self,
        condition: &str,
        choices: &BTreeMap<String, BTreeMap<String, String>>,
        state_type: &BTreeMap<&str, &str>,
        file: &SourceFile,
        violations: &mut Vec<Violation>,
    ) {
        let (state, expected) = condition
            .split_once('=')
            .map_or((condition.trim(), None), |(state, value)| {
                (state.trim(), Some(value.trim()))
            });
        if !matches!(
            state_type.get(state),
            Some(&"custom_single_choice" | &"custom_multiple_choice")
        ) {
            return;
        }
        let Some((_, role)) = state.split_once("__") else {
            return;
        };
        let Some(options) = choices.get(role) else {
            return; // N104 requires the option declaration.
        };
        let covered = self.covered.entry(state.to_string()).or_default();
        if let Some(option) = expected {
            if options.contains_key(option) {
                covered.insert(option.to_string());
            } else {
                violations.push(violation(
                    file,
                    format!("Conditional state `{state}` names undeclared option `{option}`"),
                ));
            }
        } else {
            // The renderer treats these strings as false even when selected.
            covered.extend(
                options
                    .keys()
                    .filter(|option| {
                        !option.is_empty() && !matches!(option.as_str(), "false" | "no" | "0")
                    })
                    .cloned(),
            );
        }
    }

    fn finish(
        self,
        choices: &BTreeMap<String, BTreeMap<String, String>>,
        file: &SourceFile,
        violations: &mut Vec<Violation>,
    ) {
        if self.fallback {
            return;
        }
        for (state, covered) in self.covered {
            let Some(options) = state
                .split_once("__")
                .and_then(|(_, role)| choices.get(role))
            else {
                continue;
            };
            for option in options.keys().filter(|option| !covered.contains(*option)) {
                violations.push(violation(file, format!(
                    "Conditional state `{state}` is missing option `{option}`; add a branch or `{{{{else}}}}` fallback"
                )));
            }
        }
    }
}

/// One `{{ … }}` token: its inner trimmed text.
fn tokens(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            if let Some(rel) = body[i + 2..].find("}}") {
                let end = i + 2 + rel;
                out.push(body[i + 2..end].trim().to_string());
                i = end + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

impl Rule for F115PathResolution {
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
        let Some(questionnaire) = parsed.questionnaire else {
            let mut violations = Vec::new();
            check_declared_signer_backing(file, &file.contents, &BTreeMap::new(), &mut violations);
            return violations;
        };
        // state name → its `<type>` token.
        let state_type: BTreeMap<&str, &str> = questionnaire
            .keys()
            .filter(|s| s.as_str() != "BEGIN" && s.as_str() != "END")
            .map(|s| {
                (
                    s.as_str(),
                    s.split_once("__").map_or(s.as_str(), |(t, _)| t),
                )
            })
            .collect();

        // Only the body carries render placeholders; scan past frontmatter.
        let compiled =
            crate::clauses::compile(&file.contents).unwrap_or_else(|_| file.contents.clone());
        let body = frontmatter::split(&compiled).map_or(compiled.as_str(), |(_, body)| body);

        let mut violations = Vec::new();
        // Active `#for` loop bindings: var → aggregate `<type>` token.
        let mut loops: Vec<(String, &'static str)> = Vec::new();
        let mut conditionals = Vec::new();
        for tok in tokens(body) {
            if let Some(rest) = tok.strip_prefix("#for ") {
                handle_for(rest, &state_type, &mut loops, file, &mut violations);
            } else if tok == "/for" {
                if loops.pop().is_none() {
                    violations.push(violation(file, "`{{/for}}` without a matching `{{#for}}`"));
                }
            } else if let Some(rest) = tok.strip_prefix("#if ") {
                let mut conditional = Conditional::default();
                conditional.record(rest, &parsed.choices, &state_type, file, &mut violations);
                conditionals.push(conditional);
                let state = rest.split_once('=').map_or(rest, |(state, _)| state).trim();
                if let Some((head, tail)) = state.split_once('.') {
                    handle_path(head, tail, &state_type, &loops, file, &mut violations);
                } else if !state_type.contains_key(state) {
                    violations.push(violation(
                        file,
                        format!("Conditional `#if {rest}` names undeclared questionnaire state `{state}`"),
                    ));
                }
            } else if let Some(rest) = tok.strip_prefix("else if ") {
                if conditionals.last().is_none() {
                    violations.push(violation(
                        file,
                        "`{{else if}}` without a matching `{{#if}}`",
                    ));
                    continue;
                }
                if conditionals
                    .last()
                    .is_some_and(|conditional| conditional.fallback)
                {
                    violations.push(violation(file, "`{{else if}}` cannot follow `{{else}}`"));
                }
                if let Some(conditional) = conditionals.last_mut() {
                    conditional.record(rest, &parsed.choices, &state_type, file, &mut violations);
                }
                let state = rest.split_once('=').map_or(rest, |(state, _)| state).trim();
                if let Some((head, tail)) = state.split_once('.') {
                    handle_path(head, tail, &state_type, &loops, file, &mut violations);
                } else if !state_type.contains_key(state) {
                    violations.push(violation(
                        file,
                        format!("Conditional `else if {rest}` names undeclared questionnaire state `{state}`"),
                    ));
                }
            } else if tok == "else" {
                match conditionals.last_mut() {
                    Some(conditional) if !conditional.fallback => conditional.fallback = true,
                    Some(_) => violations.push(violation(file, "duplicate `{{else}}` branch")),
                    None => {
                        violations.push(violation(file, "`{{else}}` without a matching `{{#if}}`"));
                    }
                }
            } else if tok == "/if" {
                if let Some(conditional) = conditionals.pop() {
                    conditional.finish(&parsed.choices, file, &mut violations);
                } else {
                    violations.push(violation(file, "`{{/if}}` without a matching `{{#if}}`"));
                }
            } else if let Some((head, tail)) = tok.split_once('.') {
                handle_path(head, tail, &state_type, &loops, file, &mut violations);
            }
        }
        if !loops.is_empty() {
            violations.push(violation(file, "`{{#for}}` is not closed by `{{/for}}`"));
        }
        if !conditionals.is_empty() {
            violations.push(violation(file, "`{{#if}}` is not closed by `{{/if}}`"));
        }
        check_declared_signer_backing(file, &file.contents, &state_type, &mut violations);
        violations
    }
}

fn handle_for(
    rest: &str,
    state_type: &BTreeMap<&str, &str>,
    loops: &mut Vec<(String, &'static str)>,
    file: &SourceFile,
    violations: &mut Vec<Violation>,
) {
    let Some((var, state)) = rest.split_once(" in ") else {
        violations.push(violation(
            file,
            format!("Malformed iterator `{{{{#for {rest}}}}}` — expected `#for <var> in <state>`"),
        ));
        return;
    };
    let (var, state) = (var.trim(), state.trim());
    let Some(&ty) = state_type.get(state) else {
        violations.push(violation(
            file,
            format!(
                "Iterator `#for {var} in {state}` names undeclared questionnaire state `{state}`"
            ),
        ));
        loops.push((var.to_string(), "")); // still track so `/for` balances
        return;
    };
    // Ground the aggregate token to a static str for the binding.
    let Some(agg) = AGGREGATE_QUESTION_TYPES.iter().find(|a| **a == ty) else {
        violations.push(violation(
            file,
            format!("Iterator `#for {var} in {state}` is not an aggregate (`{ty}` is singular)"),
        ));
        loops.push((var.to_string(), ""));
        return;
    };
    loops.push((var.to_string(), agg));
}

fn handle_path(
    head: &str,
    tail: &str,
    state_type: &BTreeMap<&str, &str>,
    loops: &[(String, &'static str)],
    file: &SourceFile,
    violations: &mut Vec<Violation>,
) {
    // Signature blocks (`client.signature`) are N107's domain.
    let field = tail.split('.').next().unwrap_or(tail);
    if SIGNATURE_FIELDS.contains(&field) {
        return;
    }
    // A loop variable resolves against its aggregate row shape.
    if let Some((_, agg)) = loops.iter().find(|(v, _)| v == head) {
        if agg.is_empty() {
            return; // the `#for` was already flagged; don't double-report
        }
        check_field(shape_for(agg), field, head, file, violations);
        return;
    }
    // Otherwise `head` must be a declared questionnaire state.
    let Some(&ty) = state_type.get(head) else {
        violations.push(violation(
            file,
            format!(
                "Data path `{{{{{head}.{tail}}}}}` names undeclared questionnaire state `{head}`"
            ),
        ));
        return;
    };
    check_field(shape_for(ty), field, head, file, violations);
}

fn check_declared_signer_backing(
    file: &SourceFile,
    contents: &str,
    state_type: &BTreeMap<&str, &str>,
    violations: &mut Vec<Violation>,
) {
    let Ok(set) = crate::f107::signer_set(contents) else {
        return;
    };
    if !set.explicit {
        return;
    }
    for role in &set.roles {
        if role == "firm" {
            // The firm countersigns from the configured signer, not a
            // questionnaire state.
            continue;
        }
        let state = format!("person__{role}");
        let Some(&ty) = state_type.get(state.as_str()) else {
            violations.push(violation(
                file,
                format!(
                    "declared signer role `{role}` has no questionnaire state \
                     `{state}` carrying at least a name and an email"
                ),
            ));
            continue;
        };
        if ty != "person" {
            violations.push(violation(
                file,
                format!(
                    "declared signer role `{role}` must be backed by a `person` \
                     state (`{state}`) carrying at least a name and an email"
                ),
            ));
        }
    }
}

fn check_field(
    shape: Shape,
    field: &str,
    owner: &str,
    file: &SourceFile,
    violations: &mut Vec<Violation>,
) {
    match shape {
        Shape::Primitive => violations.push(violation(
            file,
            format!("`{owner}` is a custom primitive and has no dotted fields (`.{field}`)"),
        )),
        Shape::Fields(fields) if !fields.contains(&field) => violations.push(violation(
            file,
            format!("`{owner}` has no field `{field}` (expected one of {fields:?})"),
        )),
        _ => {}
    }
}

fn violation(file: &SourceFile, message: impl Into<String>) -> Violation {
    Violation {
        code: F115PathResolution::CODE,
        path: file.path.clone(),
        line: 1,
        range: line_byte_range(&file.contents, 1),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::F115PathResolution;
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    fn file(body: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.md"),
            contents: body.to_string(),
        }
    }

    fn tmpl(states: &str, body: &str) -> String {
        format!("---\nquestionnaire:\n{states}---\n\n{body}\n")
    }

    #[test]
    fn resolves_a_valid_dotted_person_field() {
        let body = tmpl(
            "  BEGIN:\n    _: person__trustor\n  person__trustor:\n    _: END\n  END: {}\n",
            "The trustor is {{person__trustor.name}}.",
        );
        assert!(F115PathResolution.lint(&file(&body)).is_empty());
    }

    #[test]
    fn grounds_and_balances_conditional_states() {
        let valid = tmpl(
            "  BEGIN:\n    _: custom_yes_no__approved\n  custom_yes_no__approved:\n    _: END\n  END: {}\n",
            "{{#if custom_yes_no__approved}}Approved.{{/if}}",
        );
        assert!(F115PathResolution.lint(&file(&valid)).is_empty());

        let undeclared = tmpl(
            "  BEGIN:\n    _: custom_yes_no__approved\n  custom_yes_no__approved:\n    _: END\n  END: {}\n",
            "{{#if custom_yes_no__missing}}Missing.{{/if}}",
        );
        assert!(F115PathResolution
            .lint(&file(&undeclared))
            .iter()
            .any(|violation| violation.message.contains("undeclared")));

        let unclosed = tmpl(
            "  BEGIN:\n    _: custom_yes_no__approved\n  custom_yes_no__approved:\n    _: END\n  END: {}\n",
            "{{#if custom_yes_no__approved}}Approved.",
        );
        assert!(F115PathResolution
            .lint(&file(&unclosed))
            .iter()
            .any(|violation| violation.message.contains("not closed")));
    }

    #[test]
    fn grounds_else_if_states_and_balances_branch_markers() {
        let valid = tmpl(
            "  BEGIN:\n    _: jurisdiction__law\n  jurisdiction__law:\n    _: END\n  END: {}\n",
            "{{#if jurisdiction__law=California}}CA{{else if jurisdiction__law=Nevada}}NV{{else}}Other{{/if}}",
        );
        assert!(F115PathResolution.lint(&file(&valid)).is_empty());

        let undeclared = valid.replace("jurisdiction__law=Nevada", "jurisdiction__missing=Nevada");
        assert!(F115PathResolution
            .lint(&file(&undeclared))
            .iter()
            .any(|violation| violation.message.contains("else if")
                && violation.message.contains("undeclared")));

        let duplicate_else = valid.replace(
            "{{else}}Other{{/if}}",
            "{{else}}Other{{else}}Unexpected{{/if}}",
        );
        assert!(F115PathResolution
            .lint(&file(&duplicate_else))
            .iter()
            .any(|violation| violation.message.contains("duplicate")));

        let else_without_if = valid.replace(
            "{{#if jurisdiction__law=California}}CA{{else if jurisdiction__law=Nevada}}NV{{else}}Other{{/if}}",
            "{{else}}Orphan{{/if}}",
        );
        assert!(F115PathResolution
            .lint(&file(&else_without_if))
            .iter()
            .any(|violation| violation.message.contains("without a matching")));

        let else_if_after_else = valid.replace(
            "{{else}}Other{{/if}}",
            "{{else}}Other{{else if jurisdiction__law=California}}Late{{/if}}",
        );
        assert!(F115PathResolution
            .lint(&file(&else_if_after_else))
            .iter()
            .any(|violation| violation.message.contains("cannot follow `{{else}}`")));
    }

    fn choice_template(body: &str) -> String {
        format!(
            "---\nquestionnaire:\n  BEGIN: {{ _: custom_single_choice__delivery }}\n  custom_single_choice__delivery: {{ _: END }}\n  END: {{}}\nchoices:\n  delivery:\n    email: By email\n    post: By post\n---\n{body}\n"
        )
    }

    #[test]
    fn choice_conditional_requires_every_option_or_fallback() {
        let source = choice_template("{{#if custom_single_choice__delivery=email}}Email{{/if}}");
        let findings = F115PathResolution.lint(&file(&source));
        assert!(findings.iter().any(|finding| finding.code == "N115"
            && finding.message.contains("custom_single_choice__delivery")
            && finding.message.contains("post")));
        for body in [
            "{{#if custom_single_choice__delivery=email}}Email{{else if custom_single_choice__delivery=post}}Post{{/if}}",
            "{{#if custom_single_choice__delivery=email}}Email{{else}}Other{{/if}}",
            "{{#if custom_single_choice__delivery}}Any delivery{{/if}}",
        ] {
            assert!(F115PathResolution.lint(&file(&choice_template(body))).is_empty(), "{body}");
        }
    }

    #[test]
    fn choice_conditional_rejects_dead_options_even_with_fallback() {
        let source = choice_template(
            "{{#if custom_single_choice__delivery=emali}}Email{{else}}Other{{/if}}",
        );
        let findings = F115PathResolution.lint(&file(&source));
        assert!(findings.iter().any(|finding| finding
            .message
            .contains("custom_single_choice__delivery")
            && finding.message.contains("emali")
            && finding.message.contains("undeclared option")));
    }

    #[test]
    fn nested_and_separate_conditionals_do_not_share_coverage() {
        for body in [
            "{{#if custom_single_choice__delivery=email}}{{#if custom_single_choice__delivery=post}}Post{{else}}Other{{/if}}{{/if}}",
            "{{#if custom_single_choice__delivery=email}}Email{{/if}}{{#if custom_single_choice__delivery=post}}Post{{/if}}",
        ] {
            let findings = F115PathResolution.lint(&file(&choice_template(body)));
            assert!(!findings.is_empty(), "{body}");
        }
    }

    #[test]
    fn choice_values_follow_render_whitespace() {
        let source = choice_template("{{#if custom_single_choice__delivery = email}}Email{{else if custom_single_choice__delivery=post}}Post{{/if}}");
        assert!(F115PathResolution.lint(&file(&source)).is_empty());
    }

    #[test]
    fn truthy_choice_condition_leaves_false_values_unhandled() {
        let source = choice_template("{{#if custom_single_choice__delivery}}Delivery{{/if}}")
            .replace("    post: By post", "    'no': No delivery");
        assert!(F115PathResolution
            .lint(&file(&source))
            .iter()
            .any(|finding| finding.message.contains("no")
                && finding.message.contains("missing option")));
    }

    #[test]
    fn else_if_dead_options_are_checked_and_unrelated_else_is_not_a_fallback() {
        let source = choice_template("{{#if custom_single_choice__delivery=email}}Email{{else if custom_single_choice__delivery=poste}}Post{{else}}Other{{/if}}");
        assert!(F115PathResolution
            .lint(&file(&source))
            .iter()
            .any(|finding| finding.message.contains("poste")
                && finding.message.contains("undeclared option")));
        let source = choice_template("{{#if custom_single_choice__delivery=email}}{{#if custom_single_choice__delivery}}Any{{else}}None{{/if}}{{/if}}");
        assert!(F115PathResolution
            .lint(&file(&source))
            .iter()
            .any(|finding| finding.message.contains("post")
                && finding.message.contains("missing option")));
    }

    #[test]
    fn multiple_choice_branches_use_the_same_declared_option_checks() {
        let source = choice_template("{{#if custom_single_choice__delivery=email}}Email{{/if}}")
            .replace("custom_single_choice", "custom_multiple_choice");
        assert!(F115PathResolution
            .lint(&file(&source))
            .iter()
            .any(
                |finding| finding.message.contains("custom_multiple_choice__delivery")
                    && finding.message.contains("missing option `post`")
            ));
    }

    #[test]
    fn quoted_comparisons_are_literal_values_like_the_renderer() {
        let source = choice_template(
            "{{#if custom_single_choice__delivery='email'}}Email{{else}}Other{{/if}}",
        );
        assert!(F115PathResolution
            .lint(&file(&source))
            .iter()
            .any(|finding| finding.message.contains("undeclared option `'email'`")));
    }

    #[test]
    fn skips_signature_blocks() {
        let body = tmpl(
            "  BEGIN:\n    _: person__trustor\n  person__trustor:\n    _: END\n  END: {}\n",
            "Sign here: {{client.signature}} {{firm.date}}",
        );
        assert!(F115PathResolution.lint(&file(&body)).is_empty());
    }

    #[test]
    fn flags_an_unknown_person_field() {
        let body = tmpl(
            "  BEGIN:\n    _: person__trustor\n  person__trustor:\n    _: END\n  END: {}\n",
            "{{person__trustor.middle_initial}}",
        );
        let v = F115PathResolution.lint(&file(&body));
        assert!(
            v.iter()
                .any(|x| x.message.contains("no field `middle_initial`")),
            "{v:?}"
        );
    }

    #[test]
    fn flags_a_dotted_path_on_a_custom_primitive() {
        let body = tmpl(
            "  BEGIN:\n    _: custom_text__mission\n  custom_text__mission:\n    _: END\n  END: {}\n",
            "{{custom_text__mission.value}}",
        );
        let v = F115PathResolution.lint(&file(&body));
        assert!(
            v.iter().any(|x| x.message.contains("custom primitive")),
            "{v:?}"
        );
    }

    #[test]
    fn flags_a_path_to_an_undeclared_state() {
        let body = tmpl(
            "  BEGIN:\n    _: person__trustor\n  person__trustor:\n    _: END\n  END: {}\n",
            "{{person__grantor.name}}",
        );
        let v = F115PathResolution.lint(&file(&body));
        assert!(
            v.iter().any(|x| x
                .message
                .contains("undeclared questionnaire state `person__grantor`")),
            "{v:?}"
        );
    }

    #[test]
    fn resolves_a_for_loop_over_an_aggregate() {
        let body = tmpl(
            "  BEGIN:\n    _: people__members\n  people__members:\n    _: END\n  END: {}\n",
            "{{#for m in people__members}}{{m.name}}, {{m.city}}\n{{/for}}",
        );
        assert!(F115PathResolution.lint(&file(&body)).is_empty());
    }

    #[test]
    fn flags_a_for_over_a_singular_state() {
        let body = tmpl(
            "  BEGIN:\n    _: person__trustor\n  person__trustor:\n    _: END\n  END: {}\n",
            "{{#for p in person__trustor}}{{p.name}}{{/for}}",
        );
        let v = F115PathResolution.lint(&file(&body));
        assert!(
            v.iter().any(|x| x.message.contains("is not an aggregate")),
            "{v:?}"
        );
    }

    #[test]
    fn flags_an_unclosed_for() {
        let body = tmpl(
            "  BEGIN:\n    _: people__members\n  people__members:\n    _: END\n  END: {}\n",
            "{{#for m in people__members}}{{m.name}}",
        );
        let v = F115PathResolution.lint(&file(&body));
        assert!(v.iter().any(|x| x.message.contains("not closed")), "{v:?}");
    }

    #[test]
    fn flags_a_bad_loop_var_field() {
        let body = tmpl(
            "  BEGIN:\n    _: people__members\n  people__members:\n    _: END\n  END: {}\n",
            "{{#for m in people__members}}{{m.ssn}}{{/for}}",
        );
        let v = F115PathResolution.lint(&file(&body));
        assert!(
            v.iter().any(|x| x.message.contains("no field `ssn`")),
            "{v:?}"
        );
    }

    #[test]
    fn no_frontmatter_means_no_violation() {
        assert!(F115PathResolution
            .lint(&file("{{a.b}} just body"))
            .is_empty());
    }

    fn three_party(signers: &str, questionnaire: &str) -> String {
        format!(
            "---\ntitle: Assignment\n{signers}questionnaire:\n{questionnaire}workflow:\n  BEGIN:\n    \
             created: sent_for_signature__pending\n  sent_for_signature__pending:\n    \
             signature_received: END\n  END: {{}}\n---\n\
             {{{{client.signature}}}}\n{{{{firm.signature}}}}\n{{{{confirming_assignor.signature}}}}\n"
        )
    }

    #[test]
    fn declared_three_party_roles_resolve_against_person_states() {
        let body = three_party(
            "signers:\n  - client\n  - firm\n  - confirming_assignor\n",
            "  BEGIN:\n    _: person__client\n  person__client:\n    _: person__confirming_assignor\n  \
             person__confirming_assignor:\n    _: END\n  END: {}\n",
        );
        assert!(
            F115PathResolution.lint(&file(&body)).is_empty(),
            "client and confirming_assignor are person states with name and email: {:?}",
            F115PathResolution.lint(&file(&body)),
        );
    }

    #[test]
    fn declared_role_without_questionnaire_state_fails() {
        let body = three_party(
            "signers:\n  - client\n  - firm\n  - confirming_assignor\n",
            "  BEGIN:\n    _: person__client\n  person__client:\n    _: END\n  END: {}\n",
        );
        let v = F115PathResolution.lint(&file(&body));
        assert!(
            v.iter().any(|x| {
                x.code == "N115"
                    && x.message.contains("confirming_assignor")
                    && (x.message.contains("questionnaire")
                        || x.message.contains("name and an email"))
            }),
            "a declared role with no person state must fail N115; got {v:?}"
        );
    }

    #[test]
    fn shipped_onboarding_letter_needs_no_signers_key() {
        let body = include_str!("../../templates/notations/neon_law/onboarding__letter.md");
        assert!(
            F115PathResolution.lint(&file(body)).is_empty(),
            "{:?}",
            F115PathResolution.lint(&file(body)),
        );
    }

    #[test]
    fn shipped_offboarding_letter_needs_no_signers_key() {
        let body = include_str!("../../templates/notations/neon_law/offboarding__letter.md");
        assert!(
            F115PathResolution.lint(&file(body)).is_empty(),
            "{:?}",
            F115PathResolution.lint(&file(body)),
        );
    }
}
