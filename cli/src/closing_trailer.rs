//! The closing-trailer check: a pull request may not close an issue while its
//! own body says a rollout, an apply, or a caller pin is still pending.
//!
//! A `Closes ENG-12` line in a pull-request body tells Linear to complete the
//! issue on merge. When the same body says the work that proves the issue is
//! still to happen — a rollout to other repositories, a live apply, callers
//! still pinned to an older tag — merging moves the issue to Done with that
//! work left invisible. The right trailer then is `Refs ENG-12`, with the
//! remaining step and its owner named in the body.
//!
//! The check reads the body and nothing else: no issue tracker, no network,
//! no secret. It is deliberately narrow. A sentence counts only when it names
//! one of the three steps *and* says that step has not happened, so a body
//! that mentions a finished rollout, or explains what `apply` does, passes.

/// The steps whose absence keeps an issue open after its code merges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingStep {
    Rollout,
    Apply,
    CallerPin,
}

impl PendingStep {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rollout => "a rollout",
            Self::Apply => "an apply",
            Self::CallerPin => "a caller pin",
        }
    }
}

/// One refused closing trailer: the issues it would close and the sentence
/// that says their work is unfinished.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Refusal {
    pub issues: Vec<String>,
    pub step: PendingStep,
    pub sentence: String,
}

impl Refusal {
    /// The message an author reads, naming the issues, the step, and the
    /// trailer to use instead.
    pub(crate) fn message(&self) -> String {
        let ids = self.issues.join(", ");
        format!(
            "the body closes {ids} but says {} is still pending: \"{}\". Use `Refs {ids}` instead, and name the \
             remaining step and who owns it in the body.",
            self.step.as_str(),
            self.sentence,
        )
    }
}

/// Every refusal for `body`. Empty when the body closes nothing, or when no
/// sentence outside a code block or quote says a rollout, apply, or caller
/// pin is still pending.
pub(crate) fn refusals(body: &str) -> Vec<Refusal> {
    let mut closed = Vec::new();
    let mut prose = Vec::new();
    let mut fenced = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || trimmed.starts_with('>') {
            continue;
        }
        if let Some(ids) = closing_trailer_ids(trimmed) {
            closed.extend(ids);
            continue;
        }
        prose.push(trimmed);
    }
    if closed.is_empty() {
        return Vec::new();
    }
    closed.dedup();

    let mut found = Vec::new();
    for line in prose {
        for sentence in sentences(line) {
            let lower = sentence.to_ascii_lowercase();
            if !says_pending(&lower) {
                continue;
            }
            if let Some(step) = named_step(&lower) {
                found.push(Refusal {
                    issues: closed.clone(),
                    step,
                    sentence: sentence.to_string(),
                });
            }
        }
    }
    found
}

/// The pull-request body from a GitHub Actions event payload, when the event
/// is a pull request. `None` for any other event, or a payload with no body.
pub(crate) fn pull_request_body(event_name: &str, payload: &str) -> Option<String> {
    if !event_name.starts_with("pull_request") {
        return None;
    }
    let event: serde_json::Value = serde_json::from_str(payload).ok()?;
    event
        .get("pull_request")?
        .get("body")?
        .as_str()
        .map(str::to_string)
}

/// The refusals for the pull request this CI run is gating, read from the
/// runner's own event payload (`GITHUB_EVENT_NAME`, `GITHUB_EVENT_PATH`).
/// Empty outside a pull-request run.
pub(crate) fn refusals_for_this_pull_request() -> Vec<Refusal> {
    let (Ok(name), Ok(path)) = (
        std::env::var("GITHUB_EVENT_NAME"),
        std::env::var("GITHUB_EVENT_PATH"),
    ) else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|payload| pull_request_body(&name, &payload))
        .map(|body| refusals(&body))
        .unwrap_or_default()
}

/// Linear's closing magic words and their inflections.
const CLOSING_WORDS: &[&str] = &[
    "close",
    "closes",
    "closed",
    "closing",
    "fix",
    "fixes",
    "fixed",
    "fixing",
    "resolve",
    "resolves",
    "resolved",
    "resolving",
    "complete",
    "completes",
    "completed",
    "completing",
    "implement",
    "implements",
    "implemented",
    "implementing",
];

/// The issue ids on a closing-trailer line — a closing word followed only by
/// a list of issue ids — or `None` when the line is anything else.
fn closing_trailer_ids(line: &str) -> Option<Vec<String>> {
    let (word, rest) = line.split_once(char::is_whitespace)?;
    let word = word.trim_end_matches(':').to_ascii_lowercase();
    if !CLOSING_WORDS.contains(&word.as_str()) {
        return None;
    }
    let ids: Vec<String> = rest
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty() && *part != "and")
        .map(str::to_string)
        .collect();
    (!ids.is_empty() && ids.iter().all(|id| is_issue_id(id))).then_some(ids)
}

/// `ENG-12`: an uppercase team key, a hyphen, and a number.
fn is_issue_id(candidate: &str) -> bool {
    let Some((team, number)) = candidate.split_once('-') else {
        return false;
    };
    !team.is_empty()
        && team.chars().all(|c| c.is_ascii_uppercase())
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

/// A line split at sentence ends, so a pending clause is reported alone.
fn sentences(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let ends = matches!(b, b'.' | b'!' | b'?' | b';')
            && bytes.get(i + 1).is_none_or(u8::is_ascii_whitespace);
        if ends {
            let sentence = line[start..=i].trim();
            if !sentence.is_empty() {
                out.push(sentence);
            }
            start = i + 1;
        }
    }
    let tail = line[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

/// Phrases that say a step has not happened yet.
const PENDING_MARKERS: &[&str] = &[
    "not yet",
    "not been",
    "not performed",
    "not run",
    "not done",
    "not happened",
    "pending",
    "remains",
    "remaining",
    "still need",
    "still to",
    "still pin",
    "outstanding",
    "follow-up",
    "follow up",
    "has not",
    "have not",
    "hasn't",
    "haven't",
    "was not",
    "were not",
    "wasn't",
    "weren't",
    "did not",
    "didn't",
    "to be applied",
    "to be rolled",
    "after merge",
    "after this merges",
    "todo",
];

fn says_pending(lower: &str) -> bool {
    PENDING_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Which of the three steps a lower-cased sentence names, if any.
fn named_step(lower: &str) -> Option<PendingStep> {
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .collect();
    let has = |set: &[&str]| words.iter().any(|w| set.contains(w));
    let follows = |first: &str| {
        words
            .windows(2)
            .any(|pair| pair[0] == first && pair[1] == "out")
    };

    if has(&["rollout", "rollouts", "roll-out"]) || follows("roll") || follows("rolled") {
        return Some(PendingStep::Rollout);
    }
    if has(&["caller", "callers"]) && has(&["pin", "pins", "pinned", "pinning"]) {
        return Some(PendingStep::CallerPin);
    }
    if has(&["apply", "applies", "applied"]) {
        return Some(PendingStep::Apply);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{pull_request_body, refusals, PendingStep};

    /// The shape of a closing PR whose rollout to other repositories is
    /// left for later.
    const ROLLOUT_PENDING: &str = "\
## What

Adds the corrected origin gate to the reusable workflow.

Project repositories pick it up only when each updates its gate pin; that rollout has not happened yet.

Closes ENG-101, ENG-102
";

    /// The shape of a closing PR whose live apply is still to run.
    const APPLY_PENDING: &str = "\
Adds push protection to the GitHub setup reconcile.

The live apply against the three organizations was not performed.

Closes ENG-103
";

    /// The shape of a closing PR whose callers still pin an older release.
    const CALLER_PIN_PENDING: &str = "\
Lands the seeds job in the central reusable gate.

Project callers still pin the previous release tag, so none of them run it yet.

Fixes ENG-104
";

    #[test]
    fn a_pending_rollout_refuses_the_closing_trailer() {
        let found = refusals(ROLLOUT_PENDING);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].step, PendingStep::Rollout);
        assert_eq!(found[0].issues, ["ENG-101", "ENG-102"]);
        let message = found[0].message();
        assert!(message.contains("ENG-101, ENG-102"), "{message}");
        assert!(
            message.contains("that rollout has not happened yet"),
            "{message}"
        );
        assert!(
            message.contains("Use `Refs ENG-101, ENG-102` instead"),
            "{message}"
        );
        assert!(message.contains("who owns it"), "{message}");
    }

    #[test]
    fn a_pending_live_apply_refuses_the_closing_trailer() {
        let found = refusals(APPLY_PENDING);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].step, PendingStep::Apply);
        assert_eq!(found[0].issues, ["ENG-103"]);
        assert_eq!(
            found[0].sentence,
            "The live apply against the three organizations was not performed."
        );
    }

    #[test]
    fn callers_still_pinned_refuse_the_closing_trailer() {
        let found = refusals(CALLER_PIN_PENDING);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].step, PendingStep::CallerPin);
        assert_eq!(found[0].issues, ["ENG-104"]);
    }

    #[test]
    fn the_same_bodies_pass_with_a_refs_trailer() {
        for body in [ROLLOUT_PENDING, APPLY_PENDING, CALLER_PIN_PENDING] {
            let refs = body.replace("Closes ", "Refs ").replace("Fixes ", "Refs ");
            assert!(refusals(&refs).is_empty(), "{refs}");
        }
    }

    #[test]
    fn a_code_only_close_passes_even_when_it_mentions_the_steps() {
        let body = "\
## What

Adds `record_signer_completion` and a webhook branch for it.

The rollout to every Project finished last week, and `ops github setup` applies the ruleset on its next run.

## Tests

`cargo test -p store` passes.

Closes ENG-105
";
        assert!(refusals(body).is_empty());
    }

    #[test]
    fn code_blocks_and_quotes_are_not_read_as_the_author_saying_so() {
        let body = "\
Teaches the guard its fixtures.

```text
The rollout has not happened yet.
```

> The live apply was not performed.

Closes ENG-106
";
        assert!(refusals(body).is_empty());
    }

    #[test]
    fn every_closing_word_and_list_shape_is_a_trailer() {
        for trailer in [
            "Closes ENG-1",
            "closes ENG-1",
            "Closes: ENG-1",
            "Fixed ENG-1",
            "Resolves ENG-1, ENG-2",
            "Completes ENG-1 and ENG-2",
            "Implements LAW-9",
        ] {
            let body = format!("The rollout is still pending.\n\n{trailer}\n");
            assert_eq!(refusals(&body).len(), 1, "{trailer}");
        }
        for not_a_trailer in [
            "Refs ENG-1",
            "Part of ENG-1",
            "Closes the gap between ENG-1 and the gate",
            "Fixes the rollout",
        ] {
            let body = format!("The rollout is still pending.\n\n{not_a_trailer}\n");
            assert!(refusals(&body).is_empty(), "{not_a_trailer}");
        }
    }

    #[test]
    fn the_body_comes_from_a_pull_request_event_only() {
        let payload = r#"{"pull_request":{"body":"Closes ENG-1"}}"#;
        assert_eq!(
            pull_request_body("pull_request", payload).as_deref(),
            Some("Closes ENG-1")
        );
        assert_eq!(
            pull_request_body("pull_request_target", payload).as_deref(),
            Some("Closes ENG-1")
        );
        assert_eq!(pull_request_body("push", payload), None);
        assert_eq!(
            pull_request_body("pull_request", r#"{"pull_request":{"body":null}}"#),
            None
        );
        assert_eq!(pull_request_body("pull_request", "not json"), None);
    }
}
