//! Version-pinned legal prose composed into a Template before it is persisted.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    source: String,
    #[serde(default)]
    inputs: BTreeMap<String, String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Clause {
    kind: String,
    #[serde(default)]
    inputs: BTreeMap<String, Input>,
    body: String,
}

/// A Clause input: the questionnaire types it accepts and the prompt asked.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Input {
    /// Question types a binding state may use.
    pub types: Vec<String>,
    /// The question asked for this input.
    pub prompt: String,
}

/// One pinned version of a library Clause, `<code>@<version>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryClause {
    /// Stable source name shared by every version.
    pub code: String,
    /// Positive version number pinned by Templates.
    pub version: i32,
    /// The Notation Kind that may include it.
    pub kind: String,
    /// Named inputs bound to questionnaire states.
    pub inputs: BTreeMap<String, Input>,
    /// Operative prose with `{{input}}` placeholders.
    pub body: String,
}

const LIBRARY: &str = include_str!("../clauses/library.yaml");

/// Every pinned Clause version in the bundled library, ordered by code and
/// version. The YAML remains the source of truth.
///
/// # Errors
/// Returns a malformed library or source name error.
pub fn library_clauses() -> Result<Vec<LibraryClause>, String> {
    let mut clauses = library()?
        .into_iter()
        .map(|(source, clause)| {
            let (code, version) = source
                .split_once('@')
                .and_then(|(code, version)| Some((code, version.parse::<i32>().ok()?)))
                .filter(|(code, version)| !code.is_empty() && *version > 0)
                .ok_or_else(|| format!("invalid Clause source `{source}`; use code@version"))?;
            Ok(LibraryClause {
                code: code.to_string(),
                version,
                kind: clause.kind,
                inputs: clause.inputs,
                body: clause.body,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    clauses.sort_by(|left, right| (&left.code, left.version).cmp(&(&right.code, right.version)));
    Ok(clauses)
}

fn references(doc: &Value) -> Result<BTreeMap<String, Reference>, String> {
    doc.get("clauses").map_or_else(
        || Ok(BTreeMap::new()),
        |value| serde_yaml::from_value(value.clone()).map_err(|error| error.to_string()),
    )
}

fn library() -> Result<BTreeMap<String, Clause>, String> {
    serde_yaml::from_str(LIBRARY).map_err(|error| error.to_string())
}

/// List the pinned Clause sources and their permitted notation kinds.
///
/// # Errors
/// Returns a malformed bundled library error.
pub fn catalog() -> Result<BTreeMap<String, String>, String> {
    Ok(library()?
        .into_iter()
        .map(|(source, clause)| (source, clause.kind))
        .collect())
}

/// Read a pinned Clause's authored YAML, including its inputs and body.
///
/// # Errors
/// Returns an unknown source or malformed bundled library error.
pub fn show(source: &str) -> Result<String, String> {
    let library: Value = serde_yaml::from_str(LIBRARY).map_err(|error| error.to_string())?;
    let clause = library
        .get(source)
        .ok_or_else(|| format!("unknown Clause version `{source}`"))?;
    serde_yaml::to_string(clause).map_err(|error| error.to_string())
}

fn selections(doc: &Value) -> Result<BTreeMap<String, (Reference, Clause)>, String> {
    let references = references(doc)?;
    if references.is_empty() {
        return Ok(BTreeMap::new());
    }
    let library = library()?;
    let mut selected = BTreeMap::new();
    for (alias, reference) in references {
        if alias.is_empty()
            || !alias
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(format!(
                "invalid Clause name `{alias}`; use lowercase snake_case"
            ));
        }
        // Several aliases may select the same pinned source.
        let clause = library
            .get(&reference.source)
            .cloned()
            .ok_or_else(|| format!("unknown Clause version `{}`", reference.source))?;
        if doc.get("kind").and_then(Value::as_str) != Some(clause.kind.as_str()) {
            return Err(format!("Clause `{alias}` requires kind `{}`", clause.kind));
        }
        if reference.inputs.keys().collect::<BTreeSet<_>>()
            != clause.inputs.keys().collect::<BTreeSet<_>>()
        {
            return Err(format!(
                "Clause `{alias}` must bind exactly its declared inputs"
            ));
        }
        for (name, state) in &reference.inputs {
            let Some((question_type, role)) = state.split_once("__") else {
                return Err(format!(
                    "Clause `{alias}` input `{name}` must name a typed questionnaire state"
                ));
            };
            if role.is_empty()
                || !role
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
                || !clause.inputs[name]
                    .types
                    .iter()
                    .any(|allowed| allowed == question_type)
            {
                return Err(format!(
                    "Clause `{alias}` input `{name}` has incompatible state `{state}`"
                ));
            }
        }
        selected.insert(alias, (reference, clause));
    }
    Ok(selected)
}

/// Add a Clause's required questions to an already normalized questionnaire.
/// Existing questions and prompts retain their order and wording.
///
/// # Errors
/// Returns an invalid reference, binding, or questionnaire error.
pub fn merge_questions(doc: &mut Value) -> Result<(), String> {
    let selected = selections(doc)?;
    if selected.is_empty() {
        return Ok(());
    }
    let mut questionnaire = doc
        .get("questionnaire")
        .and_then(Value::as_mapping)
        .cloned()
        .ok_or("Clauses require a questionnaire")?;
    let mut prompts = doc
        .get("prompts")
        .and_then(Value::as_mapping)
        .cloned()
        .unwrap_or_default();
    for (reference, clause) in selected.values() {
        for (name, state) in &reference.inputs {
            if !questionnaire.contains_key(Value::String(state.clone())) {
                // Insert before END; validation still checks the whole chain.
                let predecessors: Vec<Value> = questionnaire
                    .iter()
                    .filter(|(_, edges)| edges.get("_").and_then(Value::as_str) == Some("END"))
                    .map(|(key, _)| key.clone())
                    .collect();
                if predecessors.len() != 1 {
                    return Err(
                        "Clause questions require one questionnaire edge to END".to_string()
                    );
                }
                let edges = questionnaire
                    .get_mut(&predecessors[0])
                    .and_then(Value::as_mapping_mut)
                    .ok_or("questionnaire edges must be maps")?;
                edges.insert(Value::String("_".into()), Value::String(state.clone()));
                questionnaire.insert(
                    Value::String(state.clone()),
                    serde_yaml::to_value(BTreeMap::from([("_", "END")]))
                        .map_err(|error| error.to_string())?,
                );
            }
            let role = state
                .split_once("__")
                .map_or(state.as_str(), |(_, role)| role);
            prompts
                .entry(Value::String(role.into()))
                .or_insert_with(|| Value::String(clause.inputs[name].prompt.clone()));
        }
    }
    let root = doc.as_mapping_mut().ok_or("frontmatter must be a map")?;
    root.insert(
        Value::String("questionnaire".into()),
        Value::Mapping(questionnaire),
    );
    root.insert(Value::String("prompts".into()), Value::Mapping(prompts));
    Ok(())
}

/// Compose `{{> alias}}` references into ordinary notation placeholders.
/// The resulting source is self-contained: persistence pins its expanded bytes.
/// No filesystem, network, or runtime answer is evaluated during composition.
///
/// # Errors
/// Returns malformed references, unknown versions, or missing input bindings.
pub fn compile(markdown: &str) -> Result<String, String> {
    let Some((yaml, body)) = crate::frontmatter::split(markdown) else {
        return if markdown.contains("{{>") {
            Err("Clause references require frontmatter".into())
        } else {
            Ok(markdown.to_string())
        };
    };
    let mut doc: Value = serde_yaml::from_str(yaml).map_err(|error| error.to_string())?;
    if doc.get("clauses").is_none() && !body.contains("{{>") {
        return Ok(markdown.to_string());
    }
    let selected = selections(&doc)?;
    if !selected.is_empty() && doc.get("questionnaire").is_none() {
        return Err("Clauses require a questionnaire".into());
    }
    let mut used = BTreeSet::new();
    let mut expanded = String::new();
    let mut remaining = body;
    while let Some(start) = remaining.find("{{>") {
        expanded.push_str(&remaining[..start]);
        let after = &remaining[start + 3..];
        let end = after.find("}}").ok_or("unclosed Clause reference")?;
        let alias = after[..end].trim();
        let (reference, clause) = selected
            .get(alias)
            .ok_or_else(|| format!("undeclared Clause `{alias}`"))?;
        used.insert(alias.to_string());
        let prose = bind_inputs(clause.body.trim_end(), &reference.inputs);
        expanded.push_str(&prose);
        remaining = &after[end + 2..];
    }
    expanded.push_str(remaining);
    if expanded.contains("{{>") {
        return Err("nested Clause references are not supported".into());
    }
    if used.len() != selected.len() {
        return Err("every declared Clause must be referenced in the body".into());
    }
    crate::questionnaire::normalize_value(&mut doc)?;
    if let Some(root) = doc.as_mapping_mut() {
        if let Some(references) = root.remove(Value::String("clauses".into())) {
            root.insert(Value::String("clause_sources".into()), references);
        }
    }
    // Provenance stays with the snapshot without a live library reference.
    let yaml = serde_yaml::to_string(&doc).map_err(|error| error.to_string())?;
    Ok(format!("---\n{yaml}---\n{expanded}"))
}

/// The `code@version` sources a compiled snapshot pins, from its
/// `clause_sources:` frontmatter, sorted and without repeats. A body without
/// that block, or with unreadable frontmatter, pins nothing.
#[must_use]
pub fn pinned_sources(markdown: &str) -> Vec<String> {
    let Some((yaml, _)) = crate::frontmatter::split(markdown) else {
        return Vec::new();
    };
    let Some(sources) = serde_yaml::from_str::<Value>(yaml)
        .ok()
        .and_then(|doc| doc.get("clause_sources").cloned())
        .and_then(|value| serde_yaml::from_value::<BTreeMap<String, Reference>>(value).ok())
    else {
        return Vec::new();
    };
    sources
        .into_values()
        .map(|reference| reference.source)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn bind_inputs(prose: &str, inputs: &BTreeMap<String, String>) -> String {
    let mut bound = prose.to_string();
    for (input, state) in inputs {
        bound = bound
            .replace(&format!("{{{{{input}}}}}"), &format!("{{{{{state}}}}}"))
            .replace(&format!("{{{{{input}."), &format!("{{{{{state}."))
            .replace(
                &format!("{{{{#if {input}}}}}"),
                &format!("{{{{#if {state}}}}}"),
            )
            .replace(&format!("{{{{#if {input}="), &format!("{{{{#if {state}="))
            .replace(
                &format!("{{{{else if {input}}}}}"),
                &format!("{{{{else if {state}}}}}"),
            )
            .replace(
                &format!("{{{{else if {input}="),
                &format!("{{{{else if {state}="),
            )
            .replace(&format!(" in {input}}}}}"), &format!(" in {state}}}}}"));
    }
    bound
}

/// The Clause every `kind: onboarding` template must include.
pub const ENGAGEMENT_ARBITRATION: &str = "engagement_arbitration";

/// The highest pinned version of library Clause `code`, as `code@version`.
///
/// # Errors
/// Returns a malformed bundled library error, or an unknown code.
pub fn current_source(code: &str) -> Result<String, String> {
    library_clauses()?
        .into_iter()
        .filter(|clause| clause.code == code)
        .map(|clause| clause.version)
        .max()
        .map(|version| format!("{code}@{version}"))
        .ok_or_else(|| format!("unknown Clause `{code}`"))
}

/// Why an onboarding template fails the engagement-arbitration requirement,
/// or `None` when it includes the current Clause (or is not an onboarding).
/// A compiled snapshot pins the Clause under `clause_sources:` and has already
/// expanded its references.
fn missing_engagement_arbitration(markdown: &str) -> Option<String> {
    let (yaml, body) = crate::frontmatter::split(markdown)?;
    let doc: Value = serde_yaml::from_str(yaml).ok()?;
    if doc.get("kind").and_then(Value::as_str) != Some("onboarding") {
        return None;
    }
    let current = current_source(ENGAGEMENT_ARBITRATION).ok()?;
    let sources = |key: &str| {
        doc.get(key)
            .and_then(|value| {
                serde_yaml::from_value::<BTreeMap<String, Reference>>(value.clone()).ok()
            })
            .unwrap_or_default()
    };
    let compiled = sources("clause_sources")
        .values()
        .any(|reference| reference.source == current);
    let referenced = sources("clauses")
        .into_iter()
        .filter(|(_, reference)| reference.source == current)
        .any(|(alias, _)| body.contains(&format!("{{{{> {alias}}}}}")));
    (!compiled && !referenced).then(|| {
        format!(
            "an onboarding template must declare `{current}` under `clauses:` and reference it in \
             the body with `{{{{> name}}}}`"
        )
    })
}

/// Validate Clause composition and the expanded body's existing notation rules.
pub struct ClauseReferences;

impl crate::Rule for ClauseReferences {
    fn code(&self) -> &'static str {
        "N132"
    }

    fn lint(&self, file: &crate::SourceFile) -> Vec<crate::Violation> {
        if let Some(message) = missing_engagement_arbitration(&file.contents) {
            return vec![crate::Violation {
                code: "N132",
                path: file.path.clone(),
                line: 1,
                range: crate::line_byte_range(&file.contents, 1),
                message,
            }];
        }
        let compiled = match compile(&file.contents) {
            Ok(compiled) => compiled,
            Err(message) => {
                return vec![crate::Violation {
                    code: "N132",
                    path: file.path.clone(),
                    line: 1,
                    range: crate::line_byte_range(&file.contents, 1),
                    message,
                }]
            }
        };
        if compiled == file.contents {
            return Vec::new();
        }
        let expanded = crate::SourceFile {
            path: file.path.clone(),
            contents: compiled,
        };
        [
            &crate::F115PathResolution as &dyn crate::Rule,
            &crate::F120BodyStateGrounding,
            &crate::F131TemplateHasNoDraftingMarkers,
        ]
        .into_iter()
        .flat_map(|rule| rule.lint(&expanded))
        .map(|violation| crate::Violation {
            code: "N132",
            path: file.path.clone(),
            line: 1,
            range: crate::line_byte_range(&file.contents, 1),
            message: format!("expanded Clause: {}: {}", violation.code, violation.message),
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::compile;

    #[test]
    fn library_clauses_split_every_source_into_code_and_version() {
        let clauses = super::library_clauses().unwrap();
        assert_eq!(clauses.len(), super::catalog().unwrap().len());
        let governing = clauses
            .iter()
            .find(|clause| clause.code == "governing_law" && clause.version == 1)
            .unwrap();
        assert_eq!(governing.kind, "agreement");
        assert_eq!(
            governing.inputs.keys().collect::<Vec<_>>(),
            ["forum", "jurisdiction"]
        );
        assert!(clauses
            .windows(2)
            .all(|pair| (&pair[0].code, pair[0].version) < (&pair[1].code, pair[1].version)));
    }

    fn source(reference: &str, body: &str) -> String {
        format!("---\nkind: agreement\nquestionnaire: [entity__company]\nclauses:\n  law:\n    source: {reference}\n    inputs:\n      jurisdiction: jurisdiction__law\n      forum: custom_text__forum\n---\n{body}")
    }

    #[test]
    fn a_compiled_snapshot_names_the_sources_it_pins() {
        let compiled = compile(&source("governing_law@1", "{{> law}}")).unwrap();
        assert_eq!(super::pinned_sources(&compiled), ["governing_law@1"]);
        assert!(super::pinned_sources("no frontmatter").is_empty());
        assert!(super::pinned_sources("---\nkind: agreement\n---\nbody").is_empty());
        assert!(super::pinned_sources("---\nclause_sources: [oops\n---\nbody").is_empty());
    }

    #[test]
    fn pinned_clause_composes_and_adds_missing_questions() {
        let compiled = compile(&source("governing_law@1", "{{> law}}")).unwrap();
        assert!(compiled
            .contains("Governing law: {{jurisdiction__law}}. Courts: {{custom_text__forum}}."));
        let yaml = crate::frontmatter::extract(&compiled).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            doc["questionnaire"]["entity__company"]["_"],
            "custom_text__forum"
        );
        assert_eq!(
            doc["questionnaire"]["custom_text__forum"]["_"],
            "jurisdiction__law"
        );
        assert_eq!(doc["questionnaire"]["jurisdiction__law"]["_"], "END");
        assert_eq!(compile(&compiled).unwrap(), compiled);
    }

    #[test]
    fn unknown_versions_references_and_wrong_input_types_fail() {
        assert!(compile(&source("governing_law@99", "{{> law}}")).is_err());
        assert!(compile(&source("governing_law@1", "{{> absent}}")).is_err());
        assert!(compile(&source("governing_law@1", "{{> law")).is_err());
        assert!(compile(
            &source("governing_law@1", "{{> law}}").replace("jurisdiction__law", "person__law")
        )
        .is_err());
    }

    #[test]
    fn empty_and_extra_inputs_fail_and_plain_sources_keep_their_bytes() {
        assert!(compile(
            &source("governing_law@1", "{{> law}}")
                .replace("      forum: custom_text__forum\n", "")
        )
        .is_err());
        assert!(compile(&source("governing_law@1", "{{> law}}").replace(
            "    inputs:",
            "    inputs:\n      extra: custom_text__extra"
        ))
        .is_err());
        let plain = "---\r\nkind: agreement\r\n---\r\nBody.\r\n";
        assert_eq!(compile(plain).unwrap(), plain);
    }

    #[test]
    fn existing_question_order_and_prompt_are_preserved() {
        let source = source("governing_law@1", "{{> law}}")
            .replace("questionnaire: [entity__company]", "questionnaire: [jurisdiction__law, custom_text__forum]\nprompts:\n  law: Authored governing-law question");
        let compiled = compile(&source).unwrap();
        let doc: serde_yaml::Value =
            serde_yaml::from_str(crate::frontmatter::extract(&compiled).unwrap()).unwrap();
        assert_eq!(doc["questionnaire"]["BEGIN"]["_"], "jurisdiction__law");
        assert_eq!(
            doc["questionnaire"]["jurisdiction__law"]["_"],
            "custom_text__forum"
        );
        assert_eq!(doc["prompts"]["law"], "Authored governing-law question");
    }

    #[test]
    fn gate_refuses_unused_and_undeclared_references() {
        use crate::Rule;
        for source in [
            source("governing_law@1", "No reference."),
            source("governing_law@1", "{{> absent}}"),
        ] {
            let violations = super::ClauseReferences.lint(&crate::SourceFile {
                path: "sample.md".into(),
                contents: source,
            });
            assert_eq!(violations.len(), 1);
            assert_eq!(violations[0].code, "N132");
        }
    }

    fn onboarding(clauses: &str, body: &str) -> crate::SourceFile {
        crate::SourceFile {
            path: "onboarding__letter.md".into(),
            contents: format!(
                "---\nkind: onboarding\nquestionnaire: [jurisdiction__governing_law, jurisdiction__arbitration_seat]\n{clauses}---\n{body}"
            ),
        }
    }

    const ARBITRATION: &str = "clauses:\n  arbitration:\n    source: engagement_arbitration@1\n    inputs:\n      law: jurisdiction__governing_law\n      seat: jurisdiction__arbitration_seat\n";

    #[test]
    fn an_onboarding_template_must_include_the_current_engagement_arbitration() {
        use crate::Rule;
        let lint = |file: crate::SourceFile| super::ClauseReferences.lint(&file);
        assert!(lint(onboarding(ARBITRATION, "{{> arbitration}}\n")).is_empty());
        for file in [
            onboarding("", "Letter.\n"),
            onboarding(&ARBITRATION.replace("@1", "@0"), "{{> arbitration}}\n"),
        ] {
            let violations = lint(file);
            assert_eq!(violations.len(), 1, "{violations:?}");
            assert_eq!(violations[0].code, "N132");
            assert!(violations[0].message.contains("engagement_arbitration@"));
        }
        // Declared but never placed in the body.
        assert_eq!(lint(onboarding(ARBITRATION, "Letter.\n")).len(), 1);
        // Other kinds are unaffected.
        assert!(lint(crate::SourceFile {
            path: "offboarding__letter.md".into(),
            contents: "---\nkind: offboarding\n---\nLetter.\n".into()
        })
        .is_empty());
    }

    #[test]
    fn a_compiled_onboarding_snapshot_satisfies_the_requirement() {
        use crate::Rule;
        let compiled = compile(&onboarding(ARBITRATION, "{{> arbitration}}\n").contents).unwrap();
        let violations = super::ClauseReferences.lint(&crate::SourceFile {
            path: "onboarding__letter.md".into(),
            contents: compiled,
        });
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn snapshots_do_not_resolve_historical_library_versions() {
        let compiled = compile(&source("governing_law@1", "{{> law}}")).unwrap();
        let historical = compiled.replace("governing_law@1", "retired_source@77");
        assert_eq!(compile(&historical).unwrap(), historical);
        assert!(compile(
            &source("governing_law@1", "{{> law}}")
                .replace("questionnaire: [entity__company]\n", "")
        )
        .is_err());
    }

    #[test]
    fn bindings_preserve_condition_and_iterator_grammars() {
        let bound = super::bind_inputs(
            "{{law}} {{#if law=NV}}Nevada{{else if law=CA}}California{{else}}Other{{/if}} {{#if law}}Chosen{{/if}} {{#if other}}{{else if law}}Law branch{{/if}} {{#for p in parties}}{{p.name}}{{/for}} {{company.name}}",
            &std::collections::BTreeMap::from([
                ("law".into(), "jurisdiction__law".into()),
                ("parties".into(), "people__parties".into()),
                ("company".into(), "entity__company".into()),
            ]),
        );
        assert!(bound.contains("{{#if jurisdiction__law=NV}}"));
        assert!(bound.contains("{{else if jurisdiction__law=CA}}"));
        assert!(bound.contains("{{else}}Other{{/if}}"));
        assert!(bound.contains("{{#if jurisdiction__law}}"));
        assert!(bound.contains("{{else if jurisdiction__law}}"));
        assert!(bound.contains("{{#for p in people__parties}}"));
        assert!(bound.contains("{{entity__company.name}}"));
    }
}
