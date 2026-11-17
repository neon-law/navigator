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
    retired_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    inputs: BTreeMap<String, Input>,
    /// Optional reserved caption for the section that restates this Clause.
    #[serde(default)]
    heading: Option<String>,
    /// Further captions that reserve the same Clause.
    #[serde(default)]
    aliases: Vec<String>,
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
    /// When this version stopped accepting new references. Wording stays readable.
    pub retired_at: Option<chrono::DateTime<chrono::Utc>>,
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
    /// Optional reserved caption for the section that restates this Clause.
    pub heading: Option<String>,
    /// Further captions that reserve the same Clause.
    pub aliases: Vec<String>,
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
                retired_at: clause.retired_at,
                code: code.to_string(),
                version,
                kind: clause.kind,
                inputs: clause.inputs,
                body: clause.body,
                heading: clause.heading,
                aliases: clause.aliases,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    clauses.sort_by(|left, right| (&left.code, left.version).cmp(&(&right.code, right.version)));
    check_reserved_headings(&clauses)?;
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
        if clause.retired_at.is_some() {
            return Err(format!(
                "Clause version `{}` is retired; choose an active version",
                reference.source
            ));
        }
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
            // A direct Project binding (`project.onboarding_jurisdiction`,
            // `project.client_dri`) answers no questionnaire question: the
            // render context supplies the value from the notation's live
            // Project row. Only those two fields bind directly, each only
            // to an input of the field's own kind.
            if let Some((table, field)) = state.split_once('.') {
                let required = match (table, field) {
                    ("project", "onboarding_jurisdiction") => "jurisdiction",
                    ("project", "client_dri") => "person",
                    _ => {
                        return Err(format!(
                            "Clause `{alias}` input `{name}` binds `{state}`; only `project.onboarding_jurisdiction` and `project.client_dri` bind directly to the Project"
                        ));
                    }
                };
                if !clause.inputs[name]
                    .types
                    .iter()
                    .any(|allowed| allowed == required)
                {
                    return Err(format!(
                        "Clause `{alias}` input `{name}` binds `{state}`, which is a {required} and must bind a `{required}` input"
                    ));
                }
                continue;
            }
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
            // A direct Project binding answers no questionnaire question.
            if state.contains('.') {
                continue;
            }
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

/// The highest active version of library Clause `code`, as `code@version`.
///
/// # Errors
/// Returns a malformed bundled library error, or an unknown code.
pub fn current_source(code: &str) -> Result<String, String> {
    library_clauses()?
        .into_iter()
        .filter(|clause| clause.code == code && clause.retired_at.is_none())
        .map(|clause| clause.version)
        .max()
        .map(|version| format!("{code}@{version}"))
        .ok_or_else(|| format!("unknown Clause `{code}`"))
}

/// Whether `markdown` is an onboarding template that lacks the current
/// `engagement_arbitration` Clause — what N132 fails on an unexecuted
/// onboarding. The Project gate asks it of an executed one to say which
/// template the signed copy has made retirable.
#[must_use]
pub fn onboarding_lacks_engagement_arbitration(markdown: &str) -> bool {
    missing_engagement_arbitration(markdown, false).is_some()
}

/// Why an onboarding template fails the engagement-arbitration requirement,
/// or `None` when it includes the current Clause (or is not an onboarding).
/// A compiled snapshot pins the Clause under `clause_sources:` and has already
/// expanded its references. `engagement_executed` waives the requirement: the
/// executed onboarding was generated from this template as it stood when it
/// was sent, and the signed copy, not the template, is now the record.
fn missing_engagement_arbitration(markdown: &str, engagement_executed: bool) -> Option<String> {
    let (yaml, body) = crate::frontmatter::split(markdown)?;
    let doc: Value = serde_yaml::from_str(yaml).ok()?;
    if doc.get("kind").and_then(Value::as_str) != Some("onboarding") {
        return None;
    }
    if engagement_executed {
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

/// Normalize a heading caption for reserved-heading matching: strip a
/// leading outline numeral (`IX.`, `7.`), trim, lower case, and drop a
/// trailing colon.
fn normalize_heading(text: &str) -> String {
    let mut text = text.trim();
    if let Some((numeral, rest)) = text.split_once('.') {
        let roman_or_arabic = !numeral.is_empty()
            && numeral.bytes().all(|byte| {
                byte.is_ascii_digit()
                    || matches!(
                        byte.to_ascii_uppercase(),
                        b'I' | b'V' | b'X' | b'L' | b'C' | b'D' | b'M'
                    )
            });
        if roman_or_arabic {
            text = rest.trim_start();
        }
    }
    let caption = text.trim_end();
    let caption = caption.strip_suffix(':').unwrap_or(caption).trim_end();
    caption.to_lowercase()
}

/// Every reserved caption and alias may claim only one Clause code; a
/// duplicate or cross-code collision fails library parsing.
fn check_reserved_headings(clauses: &[LibraryClause]) -> Result<(), String> {
    let mut claims = BTreeMap::<String, String>::new();
    for clause in clauses {
        let captions = clause
            .heading
            .iter()
            .chain(clause.aliases.iter())
            .map(String::as_str);
        for caption in captions {
            let normalized = normalize_heading(caption);
            if normalized.is_empty() {
                return Err(format!(
                    "Clause `{}` reserves an empty heading",
                    clause.code
                ));
            }
            match claims.get(&normalized) {
                Some(existing) if existing != &clause.code => {
                    return Err(format!(
                        "reserved heading `{caption}` is claimed by both `{existing}` and `{}`",
                        clause.code
                    ));
                }
                Some(_) => {}
                None => {
                    claims.insert(normalized, clause.code.clone());
                }
            }
        }
    }
    Ok(())
}

/// Why an authored template misuses a reserved heading, one message per
/// offending heading, or empty. Compiled snapshots — which carry
/// `clause_sources:` and expanded wording — are never re-checked.
fn reserved_heading_violations(markdown: &str) -> Vec<String> {
    let Some((yaml, body)) = crate::frontmatter::split(markdown) else {
        return Vec::new();
    };
    let Ok(doc) = serde_yaml::from_str::<Value>(yaml) else {
        return Vec::new();
    };
    if doc.get("clause_sources").is_some() {
        return Vec::new();
    }
    let Ok(library) = library_clauses() else {
        return Vec::new();
    };
    let reserved = library
        .iter()
        .flat_map(|clause| {
            clause
                .heading
                .iter()
                .chain(clause.aliases.iter())
                .map(|caption| (normalize_heading(caption), clause.code.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut violations = Vec::new();
    let mut lines = body.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        let caption = trimmed.trim_start_matches('#').trim_start();
        let Some(code) = reserved.get(&normalize_heading(caption)) else {
            continue;
        };
        let section = (&mut lines)
            .take_while(|line| !line.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        let reference = section
            .strip_prefix("{{>")
            .and_then(|rest| rest.strip_suffix("}}"))
            .map(str::trim);
        let declared = reference.and_then(|alias| {
            doc.get("clauses")
                .and_then(|clauses| clauses.get(alias))
                .and_then(|pin| pin.get("source"))
                .and_then(Value::as_str)
                .map(String::from)
        });
        if !matches!(&declared, Some(source) if source.starts_with(&format!("{code}@"))) {
            violations.push(format!(
                "the reserved heading `{}` must open a section whose only content is a `{{{{> name}}}}` reference to an active `{code}` version",
                caption.trim_end()
            ));
        }
    }
    violations
}

/// Validate Clause composition and the expanded body's existing notation rules.
#[derive(Debug, Default)]
pub struct ClauseReferences {
    /// Execution evidence — a committed signed `kind: onboarding` pointer
    /// carrying a `docusign_envelope_id` — waives the onboarding
    /// engagement-arbitration requirement. `Default` is the unwaived rule,
    /// what every non-gate caller gets.
    engagement_executed: bool,
}

impl ClauseReferences {
    /// The rule as the Project gate runs it, told whether the Project's
    /// engagement has been executed.
    #[must_use]
    pub const fn executed(engagement_executed: bool) -> Self {
        Self {
            engagement_executed,
        }
    }
}

impl crate::Rule for ClauseReferences {
    fn code(&self) -> &'static str {
        "N132"
    }

    fn lint(&self, file: &crate::SourceFile) -> Vec<crate::Violation> {
        if let Some(message) =
            missing_engagement_arbitration(&file.contents, self.engagement_executed)
        {
            return vec![crate::Violation {
                code: "N132",
                path: file.path.clone(),
                line: 1,
                range: crate::line_byte_range(&file.contents, 1),
                message,
            }];
        }
        let reserved = reserved_heading_violations(&file.contents);
        if !reserved.is_empty() {
            return reserved
                .into_iter()
                .map(|message| crate::Violation {
                    code: "N132",
                    path: file.path.clone(),
                    line: 1,
                    range: crate::line_byte_range(&file.contents, 1),
                    message,
                })
                .collect();
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
    use std::collections::BTreeMap;

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

    /// The Global DPA's version restates `@1` verbatim and adds only the
    /// transfer-instrument carve-out, so the two never drift apart silently.
    #[test]
    fn dispute_resolution_two_adds_only_the_transfer_instrument_carve_out() {
        let clauses = super::library_clauses().unwrap();
        let one = clauses
            .iter()
            .find(|clause| clause.code == "dispute_resolution" && clause.version == 1)
            .unwrap();
        let two = clauses
            .iter()
            .find(|clause| clause.code == "dispute_resolution" && clause.version == 2)
            .unwrap();
        assert_eq!(one.kind, "agreement");
        assert_eq!(one.inputs, two.inputs);
        assert_eq!(
            one.inputs.keys().collect::<Vec<_>>(),
            ["law", "seat"],
            "the two versions bind the same typed inputs"
        );
        let carve_out = "Except that a dispute arising under a transfer instrument adopted for international data \
            transfers, or affecting a data subject's or supervisory authority's rights under such an instrument, \
            is resolved as that instrument provides.";
        let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            words(&two.body),
            format!("{} {}", words(&one.body), words(carve_out)),
            "`@2` is `@1` plus the carve-out sentence, nothing else"
        );
        // `@1` stays active: the advisor agreement and MSA keep pinning it.
        assert!(one.retired_at.is_none());
        assert_eq!(
            super::current_source("dispute_resolution").unwrap(),
            "dispute_resolution@2"
        );
    }

    fn source(reference: &str, body: &str) -> String {
        let inputs = if reference.starts_with("governing_law@") {
            "      jurisdiction: jurisdiction__law\n      forum: custom_text__forum"
        } else {
            "      law: jurisdiction__law\n      seat: jurisdiction__forum"
        };
        format!("---\nkind: agreement\nquestionnaire: [entity__company]\nclauses:\n  law:\n    source: {reference}\n    inputs:\n{inputs}\n---\n{body}")
    }

    #[test]
    fn retired_versions_refuse_new_references_but_saved_snapshots_stay_readable() {
        use crate::Rule;
        let authored = source("governing_law@1", "{{> law}}");
        let violations = super::ClauseReferences::default().lint(&crate::SourceFile {
            path: "synthetic.md".into(),
            contents: authored.clone(),
        });
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].code, "N132");
        assert!(violations[0].message.contains("retired"));
        assert!(compile(&authored).unwrap_err().contains("retired"));
        let nevada = "---\nkind: letter\nquestionnaire: [jurisdiction__law]\nclauses:\n  arbitration:\n    source: nevada_engagement_arbitration@1\n    inputs:\n      law: jurisdiction__law\n---\n{{> arbitration}}\n";
        assert!(compile(nevada).unwrap_err().contains("retired"));
        assert!(super::current_source("governing_law").is_err());
        assert_eq!(
            super::current_source("nevada_engagement_arbitration").unwrap(),
            "nevada_engagement_arbitration@2"
        );
        let snapshot = "---\nkind: agreement\nclause_sources:\n  law:\n    source: governing_law@1\n    inputs:\n      jurisdiction: jurisdiction__law\n      forum: custom_text__forum\n---\nSaved operative wording.\n";
        assert_eq!(compile(snapshot).unwrap(), snapshot);
        assert!(super::show("governing_law@1")
            .unwrap()
            .contains("Governing law"));
        assert!(super::show("nevada_engagement_arbitration@1")
            .unwrap()
            .contains("JAMS"));
    }

    #[test]
    fn a_compiled_snapshot_names_the_sources_it_pins() {
        let compiled = compile(&source("dispute_resolution@1", "{{> law}}")).unwrap();
        assert_eq!(super::pinned_sources(&compiled), ["dispute_resolution@1"]);
        assert!(super::pinned_sources("no frontmatter").is_empty());
        assert!(super::pinned_sources("---\nkind: agreement\n---\nbody").is_empty());
        assert!(super::pinned_sources("---\nclause_sources: [oops\n---\nbody").is_empty());
    }

    #[test]
    fn pinned_clause_composes_and_adds_missing_questions() {
        let compiled = compile(&source("dispute_resolution@1", "{{> law}}")).unwrap();
        assert!(compiled.contains("The law of {{jurisdiction__law}} governs this agreement"));
        let yaml = crate::frontmatter::extract(&compiled).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            doc["questionnaire"]["entity__company"]["_"],
            "jurisdiction__law"
        );
        assert_eq!(
            doc["questionnaire"]["jurisdiction__law"]["_"],
            "jurisdiction__forum"
        );
        assert_eq!(doc["questionnaire"]["jurisdiction__forum"]["_"], "END");
        assert_eq!(compile(&compiled).unwrap(), compiled);
    }

    #[test]
    fn unknown_versions_references_and_wrong_input_types_fail() {
        assert!(compile(&source("governing_law@99", "{{> law}}")).is_err());
        assert!(compile(&source("dispute_resolution@1", "{{> absent}}")).is_err());
        assert!(compile(&source("dispute_resolution@1", "{{> law")).is_err());
        assert!(compile(
            &source("dispute_resolution@1", "{{> law}}")
                .replace("jurisdiction__law", "person__law")
        )
        .is_err());
    }

    #[test]
    fn empty_and_extra_inputs_fail_and_plain_sources_keep_their_bytes() {
        assert!(compile(
            &source("dispute_resolution@1", "{{> law}}")
                .replace("      seat: jurisdiction__forum\n", "")
        )
        .is_err());
        assert!(
            compile(&source("dispute_resolution@1", "{{> law}}").replace(
                "    inputs:",
                "    inputs:\n      extra: custom_text__extra"
            ))
            .is_err()
        );
        let plain = "---\r\nkind: agreement\r\n---\r\nBody.\r\n";
        assert_eq!(compile(plain).unwrap(), plain);
    }

    #[test]
    fn existing_question_order_and_prompt_are_preserved() {
        let source = source("dispute_resolution@1", "{{> law}}")
            .replace("questionnaire: [entity__company]", "questionnaire: [jurisdiction__law, jurisdiction__forum]\nprompts:\n  law: Authored governing-law question");
        let compiled = compile(&source).unwrap();
        let doc: serde_yaml::Value =
            serde_yaml::from_str(crate::frontmatter::extract(&compiled).unwrap()).unwrap();
        assert_eq!(doc["questionnaire"]["BEGIN"]["_"], "jurisdiction__law");
        assert_eq!(
            doc["questionnaire"]["jurisdiction__law"]["_"],
            "jurisdiction__forum"
        );
        assert_eq!(doc["prompts"]["law"], "Authored governing-law question");
    }

    #[test]
    fn gate_refuses_unused_and_undeclared_references() {
        use crate::Rule;
        for source in [
            source("dispute_resolution@1", "No reference."),
            source("dispute_resolution@1", "{{> absent}}"),
        ] {
            let violations = super::ClauseReferences::default().lint(&crate::SourceFile {
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
        let lint = |file: crate::SourceFile| super::ClauseReferences::default().lint(&file);
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
        let violations = super::ClauseReferences::default().lint(&crate::SourceFile {
            path: "onboarding__letter.md".into(),
            contents: compiled,
        });
        assert!(violations.is_empty(), "{violations:?}");
    }

    /// LAW-226: once the engagement is executed, an onboarding template no
    /// longer has to carry today's `engagement_arbitration` — the signed copy
    /// was generated from the template as it stood. The waiver covers the
    /// requirement only: composition checks (retired sources, bindings)
    /// still apply, and the default rule stays unwaived.
    #[test]
    fn an_executed_engagement_waives_the_requirement_and_the_unexecuted_still_fails() {
        use crate::Rule;
        let file = onboarding("", "Letter.\n");
        let executed = super::ClauseReferences::executed(true);
        assert!(executed.lint(&file).is_empty(), "executed skips");
        assert_eq!(
            super::ClauseReferences::default().lint(&file).len(),
            1,
            "default (not executed) still fails"
        );

        let retired = onboarding(
            "clauses:\n  law:\n    source: governing_law@1\n    inputs:\n      jurisdiction: jurisdiction__law\n      forum: custom_text__forum\n",
            "{{> law}}\n",
        );
        let violations = executed.lint(&retired);
        assert_eq!(violations.len(), 1, "composition still applies");
        assert!(violations[0].message.contains("retired"), "{violations:?}");
    }

    #[test]
    fn snapshots_do_not_resolve_historical_library_versions() {
        let compiled = compile(&source("dispute_resolution@1", "{{> law}}")).unwrap();
        let historical = compiled.replace("dispute_resolution@1", "retired_source@77");
        assert_eq!(compile(&historical).unwrap(), historical);
        assert!(compile(
            &source("dispute_resolution@1", "{{> law}}")
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

    /// A `jurisdiction` input may bind straight to the Project's
    /// `onboarding_jurisdiction` field: the compiled body carries the dotted
    /// placeholder the render context seeds, and the binding answers no
    /// questionnaire question, so nothing is merged for it.
    #[test]
    fn a_jurisdiction_input_may_bind_directly_to_the_project_onboarding_jurisdiction() {
        let template = crate::SourceFile {
            path: "onboarding__letter.md".into(),
            contents: "---\nkind: onboarding\nquestionnaire: [jurisdiction__arbitration_seat]\nclauses:\n  arbitration:\n    source: engagement_arbitration@1\n    inputs:\n      law: project.onboarding_jurisdiction\n      seat: jurisdiction__arbitration_seat\n---\n{{> arbitration}}\n".into(),
        };
        let compiled = compile(&template.contents).unwrap();
        assert!(
            compiled
                .contains("The law of {{project.onboarding_jurisdiction}} governs this letter."),
            "the project binding compiles to the dotted placeholder: {compiled}"
        );
        assert!(compiled.contains("{{jurisdiction__arbitration_seat}}"));
        // The questionnaire keeps only the seat question: the project binding
        // must not be merged in as a question edge.
        assert!(!compiled.contains("jurisdiction__governing_law"));
    }

    /// Only the one supported Project field binds directly; any other
    /// `project.*` path is a mistake the compile refuses, not a silent
    /// empty placeholder.
    #[test]
    fn a_project_binding_must_name_the_onboarding_jurisdiction_field() {
        let template = onboarding(
            "clauses:\n  arbitration:\n    source: engagement_arbitration@1\n    inputs:\n      law: project.governing_law\n      seat: jurisdiction__arbitration_seat\n",
            "{{> arbitration}}\n",
        );
        let error = compile(&template.contents).unwrap_err();
        assert!(error.contains("project.governing_law"), "{error}");
    }

    /// A `person` input may bind straight to the Project's `client_dri`
    /// field: the compiled body carries the dotted `project.client_dri`
    /// placeholders the render context seeds from the bound Person row.
    #[test]
    fn a_person_input_may_bind_directly_to_the_project_client_dri() {
        let template = crate::SourceFile {
            path: "letter__notice.md".into(),
            contents: "---\nkind: letter\nquestionnaire: [jurisdiction__governing_law]\nclauses:\n  contact:\n    source: client_dri_contact@1\n    inputs:\n      client: project.client_dri\n---\n{{> contact}}\n".into(),
        };
        let compiled = compile(&template.contents).unwrap();
        assert!(
            compiled.contains("{{project.client_dri.name}}"),
            "the person binding expands the field reference: {compiled}"
        );
        assert!(
            compiled.contains("{{project.client_dri.email}}"),
            "the person binding expands the email reference: {compiled}"
        );
    }

    /// Fails closed: `project.client_dri` is a person, so a non-person
    /// input bound to it is refused, naming the field and the input kind.
    #[test]
    fn a_client_dri_binding_refuses_a_non_person_input() {
        let template = onboarding(
            "clauses:\n  arbitration:\n    source: engagement_arbitration@1\n    inputs:\n      law: project.client_dri\n      seat: jurisdiction__arbitration_seat\n",
            "{{> arbitration}}\n",
        );
        let error = compile(&template.contents).unwrap_err();
        assert!(
            error.contains("project.client_dri") && error.contains("person"),
            "the refusal names the field and its kind: {error}"
        );
    }

    /// A DPA-shaped authored template: a numbered reserved section holding
    /// exactly the body given, under frontmatter pinning `dispute_resolution`.
    fn dpa(heading: &str, body: &str, pinned: &str) -> crate::SourceFile {
        crate::SourceFile {
            path: "dpa_us.md".into(),
            contents: format!(
                "---\nkind: agreement\nquestionnaire: [entity__company]\nclauses:\n  dispute:\n    source: {pinned}\n    inputs:\n      law: jurisdiction__law\n      seat: jurisdiction__seat\n---\n## VIII. Return, deletion, and priority\n\nProtection continues until deletion.\n\n{heading}\n\n{body}\n\n## Signatures\n"
            ),
        }
    }

    /// LAW-142: a reserved heading whose section is exactly one reference to
    /// a pinned version of the reserving Clause passes the gate — whatever
    /// the questionnaire binds into the declared inputs.
    #[test]
    fn a_compliant_reserved_heading_section_passes_the_gate() {
        use crate::Rule;
        for heading in [
            "## IX. Governing law and dispute resolution",
            "## Governing Law:",
            "## Dispute resolution",
            "### 7. Choice of Law",
        ] {
            assert!(
                super::ClauseReferences::default()
                    .lint(&dpa(heading, "{{> dispute}}", "dispute_resolution@1"))
                    .is_empty(),
                "the alias `{heading}` reserves the same Clause"
            );
        }
    }

    #[test]
    fn a_hand_written_copy_of_the_clause_fails_the_reserved_heading() {
        use crate::Rule;
        let near_copy =
            "The law of {{jurisdiction__law}} governs this agreement. Disputes go to AAA \
             arbitration seated in {{jurisdiction__seat}}.";
        let violations = super::ClauseReferences::default().lint(&dpa(
            "## IX. Governing law and dispute resolution",
            near_copy,
            "dispute_resolution@1",
        ));
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].code, "N132");
        assert!(
            violations[0]
                .message
                .contains("Governing law and dispute resolution")
                && violations[0].message.contains("dispute_resolution"),
            "the error names the heading and the required Clause: {}",
            violations[0].message
        );
    }

    #[test]
    fn a_reserved_heading_without_a_pin_fails() {
        use crate::Rule;
        let unpinned = "---\nkind: agreement\nquestionnaire: [entity__company]\n---\n## IX. Governing law and dispute resolution\n\nThe parties will arbitrate.\n";
        let violations = super::ClauseReferences::default().lint(&crate::SourceFile {
            path: "dpa.md".into(),
            contents: unpinned.into(),
        });
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].code, "N132");
        assert!(violations[0].message.contains("dispute_resolution"));
        // An ordinary caption reserves nothing.
        let ordinary = unpinned.replace(
            "Governing law and dispute resolution",
            "Term and termination",
        );
        assert!(super::ClauseReferences::default()
            .lint(&crate::SourceFile {
                path: "dpa.md".into(),
                contents: ordinary
            })
            .is_empty());
    }

    /// A compiled snapshot carries expanded wording and `clause_sources:`
    /// provenance and is never re-checked, so retired-version snapshots
    /// stay readable (ENG-1092).
    #[test]
    fn compiled_snapshots_are_not_rechecked_for_reserved_headings() {
        use crate::Rule;
        let authored = dpa(
            "## IX. Governing law and dispute resolution",
            "{{> dispute}}",
            "dispute_resolution@1",
        );
        let compiled = compile(&authored.contents).unwrap();
        assert!(compiled.contains("clause_sources:"));
        assert!(super::ClauseReferences::default()
            .lint(&crate::SourceFile {
                path: authored.path.clone(),
                contents: compiled
            })
            .is_empty());
    }

    /// Reserved captions are data on the library versions; the same caption
    /// may be declared again by another version of the same code, but a
    /// cross-code claim fails library parsing.
    #[test]
    fn reserved_captions_may_claim_only_one_clause_code() {
        let clause = |code: &str, caption: &str| super::LibraryClause {
            retired_at: None,
            code: code.into(),
            version: 1,
            kind: "agreement".into(),
            inputs: BTreeMap::new(),
            body: "Body.".into(),
            heading: Some(caption.into()),
            aliases: Vec::new(),
        };
        assert!(super::check_reserved_headings(&[
            clause("dispute_resolution", "Governing law"),
            clause("dispute_resolution", "governing law:"),
        ])
        .is_ok());
        let error = super::check_reserved_headings(&[
            clause("dispute_resolution", "Choice of law"),
            clause("other_code", "Choice of law"),
        ])
        .unwrap_err();
        assert!(
            error.contains("Choice of law")
                && error.contains("dispute_resolution")
                && error.contains("other_code"),
            "the refusal names the caption and both codes: {error}"
        );
    }
}
