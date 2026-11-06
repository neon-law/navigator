//! `N120` — a template body's undotted typed placeholders must be declared
//! questionnaire states collected on every route that renders the read.
//!
//! [`crate::F115PathResolution`] grounds the *dotted* half of the body
//! grammar: `{{person__trustor.name}}` reads a field off a typed answer, so
//! the state before the dot must be declared. This rule grounds the other
//! half — the bare `{{<type>__<role>}}` token that substitutes a whole
//! answer, with no field access.
//!
//! The two are the same claim about the same grammar. A body token written
//! in the typed-state form asserts that the questionnaire collects that
//! state; when it does not, nothing fills the token and it reaches the page
//! verbatim. The Markdown-to-Typst conversion marks such a token with a
//! highlight so a reader can see the blank, but a highlight is a signal to a
//! human, not a guarantee — the fix belongs at authoring time, where the
//! template and its questionnaire sit in the same file and can be compared.
//!
//! Scope, deliberately narrow so the rule cannot fight the other grammars:
//!
//! - **Dotted paths** (`{{person__client.name}}`) are `N115`'s, skipped here.
//! - **Signature placeholders** (`{{client.signature}}`) are `N107`'s; they
//!   are dotted, so the same skip covers them.
//! - **Iterators** (`{{#for x in people__members}}`, `{{/for}}`) are `N115`'s.
//! - **Conditionals** (`{{#if custom_yes_no__approved}}`, `{{/if}}`) are
//!   `N115`'s.
//! - **Untyped tokens** (`{{custom_clauses}}`, `{{for_label}}`) carry no
//!   `__` discriminator, are filled by mechanisms outside the questionnaire,
//!   and are not checked.
//!
//! What remains is exactly the token that claims to be a typed state, and
//! the frontmatter beside it says whether that claim holds.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{frontmatter, line_byte_range, Rule, SourceFile, Violation};

pub struct F120BodyStateGrounding;

impl F120BodyStateGrounding {
    pub const CODE: &'static str = "N120";
}

#[derive(Debug, Deserialize)]
struct FrontmatterShape {
    #[serde(default)]
    questionnaire: Option<BTreeMap<String, BTreeMap<String, String>>>,
}

/// Every `{{ … }}` token in `body`, as `(inner trimmed text, 1-based line
/// within `body`)`.
fn tokens_with_lines(body: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (idx, line) in body.lines().enumerate() {
        let mut rest = line;
        while let Some(start) = rest.find("{{") {
            let Some(end_rel) = rest[start + 2..].find("}}") else {
                break;
            };
            let end = start + 2 + end_rel;
            out.push((rest[start + 2..end].trim().to_string(), idx + 1));
            rest = &rest[end + 2..];
        }
    }
    out
}

/// The byte offset at which the body begins — everything after the
/// frontmatter's closing `---`. Mirrors `F115PathResolution`'s split so both
/// rules read the same region, and lets a body line map back to a file line.
fn body_offset(contents: &str) -> Option<usize> {
    let open = contents.strip_prefix("---")?;
    let close_rel = open.find("\n---")?;
    let after = open[close_rel + 4..].find('\n')?;
    Some(3 + close_rel + 4 + after + 1)
}

/// Translate a 1-based line within the body to a 1-based line within the file.
fn file_line(contents: &str, body_line: usize) -> usize {
    let Some(offset) = body_offset(contents) else {
        return 1;
    };
    let preceding = contents[..offset].lines().count();
    preceding + body_line
}

impl Rule for F120BodyStateGrounding {
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
            return Vec::new();
        };
        let declared: Vec<&str> = questionnaire
            .keys()
            .map(String::as_str)
            .filter(|s| *s != "BEGIN" && *s != "END")
            .collect();

        let Some(offset) = body_offset(&file.contents) else {
            return Vec::new();
        };
        let body = &file.contents[offset..];

        let mut violations = Vec::new();
        let mut reported: Vec<String> = Vec::new();
        for (tok, body_line) in tokens_with_lines(body) {
            // `N115` owns dotted paths and both iterator forms.
            if tok.contains('.')
                || tok.starts_with("#for ")
                || tok == "/for"
                || tok.starts_with("#if ")
                || tok.starts_with("else if ")
                || tok == "else"
                || tok == "/if"
            {
                continue;
            }
            // Only the typed-state grammar makes a claim this rule can check.
            if !tok.contains("__") {
                continue;
            }
            if declared.contains(&tok.as_str()) || reported.contains(&tok) {
                continue;
            }
            reported.push(tok.clone());
            let line = file_line(&file.contents, body_line);
            violations.push(Violation {
                code: Self::CODE,
                path: file.path.clone(),
                line,
                range: line_byte_range(&file.contents, line),
                message: format!(
                    "Placeholder `{{{{{tok}}}}}` names undeclared questionnaire state `{tok}`; \
                     nothing fills it, so the token renders verbatim in the document"
                ),
            });
        }
        violations.extend(crate::route_safety::findings(file, Self::CODE));
        violations
    }
}

#[cfg(test)]
mod tests {
    use super::F120BodyStateGrounding;
    use crate::{Rule, SourceFile};
    use std::{fmt::Write, path::PathBuf};

    fn file(contents: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.md"),
            contents: contents.to_string(),
        }
    }

    /// A questionnaire declaring `states`, then `body`.
    fn template(states: &[&str], body: &str) -> String {
        let first = states.first().copied().unwrap_or("END");
        let mut lines = vec![
            "---".to_string(),
            "kind: onboarding".to_string(),
            "title: T".to_string(),
            "questionnaire:".to_string(),
            "  BEGIN:".to_string(),
            format!("    _: {first}"),
        ];
        for (i, s) in states.iter().enumerate() {
            let next = states.get(i + 1).copied().unwrap_or("END");
            lines.push(format!("  {s}:"));
            lines.push(format!("    _: {next}"));
        }
        lines.push("  END: {}".to_string());
        lines.push("---".to_string());
        lines.push(String::new());
        format!("{}\n{body}", lines.join("\n"))
    }

    fn branched(body: &str) -> SourceFile {
        file(&format!("---\nkind: onboarding\ntitle: T\nchoices:\n  path:\n    a: A\n    b: B\nquestionnaire:\n  BEGIN: {{_: custom_single_choice__path}}\n  custom_single_choice__path: {{a: person__a, b: custom_text__b}}\n  person__a: {{_: END}}\n  custom_text__b: {{_: END}}\n  END: {{}}\n---\n{body}\n"))
    }

    #[test]
    fn an_unconditional_placeholder_must_be_collected_on_every_route() {
        let findings = F120BodyStateGrounding.lint(&branched("{{custom_text__b}}"));
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].message.contains("route"));
    }

    #[test]
    fn conditional_placeholders_follow_their_selected_routes() {
        let safe = "{{#if custom_single_choice__path=a}}{{person__a.name}}{{else}}{{custom_text__b}}{{/if}}";
        assert!(F120BodyStateGrounding.lint(&branched(safe)).is_empty());
        let unsafe_body = "{{#if custom_single_choice__path=a}}{{custom_text__b}}{{else}}B{{/if}}";
        assert_eq!(F120BodyStateGrounding.lint(&branched(unsafe_body)).len(), 1);
    }

    #[test]
    fn nested_alternatives_and_dotted_reads_are_route_sensitive() {
        use crate::F115PathResolution;
        for (body, expected) in [
            ("{{person__a.name}}", 1),
            ("{{#if custom_single_choice__path=a}}{{person__a.name}}{{else}}{{custom_text__b}}{{/if}}", 0),
            ("{{#if custom_single_choice__path=a}}{{#if custom_single_choice__path=b}}{{custom_text__b}}{{/if}}{{else}}B{{/if}}", 0),
            ("{{#if custom_single_choice__path=a}}A{{else if custom_single_choice__path=b}}{{person__a.name}}{{else}}Other{{/if}}", 1),
            ("{{#if custom_single_choice__path=a}}A{{else}}{{#if custom_text__b}}{{custom_text__b}}{{/if}}{{/if}}", 0),
            ("{{#if custom_text__b=}}{{person__a.name}}{{/if}}", 1),
        ] {
            let source = branched(body);
            let actual = F120BodyStateGrounding.lint(&source).into_iter()
                .chain(F115PathResolution.lint(&source))
                .filter(|finding| finding.message.contains("questionnaire route"))
                .count();
            assert_eq!(actual, expected, "{body}");
        }
    }

    #[test]
    fn fallback_and_shared_questions_preserve_safe_reads() {
        let source = branched("{{#if custom_single_choice__path=a}}{{person__a.name}}{{else}}{{custom_text__b}}{{/if}}");
        let fallback = file(&source.contents.replace(
            "a: person__a, b: custom_text__b",
            "a: person__a, _: custom_text__b",
        ));
        assert!(F120BodyStateGrounding.lint(&fallback).is_empty());
        let shared = file(
            &branched("{{custom_text__b}}")
                .contents
                .replace("person__a: {_: END}", "person__a: {_: custom_text__b}"),
        );
        assert!(F120BodyStateGrounding.lint(&shared).is_empty());
    }

    #[test]
    fn numeric_choice_keys_follow_the_authored_route() {
        let source =
            branched("{{#if custom_single_choice__path=1}}{{custom_text__b}}{{else}}Other{{/if}}");
        let numeric = file(
            &source
                .contents
                .replace("a: A", "\"1\": A")
                .replace("a: person__a", "\"1\": person__a"),
        );
        assert_eq!(F120BodyStateGrounding.lint(&numeric).len(), 1);
    }

    #[test]
    fn converging_routes_do_not_enumerate_every_combination() {
        let count = 28;
        let mut source = String::from("---\nkind: onboarding\ntitle: T\nchoices:\n  initial: {include: Include, skip: Skip}\n");
        for i in 0..count {
            writeln!(source, "  path_{i}: {{a: A, b: B}}").unwrap();
        }
        source.push_str("questionnaire:\n  BEGIN: {_: custom_single_choice__initial}\n  custom_single_choice__initial: {include: custom_text__read, skip: custom_single_choice__path_0}\n  custom_text__read: {_: custom_single_choice__path_0}\n");
        for i in 0..count {
            let next = if i + 1 == count {
                "END".into()
            } else {
                format!("custom_single_choice__path_{}", i + 1)
            };
            writeln!(source, "  custom_single_choice__path_{i}: {{a: custom_text__guard_{i}, b: {next}}}\n  custom_text__guard_{i}: {{_: {next}}}").unwrap();
        }
        source.push_str("  END: {}\n---\n");
        for i in 0..count {
            write!(source, "{{{{#if custom_text__guard_{i}=yes}}}}").unwrap();
        }
        source.push_str("{{custom_text__read}}");
        source.push_str(&"{{/if}}".repeat(count));
        assert_eq!(F120BodyStateGrounding.lint(&file(&source)).len(), 1);
    }

    #[test]
    fn reusable_clause_reads_obey_the_enclosing_route_condition() {
        use crate::F115PathResolution;
        let source = file(include_str!(
            "../../workflows/tests/fixtures/onboarding_branched.txt"
        ));
        let route_findings = |source: &SourceFile| {
            F115PathResolution
                .lint(source)
                .into_iter()
                .chain(F120BodyStateGrounding.lint(source))
                .filter(|finding| finding.message.contains("questionnaire route"))
                .count()
        };
        assert_eq!(route_findings(&source), 0);
        let unguarded = file(
            &source
                .contents
                .replace("{{#if custom_single_choice__path=transactional}}", "")
                .replace("{{else}}", "")
                .replace("{{/if}}", ""),
        );
        assert_eq!(route_findings(&unguarded), 2);
    }

    #[test]
    fn invalid_routes_are_refused_by_the_route_gate() {
        for source in [
            branched("{{custom_text__b}}")
                .contents
                .replace("person__a: {_: END}", "person__a: {_: person__a}"),
            branched("{{custom_text__b}}").contents.replace(
                "a: person__a, b: custom_text__b",
                "a: person__a, b: person__a",
            ),
        ] {
            let source = file(&source);
            assert!(crate::engine::blocking_violations_without_checkout(&source)
                .iter()
                .any(|finding| finding.code == "N118"));
            assert!(F120BodyStateGrounding.lint(&source).is_empty());
        }
    }

    #[test]
    fn a_declared_state_placeholder_passes() {
        let f = file(&template(
            &["custom_text__fee_basis"],
            "Fees:\n\n{{custom_text__fee_basis}}\n",
        ));
        assert!(F120BodyStateGrounding.lint(&f).is_empty());
    }

    #[test]
    fn an_undeclared_state_placeholder_is_flagged() {
        let f = file(&template(
            &["person__client"],
            "Fees:\n\n{{custom_text__fee_basis}}\n",
        ));
        let v = F120BodyStateGrounding.lint(&f);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].code, "N120");
        assert!(v[0].message.contains("custom_text__fee_basis"), "{v:?}");
    }

    #[test]
    fn the_violation_points_at_the_body_line_carrying_the_token() {
        let contents = template(&["person__client"], "One\n\nTwo\n\n{{custom_text__x}}\n");
        let f = file(&contents);
        let v = F120BodyStateGrounding.lint(&f);
        assert_eq!(v.len(), 1, "{v:?}");
        let line = contents
            .lines()
            .position(|l| l.contains("{{custom_text__x}}"))
            .expect("the token is in the fixture")
            + 1;
        assert_eq!(v[0].line, line, "{v:?}");
    }

    #[test]
    fn a_dotted_path_is_left_to_n115() {
        let f = file(&template(
            &["person__client"],
            "{{person__undeclared.name}}\n",
        ));
        assert!(
            F120BodyStateGrounding.lint(&f).is_empty(),
            "dotted paths belong to N115"
        );
    }

    #[test]
    fn a_signature_placeholder_is_left_to_n107() {
        let f = file(&template(&["person__client"], "{{client.signature}}\n"));
        assert!(F120BodyStateGrounding.lint(&f).is_empty());
    }

    #[test]
    fn iterator_tokens_are_left_to_n115() {
        let f = file(&template(
            &["people__members"],
            "{{#for m in people__members}}{{m.name}}{{/for}}\n",
        ));
        assert!(F120BodyStateGrounding.lint(&f).is_empty());
    }

    #[test]
    fn an_untyped_token_is_not_checked() {
        // `custom_clauses` is spliced by the notation-clause mechanism, not
        // the questionnaire, and carries no `__` discriminator.
        let f = file(&template(&["person__client"], "{{custom_clauses}}\n"));
        assert!(F120BodyStateGrounding.lint(&f).is_empty());
    }

    #[test]
    fn a_repeated_undeclared_token_is_reported_once() {
        let f = file(&template(
            &["person__client"],
            "{{custom_text__x}}\n\n{{custom_text__x}}\n",
        ));
        assert_eq!(F120BodyStateGrounding.lint(&f).len(), 1);
    }

    #[test]
    fn a_file_without_frontmatter_is_skipped() {
        assert!(F120BodyStateGrounding
            .lint(&file("Just prose {{custom_text__x}}.\n"))
            .is_empty());
    }

    #[test]
    fn a_frontmatter_without_a_questionnaire_is_skipped() {
        assert!(F120BodyStateGrounding
            .lint(&file(
                "---\nkind: onboarding\ntitle: T\n---\n\n{{custom_text__x}}\n"
            ))
            .is_empty());
    }

    #[test]
    fn a_placeholder_in_the_frontmatter_is_not_a_body_placeholder() {
        // A prompt may quote a token; only the body renders.
        let contents = "---\nkind: onboarding\ntitle: T\nprompts:\n  x: \
                        \"say {{custom_text__x}}\"\nquestionnaire:\n  BEGIN:\n    _: END\n  \
                        END: {}\n---\n\nBody.\n";
        assert!(F120BodyStateGrounding.lint(&file(contents)).is_empty());
    }
}
