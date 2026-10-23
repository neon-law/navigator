//! Pure Notation template evaluation shared by document assembly and the
//! browser's live preview.

use std::collections::BTreeMap;

const FOR_OPEN: &str = "{{#for ";
const FOR_CLOSE: &str = "{{/for}}";
const IF_OPEN: &str = "{{#if ";
const IF_CLOSE: &str = "{{/if}}";
const ELSE_IF_OPEN: &str = "{{else if ";
const ELSE: &str = "{{else}}";

/// The text a declared Signer Box prints when no answer fills it: the
/// marker `word::paper::SIGNER_BOX` the PDF and Word renders draw as an
/// empty rule. The bytes are repeated here because this crate also compiles
/// to the browser bundle and carries no render dependency; `views` pins the
/// copies together.
pub const SIGNER_BOX: &str = "<!-- signer-box -->";

#[must_use]
pub fn fill(body: &str, context: &BTreeMap<String, String>) -> String {
    fill_with_display(body, context, context)
}

/// Evaluate `body` against `context`, printing each declared Signer Box in
/// `boxes` that has no answer as [`SIGNER_BOX`]. An answered box prints its
/// answer like any other state. The marker reaches only the display map, so
/// a `{{#if …}}` on a box state still reads the answer, not the rule.
#[must_use]
pub fn fill_signer_boxes<'a>(
    body: &str,
    context: &BTreeMap<String, String>,
    boxes: impl IntoIterator<Item = &'a str>,
) -> String {
    let display = with_signer_boxes(context, boxes);
    fill_with_display(body, context, &display)
}

/// `context` with [`SIGNER_BOX`] under every state in `boxes` that has no
/// answer, or an answer that is only whitespace.
#[must_use]
pub fn with_signer_boxes<'a>(
    context: &BTreeMap<String, String>,
    boxes: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<String, String> {
    let mut display = context.clone();
    for state in boxes {
        let answered = display
            .get(state)
            .is_some_and(|answer| !answer.trim().is_empty());
        if !answered {
            display.insert(state.to_string(), SIGNER_BOX.to_string());
        }
    }
    display
}

#[must_use]
pub fn fill_with_display(
    body: &str,
    context: &BTreeMap<String, String>,
    display: &BTreeMap<String, String>,
) -> String {
    let conditional = expand_conditionals(body, context);
    let expanded = expand_loops(&conditional, context);
    let mut filled = expanded;
    for (key, value) in display {
        filled = filled.replace(&format!("{{{{{key}}}}}"), value);
    }
    filled
}

fn expand_conditionals(body: &str, context: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some(start) = rest.find(IF_OPEN) {
        let after_open = &rest[start + IF_OPEN.len()..];
        let Some(header_len) = after_open.find("}}") else {
            break;
        };
        let condition = after_open[..header_len].trim();
        let block_start = start + IF_OPEN.len() + header_len + 2;
        let Some((branches, close_end)) = conditional_branches(rest, block_start, condition) else {
            break;
        };
        out.push_str(&rest[..start]);
        if let Some((_, block)) = branches.iter().find(|(condition, _)| {
            condition
                .as_deref()
                .is_none_or(|condition| matches_condition(condition, context))
        }) {
            out.push_str(&expand_conditionals(block, context));
        }
        rest = &rest[close_end..];
    }
    out.push_str(rest);
    out
}

fn matches_condition(condition: &str, context: &BTreeMap<String, String>) -> bool {
    let (key, expected) = condition
        .split_once('=')
        .map_or((condition, None), |(key, value)| {
            (key.trim(), Some(value.trim()))
        });
    let actual = context.get(key).map(String::as_str).unwrap_or_default();
    expected.map_or_else(
        || !actual.is_empty() && !matches!(actual, "false" | "no" | "0"),
        |expected| actual == expected,
    )
}

type ConditionalBranch<'a> = (Option<&'a str>, &'a str);

fn conditional_branches<'a>(
    source: &'a str,
    block_start: usize,
    condition: &'a str,
) -> Option<(Vec<ConditionalBranch<'a>>, usize)> {
    let bytes = source.as_bytes();
    let mut branches = Vec::new();
    let mut active_condition = Some(condition);
    let mut branch_start = block_start;
    let mut depth = 1usize;
    let mut index = block_start;
    while index < bytes.len() {
        if bytes[index..].starts_with(IF_OPEN.as_bytes()) {
            depth += 1;
            index += IF_OPEN.len();
        } else if bytes[index..].starts_with(IF_CLOSE.as_bytes()) {
            depth -= 1;
            if depth == 0 {
                branches.push((active_condition, &source[branch_start..index]));
                return Some((branches, index + IF_CLOSE.len()));
            }
            index += IF_CLOSE.len();
        } else if depth == 1 && bytes[index..].starts_with(ELSE_IF_OPEN.as_bytes()) {
            branches.push((active_condition, &source[branch_start..index]));
            let condition_start = index + ELSE_IF_OPEN.len();
            let condition_end = source[condition_start..].find("}}")? + condition_start;
            active_condition = Some(source[condition_start..condition_end].trim());
            branch_start = condition_end + 2;
            index = branch_start;
        } else if depth == 1 && bytes[index..].starts_with(ELSE.as_bytes()) {
            branches.push((active_condition, &source[branch_start..index]));
            active_condition = None;
            branch_start = index + ELSE.len();
            index = branch_start;
        } else {
            index += 1;
        }
    }
    None
}

fn expand_loops(body: &str, context: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some(start) = rest.find(FOR_OPEN) {
        let after_open = &rest[start + FOR_OPEN.len()..];
        let Some(header_len) = after_open.find("}}") else {
            break;
        };
        let header = after_open[..header_len].trim();
        let block_start = start + FOR_OPEN.len() + header_len + 2;
        let Some(close_rel) = matching_close(&rest[block_start..], FOR_OPEN, FOR_CLOSE) else {
            break;
        };
        let block = &rest[block_start..block_start + close_rel];
        let close_end = block_start + close_rel + FOR_CLOSE.len();
        out.push_str(&rest[..start]);
        if let Some((var, state)) = header.split_once(" in ") {
            out.push_str(&render_loop(var.trim(), state.trim(), block, context));
        }
        rest = &rest[close_end..];
    }
    out.push_str(rest);
    out
}

fn matching_close(source: &str, open: &str, close: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let open = open.as_bytes();
    let close = close.as_bytes();
    let mut depth = 1usize;
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index..].starts_with(open) {
            depth += 1;
            index += open.len();
        } else if bytes[index..].starts_with(close) {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
            index += close.len();
        } else {
            index += 1;
        }
    }
    None
}

fn render_loop(var: &str, state: &str, block: &str, context: &BTreeMap<String, String>) -> String {
    let Some(json) = context.get(state) else {
        return String::new();
    };
    let rows: Vec<BTreeMap<String, serde_json::Value>> =
        serde_json::from_str(json).unwrap_or_default();
    let mut out = String::new();
    for row in &rows {
        let mut piece = block.to_string();
        for (part, value) in row {
            let needle = format!("{{{{{var}.{part}}}}}");
            piece = piece.replace(&needle, &json_scalar(value));
        }
        out.push_str(&expand_loops(&piece, context));
    }
    out
}

fn json_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{fill_signer_boxes, with_signer_boxes, SIGNER_BOX};

    fn ctx(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// LAW-141: an answered box prints its answer; a signer-completed box
    /// with no answer prints the rule marker; a state that is not a box is
    /// left to the ordinary fill.
    #[test]
    fn an_answered_box_prints_its_answer_and_an_unanswered_one_the_rule() {
        let body = "Shares: {{custom_text__shares}}; initials ({{custom_text__initials_l}}); \
                    fee {{custom_usd__fee}}.";
        let filled = fill_signer_boxes(
            body,
            &ctx(&[
                ("custom_text__shares", "1,000"),
                ("custom_text__initials_l", "  "),
            ]),
            ["custom_text__shares", "custom_text__initials_l"],
        );
        assert_eq!(
            filled,
            format!("Shares: 1,000; initials ({SIGNER_BOX}); fee {{{{custom_usd__fee}}}}.")
        );
    }

    #[test]
    fn a_box_marker_does_not_satisfy_a_conditional_on_its_state() {
        let body = "{{#if custom_text__shares}}Shares: {{custom_text__shares}}{{/if}}end";
        assert_eq!(
            fill_signer_boxes(body, &ctx(&[]), ["custom_text__shares"]),
            "end"
        );
        let display = with_signer_boxes(&ctx(&[("other", "x")]), ["custom_text__shares"]);
        assert_eq!(display["custom_text__shares"], SIGNER_BOX);
        assert_eq!(display["other"], "x");
    }

    #[test]
    fn conditional_branches_select_the_first_matching_body_or_else() {
        let body = "{{#if jurisdiction__law=California}}California terms.{{else if jurisdiction__law=Nevada}}Nevada terms.{{else}}Other law terms.{{/if}}";
        assert_eq!(
            super::fill(body, &ctx(&[("jurisdiction__law", "California")])),
            "California terms."
        );
        assert_eq!(
            super::fill(body, &ctx(&[("jurisdiction__law", "Nevada")])),
            "Nevada terms."
        );
        assert_eq!(
            super::fill(body, &ctx(&[("jurisdiction__law", "Washington")])),
            "Other law terms."
        );
        assert_eq!(super::fill(body, &ctx(&[])), "Other law terms.");
    }

    #[test]
    fn nested_conditional_branches_match_their_own_else_markers() {
        let body = "{{#if outer=yes}}A{{#if inner=yes}}B{{else}}C{{/if}}D{{else}}E{{/if}}";
        assert_eq!(
            super::fill(body, &ctx(&[("outer", "yes"), ("inner", "no")])),
            "ACD"
        );
        assert_eq!(super::fill(body, &ctx(&[("outer", "no")])), "E");
    }

    #[test]
    fn the_marker_is_the_one_the_renders_draw() {
        assert_eq!(SIGNER_BOX, word::paper::SIGNER_BOX);
    }
}
