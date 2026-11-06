//! The authoring gate and document renderer must agree about which branches
//! expose a missing answer, including empty and absent condition values.

use std::{collections::BTreeMap, path::PathBuf};

use rules::{F120BodyStateGrounding, Rule, SourceFile};

#[test]
fn route_refusals_match_the_renderers_missing_answer_behavior() {
    for body in [
        "{{custom_text__a}}",
        "{{#if custom_single_choice__path=a}}{{custom_text__a}}{{else}}{{custom_text__b}}{{/if}}",
        "{{#if custom_single_choice__path=a}}{{custom_text__b}}{{else}}Other{{/if}}",
        "{{#if custom_text__a}}{{custom_text__a}}{{/if}}",
        "{{#if custom_text__a}}{{custom_text__b}}{{/if}}",
        "{{#if custom_text__a=}}{{custom_text__b}}{{/if}}",
        "{{#if custom_text__a=}}Empty{{else}}{{custom_text__a}}{{/if}}",
        "{{#if custom_text__a=false}}{{custom_text__b}}{{/if}}",
        "{{#if custom_text__a=no}}{{custom_text__b}}{{/if}}",
        "{{#if custom_text__a=0}}{{custom_text__b}}{{/if}}",
        "{{#if custom_text__a=arbitrary}}{{custom_text__b}}{{/if}}",
        "{{#if custom_single_choice__path=a}}{{#if custom_text__a}}{{custom_text__a}}{{else}}Blank{{/if}}{{else}}{{custom_text__b}}{{/if}}",
        "{{#if custom_single_choice__path=a}}A{{else if custom_single_choice__path=b}}{{custom_text__b}}{{else}}{{custom_text__b}}{{/if}}",
        "{{#if custom_single_choice__path=a}}{{#if custom_single_choice__path=b}}{{custom_text__b}}{{/if}}{{else}}Other{{/if}}",
    ] {
        let source = SourceFile {
            path: PathBuf::from("onboarding__letter.md"),
            contents: format!(
                "---\nkind: onboarding\nchoices:\n  path: {{a: A, b: B, c: C}}\nquestionnaire:\n  BEGIN: {{_: custom_single_choice__path}}\n  custom_single_choice__path: {{a: custom_text__a, _: custom_text__b}}\n  custom_text__a: {{_: END}}\n  custom_text__b: {{_: END}}\n  END: {{}}\n---\n{body}\n"
            ),
        };
        let refused = !F120BodyStateGrounding.lint(&source).is_empty();
        let mut missing = false;
        for path in ["a", "b", "c"] {
            for answer in ["", "false", "no", "0", "TRUE", "arbitrary"] {
                let asked = if path == "a" { "custom_text__a" } else { "custom_text__b" };
                let context = BTreeMap::from([
                    ("custom_single_choice__path".into(), path.into()),
                    (asked.into(), answer.into()),
                ]);
                missing |= forms::notation::fill(body, &context).contains("{{custom_text__");
            }
        }
        assert_eq!(refused, missing, "{body}");
    }
}

#[test]
fn dotted_guards_require_the_base_answer_only_when_absence_cannot_match() {
    use rules::F115PathResolution;
    for body in [
        "{{#if person__a.name}}{{person__a.name}}{{/if}}",
        "{{#if person__a.name=}}{{person__a.name}}{{/if}}",
        "{{#if person__a.name}}Named{{else}}{{person__a.name}}{{/if}}",
        "{{#if person__a.name=}}Empty{{else}}{{person__a.name}}{{/if}}",
        "{{#if person__a.name=Name}}{{custom_text__b}}{{/if}}",
    ] {
        let source = SourceFile {
            path: PathBuf::from("onboarding__letter.md"),
            contents: format!(
                "---\nkind: onboarding\nchoices:\n  path: {{a: A, b: B}}\nquestionnaire:\n  BEGIN: {{_: custom_single_choice__path}}\n  custom_single_choice__path: {{a: person__a, b: custom_text__b}}\n  person__a: {{_: END}}\n  custom_text__b: {{_: END}}\n  END: {{}}\n---\n{body}\n"
            ),
        };
        let refused = !F115PathResolution.lint(&source).is_empty()
            || !F120BodyStateGrounding.lint(&source).is_empty();
        let mut missing = false;
        for path in ["a", "b"] {
            for name in ["", "false", "no", "0", "Name"] {
                let mut context =
                    BTreeMap::from([("custom_single_choice__path".into(), path.into())]);
                if path == "a" {
                    context.insert("person__a.name".into(), name.into());
                } else {
                    context.insert("custom_text__b".into(), "Known".into());
                }
                let rendered = forms::notation::fill(body, &context);
                missing |= rendered.contains("{{person__a.name}}")
                    || rendered.contains("{{custom_text__b}}");
            }
        }
        assert_eq!(refused, missing, "{body}");
    }
}
