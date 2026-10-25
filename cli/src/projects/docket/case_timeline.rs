//! Case Timeline records: one Docket Entry row rendered as the file name
//! `DKT NNNN - <title>.yaml` and the deterministic YAML that file holds.
//!
//! Pure rendering only. The record carries the entry's number, kind, title,
//! party, filed/served date, scheduled date, supersedes link, and the slug of
//! its document, if any. It never carries document bytes or a pointer.
//!
//! The row is the JSON the portal API returns for a Docket Entry
//! (`entry_number`, `kind`, `title`, `party`, `filed_or_served_on`,
//! `scheduled_on`, `supersedes`). `document_slug` is the Document Identity of
//! the linked document, which the caller resolves from the row's document
//! and adds before rendering; an entry without one is the record that the
//! entry exists.

use serde::Serialize;
use serde_json::Value;

/// A rendered Case Timeline file: its name and its YAML contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub file_name: String,
    pub yaml: String,
}

/// Keys serialize in declaration order, so the bytes are stable.
#[derive(Serialize)]
struct Fields {
    number: String,
    kind: String,
    title: String,
    party: Option<String>,
    filed_or_served_on: Option<String>,
    scheduled_on: Option<String>,
    supersedes: Option<String>,
    document: Option<String>,
}

fn text(row: &Value, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `4` becomes `0004` and `29-1` becomes `0029-1`: the leading digits are
/// zero-padded to four and an attachment suffix is kept as the court prints it.
fn dkt_number(entry_number: &str) -> Result<String, String> {
    let (lead, suffix) = match entry_number.split_once('-') {
        Some((lead, suffix)) => (lead, Some(suffix)),
        None => (entry_number, None),
    };
    if lead.is_empty() || !lead.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "entry_number `{entry_number}` does not start with digits"
        ));
    }
    Ok(match suffix {
        Some(suffix) => format!("{lead:0>4}-{suffix}"),
        None => format!("{lead:0>4}"),
    })
}

/// Minimal path-safety: each of `/ \ : * ? " < > |` and every control
/// character becomes `-`, runs of whitespace collapse to one space, and
/// leading or trailing dots and spaces are dropped. An empty result reads
/// `Untitled`. Nothing else about the title changes; the entry's own `title`
/// field in the YAML stays verbatim.
fn safe_title(title: &str) -> String {
    let replaced: String = title
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '-'
            } else {
                c
            }
        })
        .collect();
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|c| c == '.' || c == ' ');
    if trimmed.is_empty() {
        "Untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Render one Docket Entry row into its Case Timeline file.
pub fn render(row: &Value) -> Result<Record, String> {
    let entry_number = text(row, "entry_number").ok_or("entry has no entry_number")?;
    let title = text(row, "title").ok_or("entry has no title")?;
    let kind = text(row, "kind").ok_or("entry has no kind")?;
    let number = dkt_number(&entry_number)?;
    let file_name = format!("DKT {number} - {}.yaml", safe_title(&title));
    let fields = Fields {
        number: entry_number,
        kind,
        title,
        party: text(row, "party"),
        filed_or_served_on: text(row, "filed_or_served_on"),
        scheduled_on: text(row, "scheduled_on"),
        supersedes: text(row, "supersedes"),
        document: text(row, "document_slug"),
    };
    let yaml = serde_yaml::to_string(&fields).map_err(|error| error.to_string())?;
    Ok(Record { file_name, yaml })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_hearing_with_a_document_renders_the_golden_file() {
        let record = render(&json!({
            "entry_number": "4",
            "kind": "hearing",
            "title": "Notice of Hearing",
            "party": "Court",
            "filed_or_served_on": "2026-03-05",
            "scheduled_on": "2026-04-01T16:00:00Z",
            "document_slug": "Docket Filings/DKT 0004 - Notice of Hearing"
        }))
        .unwrap();
        assert_eq!(record.file_name, "DKT 0004 - Notice of Hearing.yaml");
        assert_eq!(
            record.yaml,
            "number: '4'\n\
             kind: hearing\n\
             title: Notice of Hearing\n\
             party: Court\n\
             filed_or_served_on: 2026-03-05\n\
             scheduled_on: 2026-04-01T16:00:00Z\n\
             supersedes: null\n\
             document: Docket Filings/DKT 0004 - Notice of Hearing\n"
        );
    }

    #[test]
    fn an_entry_without_a_document_still_renders() {
        let record = render(&json!({
            "entry_number": "2",
            "kind": "other",
            "title": "Summons Issued"
        }))
        .unwrap();
        assert_eq!(record.file_name, "DKT 0002 - Summons Issued.yaml");
        assert_eq!(
            record.yaml,
            "number: '2'\nkind: other\ntitle: Summons Issued\nparty: null\n\
             filed_or_served_on: null\nscheduled_on: null\nsupersedes: null\n\
             document: null\n"
        );
    }

    #[test]
    fn a_continuance_names_the_entry_it_supersedes() {
        let record = render(&json!({
            "entry_number": "29-1",
            "kind": "hearing",
            "title": "Order Continuing Hearing",
            "scheduled_on": "2026-05-01T16:00:00Z",
            "supersedes": "0190c2a0-0000-7000-8000-000000000001"
        }))
        .unwrap();
        assert_eq!(
            record.file_name,
            "DKT 0029-1 - Order Continuing Hearing.yaml"
        );
        assert!(record
            .yaml
            .contains("supersedes: 0190c2a0-0000-7000-8000-000000000001\n"));
    }

    #[test]
    fn an_unsafe_title_is_sanitized_in_the_name_only() {
        let record = render(&json!({
            "entry_number": "7",
            "kind": "motion",
            "title": "Motion re: A/B \"Stay\"?  Pending..."
        }))
        .unwrap();
        assert_eq!(
            record.file_name,
            "DKT 0007 - Motion re- A-B -Stay-- Pending.yaml"
        );
        assert!(record
            .yaml
            .contains("title: 'Motion re: A/B \"Stay\"?  Pending...'\n"));
    }

    #[test]
    fn a_title_with_nothing_safe_reads_untitled() {
        assert_eq!(safe_title("?*:"), "---");
        assert_eq!(safe_title(" . "), "Untitled");
    }

    #[test]
    fn rendering_twice_yields_identical_bytes() {
        let row = json!({
            "entry_number": "12", "kind": "order", "title": "Order", "party": "Court",
            "filed_or_served_on": "2026-03-05", "document_slug": "Docket Filings/x"
        });
        assert_eq!(render(&row).unwrap(), render(&row).unwrap());
    }

    #[test]
    fn a_row_missing_its_number_title_or_kind_is_refused() {
        assert!(render(&json!({"kind": "x", "title": "t"})).is_err());
        assert!(render(&json!({"entry_number": "1", "kind": "x"})).is_err());
        assert!(render(&json!({"entry_number": "1", "title": "t"})).is_err());
        assert!(render(&json!({"entry_number": "x1", "kind": "k", "title": "t"})).is_err());
    }
}
