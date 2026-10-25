//! Local redaction check for CLI feedback drafted for an outside tracker.
//!
//! The draft never leaves this process. The scan errs toward removing: a false
//! positive costs a re-edit, a false negative cannot be undone.

use regex::Regex;

/// One span of a draft that must be removed or replaced by a placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// 1-based line of the match.
    pub line: usize,
    /// What kind of identifier matched.
    pub kind: &'static str,
    /// Placeholder to write instead.
    pub placeholder: &'static str,
}

const PATTERNS: &[(&str, &str, &str)] = &[
    (
        "email address",
        "<email>",
        r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+",
    ),
    (
        "phone number",
        "<phone>",
        r"(\+?\d{1,2}[\s.-]?)?\(?\d{3}\)?[\s.-]\d{3}[\s.-]\d{4}",
    ),
    (
        "claim, case, or docket number",
        "<case number>",
        r"(?i)\b(claim|case|docket|matter)\s*(no\.?|number|#)?\s*:?\s*[A-Za-z]*[-:]?\d[\w:-]*\d",
    ),
    (
        "case number",
        "<case number>",
        r"\b\d{1,2}:\d{2}-[a-z]{2}-\d{3,6}\b",
    ),
    (
        "wallet, hash, or long identifier",
        "<identifier>",
        r"\b(0x)?[0-9a-fA-F]{24,}\b|\b[1-9A-HJ-NP-Za-km-z]{32,}\b",
    ),
    (
        "street address",
        "<address>",
        r"(?i)\b\d{1,6}\s+(?:[A-Za-z0-9.]+\s+){1,4}(street|st|avenue|ave|road|rd|boulevard|blvd|lane|ln|drive|dr|court|ct|way|place|pl)\b\.?",
    ),
];

/// Scan `text` for personal or matter identifiers. `terms` are exact names the
/// caller knows to be private (Project codes, client and counterparty names);
/// they match case-insensitively on word boundaries.
#[must_use]
pub fn scan(text: &str, terms: &[String]) -> Vec<Finding> {
    let mut found = Vec::new();
    let generic: Vec<(&str, &str, Regex)> = PATTERNS
        .iter()
        .filter_map(|(k, p, re)| Regex::new(re).ok().map(|r| (*k, *p, r)))
        .collect();
    let named: Vec<Regex> = terms
        .iter()
        .filter(|t| !t.trim().is_empty())
        .filter_map(|t| Regex::new(&format!(r"(?i)\b{}\b", regex::escape(t.trim()))).ok())
        .collect();
    for (index, line) in text.lines().enumerate() {
        for (kind, placeholder, re) in &generic {
            if re.is_match(line) {
                found.push(Finding {
                    line: index + 1,
                    kind,
                    placeholder,
                });
            }
        }
        for re in &named {
            if re.is_match(line) {
                found.push(Finding {
                    line: index + 1,
                    kind: "private name or Project code",
                    placeholder: "<client>",
                });
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_behaviour_passes_unchanged() {
        let body =
            "`navigator doc list --json` exits 0 but prints nothing when the folder is empty.";
        assert!(scan(body, &["acme-claim".into()]).is_empty());
    }

    #[test]
    fn email_phone_and_case_numbers_are_refused() {
        for body in [
            "reply to jane.doe@example.com",
            "call 702-555-0142 first",
            "see Case No. 2:24-cv-01234 for the failure",
            "docket 24-10001 was rejected",
        ] {
            assert!(!scan(body, &[]).is_empty(), "{body}");
        }
    }

    #[test]
    fn hex_ids_and_street_addresses_are_refused() {
        assert!(!scan("tx 0x9f86d081884c7d659a2feaa0c55ad015a3bf4f1b", &[]).is_empty());
        assert!(!scan("mail goes to 1234 Main Street", &[]).is_empty());
    }

    #[test]
    fn known_names_and_project_codes_match_by_word() {
        let terms = vec!["Cruller".to_string(), "sample-estate".to_string()];
        let hits = scan("line one\nthe Cruller file", &terms);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
        assert!(!scan("fails for sample-estate project", &terms).is_empty());
        assert!(scan("crullers are pastries", &terms).is_empty());
    }
}
