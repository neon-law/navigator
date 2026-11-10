//! Route-sensitive body reads share the questionnaire's authored choice keys.
//! A read is safe only when no rendering route can reach END without asking it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Deserialize;

use crate::{frontmatter, line_byte_range, SourceFile, Violation};

type Routes = BTreeMap<String, BTreeMap<String, String>>;

#[derive(Deserialize)]
struct Definition {
    questionnaire: Routes,
    #[serde(default)]
    choices: Routes,
}

impl Definition {
    // Route/shape refusals belong to N118 and the parse gate. Do not
    // search an invalid graph, including cycles whose walks are unbounded.
    fn valid_routes(&self) -> bool {
        self.questionnaire.contains_key("BEGIN")
            && self.questionnaire.contains_key("END")
            && self
                .questionnaire
                .values()
                .flat_map(BTreeMap::values)
                .all(|next| self.questionnaire.contains_key(next))
            && crate::f118::route_finding(&self.questionnaire, &self.choices).is_none()
    }
}

#[derive(Default)]
struct Branch<'a> {
    previous: Vec<&'a str>,
    current: Option<&'a str>,
}

/// Inspect both bare values and data paths after expanding reusable Clauses.
pub(crate) fn findings(file: &SourceFile, code: &'static str) -> Vec<Violation> {
    let Ok(compiled) = crate::clauses::compile(&file.contents) else {
        return Vec::new();
    };
    let Some((yaml, body)) = frontmatter::split(&compiled) else {
        return Vec::new();
    };
    let Ok(definition) = crate::questionnaire::deserialize_frontmatter::<Definition>(yaml) else {
        return Vec::new();
    };
    if !definition.valid_routes() {
        return Vec::new();
    }
    let mut branches: Vec<Branch<'_>> = Vec::new();
    let mut findings = Vec::new();
    let mut reported = BTreeSet::new();
    let mut offset = 0;
    while let Some(start) = body[offset..].find("{{").map(|i| i + offset) {
        let Some(end) = body[start + 2..].find("}}").map(|i| i + start + 2) else {
            break;
        };
        let token = body[start + 2..end].trim();
        offset = end + 2;
        if let Some(condition) = token.strip_prefix("#if ") {
            branches.push(Branch {
                previous: Vec::new(),
                current: Some(condition.trim()),
            });
            continue;
        }
        if token == "else" || token.starts_with("else if ") {
            if let Some(branch) = branches.last_mut() {
                if let Some(current) = branch.current.take() {
                    branch.previous.push(current);
                }
                branch.current = token.strip_prefix("else if ").map(str::trim);
            }
            continue;
        }
        if token == "/if" {
            branches.pop();
            continue;
        }
        let (state, owner) = if let Some(iterator) = token.strip_prefix("#for ") {
            let Some((_, state)) = iterator.split_once(" in ") else {
                continue;
            };
            (state.trim(), "N115")
        } else if let Some((head, tail)) = token.split_once('.') {
            if matches!(
                tail.split('.').next(),
                Some("signature" | "initials" | "date")
            ) {
                continue;
            }
            (head, "N115")
        } else {
            (token, "N120")
        };
        if owner != code || !definition.questionnaire.contains_key(state) {
            continue;
        }
        let predicates: Vec<(&str, bool)> = branches
            .iter()
            .flat_map(|branch| {
                branch
                    .previous
                    .iter()
                    .map(|condition| (*condition, false))
                    .chain(branch.current.map(|condition| (condition, true)))
            })
            .filter(|(condition, _)| {
                definition
                    .questionnaire
                    .contains_key(condition_state(condition))
            })
            .collect();
        if missing_route(&definition, state, &predicates) && reported.insert(state.to_string()) {
            // Expanded Clause offsets need not match the authored source;
            // report those at file level instead of inventing a body line.
            let line = if compiled == file.contents {
                compiled[..compiled.len() - body.len() + start]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count()
                    + 1
            } else {
                1
            };
            findings.push(Violation {
                code,
                path: file.path.clone(),
                line,
                range: line_byte_range(&file.contents, line),
                message: format!("Body read `{state}` can render on a questionnaire route that does not collect it; ask it on every rendering route or guard it with the matching choice"),
            });
        }
    }
    findings
}

fn condition_parts(condition: &str) -> (&str, Option<&str>) {
    condition
        .split_once('=')
        .map_or((condition.trim(), None), |(key, value)| {
            (key.trim(), Some(value.trim()))
        })
}

fn condition_state(condition: &str) -> &str {
    condition_parts(condition)
        .0
        .split('.')
        .next()
        .unwrap_or_default()
}

fn matches(condition: &str, value: &str) -> bool {
    let (_, expected) = condition_parts(condition);
    expected.map_or_else(
        || !value.is_empty() && !matches!(value, "false" | "no" | "0"),
        |expected| value == expected,
    )
}

/// Guard questions reachable without passing through the body read.
fn reachable_guards<'a>(
    definition: &'a Definition,
    read: &str,
    required: &BTreeSet<&'a str>,
) -> BTreeMap<&'a str, BTreeSet<&'a str>> {
    // A required guard question already bypassed cannot be recovered later.
    // Prune it at the next merge, preventing independent diamonds from
    // multiplying visited subsets across the rest of the graph.
    let mut predecessors: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (state, routes) in &definition.questionnaire {
        if state == read {
            continue;
        }
        for next in routes.values().filter(|next| next.as_str() != read) {
            predecessors
                .entry(next.as_str())
                .or_default()
                .push(state.as_str());
        }
    }
    let mut reachable: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for target in required {
        if *target == read {
            continue;
        }
        let mut ancestors = vec![*target];
        let mut seen = BTreeSet::new();
        while let Some(state) = ancestors.pop() {
            if !seen.insert(state) {
                continue;
            }
            reachable.entry(state).or_default().insert(*target);
            ancestors.extend(predecessors.get(state).into_iter().flatten().copied());
        }
    }
    reachable
}

/// Search only routes omitting the read. Predicates restrict each question's
/// possible answers; the search remembers predicates that cannot match absent
/// answers. Memoization merges converging routes without enumerating every walk.
fn missing_route(definition: &Definition, read: &str, predicates: &[(&str, bool)]) -> bool {
    let required: BTreeSet<&str> = predicates
        .iter()
        .filter_map(|(condition, expected)| {
            let state = condition_state(condition);
            (matches(condition, "") != *expected).then_some(state)
        })
        .collect();
    let reachable = reachable_guards(definition, read, &required);
    let mut pending = VecDeque::from([("BEGIN", BTreeSet::new())]);
    let mut seen = BTreeSet::new();
    while let Some((state, mut visited)) = pending.pop_front() {
        if state == read || !seen.insert((state, visited.clone())) {
            continue;
        }
        if required.iter().any(|needed| {
            !visited.contains(needed)
                && !reachable
                    .get(state)
                    .is_some_and(|later| later.contains(needed))
        }) {
            continue;
        }

        if state == "END" {
            if required.is_subset(&visited) {
                return true;
            }
            continue;
        }
        let Some(routes) = definition.questionnaire.get(state) else {
            continue;
        };
        let constraints: Vec<_> = predicates
            .iter()
            .filter(|(condition, _)| condition_parts(condition).0 == state)
            .collect();
        if required.contains(state) {
            visited.insert(state);
        }
        let closed_choices = crate::f118::routes_by_answer(state)
            .then(|| {
                let role = state.split_once("__").map_or(state, |(_, role)| role);
                definition.choices.get(role)
            })
            .flatten();
        let mut values: BTreeSet<&str> = if let Some(choices) = closed_choices {
            choices.keys().map(String::as_str).collect()
        } else {
            let mut values: BTreeSet<_> = routes
                .keys()
                .filter(|key| key.as_str() != "_")
                .map(String::as_str)
                .collect();
            values.extend(["", "false", "no", "0"]);
            values.extend(
                constraints
                    .iter()
                    .filter_map(|(condition, _)| condition_parts(condition).1),
            );
            // A fresh truthy value represents all answers not named by a
            // predicate or route; it must not accidentally equal a template key.
            values
        };
        let mut other = "_".to_string();
        while values.contains(other.as_str()) || routes.contains_key(&other) {
            other.push('_');
        }
        if closed_choices.is_none() {
            values.insert(&other);
        }
        for value in values {
            if constraints
                .iter()
                .any(|(condition, expected)| matches(condition, value) != *expected)
            {
                continue;
            }
            if let Some(next) = routes.get(value).or_else(|| routes.get("_")) {
                pending.push_back((next.as_str(), visited.clone()));
            }
        }
    }
    false
}
