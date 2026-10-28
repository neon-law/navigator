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

pub(crate) use store::cases::filing_name;

/// The `(entry number, attachment)` a `DKT NNNN[-att] - …` file name names,
/// with the entry number's leading zeros dropped (`0029` reads `29`).
pub(crate) fn parse_filing_name(file_name: &str) -> Option<(String, Option<u32>)> {
    let rest = file_name.strip_prefix("DKT ")?;
    let (number, _) = rest.split_once(" - ")?;
    let (lead, attachment) = match number.split_once('-') {
        Some((lead, att)) => (lead, Some(att.parse().ok()?)),
        None => (number, None),
    };
    if lead.is_empty() || !lead.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let trimmed = lead.trim_start_matches('0');
    let entry = if trimmed.is_empty() { "0" } else { trimmed };
    Some((entry.to_string(), attachment))
}

/// Render one Docket Entry row into its Case Timeline file.
pub fn render(row: &Value) -> Result<Record, String> {
    let entry_number = text(row, "entry_number").ok_or("entry has no entry_number")?;
    let title = text(row, "title").ok_or("entry has no title")?;
    let kind = text(row, "kind").ok_or("entry has no kind")?;
    let file_name =
        filing_name(&entry_number, &title, "yaml").map_err(|error| error.to_string())?;
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
    fn a_filing_is_named_dkt_number_dash_title_with_its_extension() {
        assert_eq!(
            filing_name("4", "Notice of Hearing", "pdf").unwrap(),
            "DKT 0004 - Notice of Hearing.pdf"
        );
        assert_eq!(
            filing_name("29-1", "Exhibit A: Supply/Agreement", "pdf").unwrap(),
            "DKT 0029-1 - Exhibit A- Supply-Agreement.pdf"
        );
        assert!(filing_name("x", "t", "pdf").is_err());
        assert!(filing_name("4-../../other", "t", "pdf").is_err());
        assert!(filing_name("4-", "t", "pdf").is_err());
    }

    #[test]
    fn a_long_title_is_cut_on_a_character_boundary() {
        let name = filing_name("1", &"é".repeat(300), "pdf").unwrap();
        assert!(name.len() <= 255, "{}", name.len());
        assert!(name.contains(".pdf") && name.len() <= 255);
        let name = filing_name("1", &"🗂".repeat(300), "pdf").unwrap();
        assert!(name.len() <= 255, "{name}");
    }

    #[test]
    fn a_filing_name_reads_back_to_its_entry_and_attachment() {
        assert_eq!(
            parse_filing_name("DKT 0029-1 - Exhibit A.pdf"),
            Some(("29".to_string(), Some(1)))
        );
        assert_eq!(
            parse_filing_name("DKT 0004 - Notice.pdf"),
            Some(("4".to_string(), None))
        );
        assert_eq!(parse_filing_name("di-4-notice.pdf"), None);
        assert_eq!(parse_filing_name("DKT 00x4 - Notice.pdf"), None);
    }

    #[test]
    fn a_title_with_nothing_safe_reads_untitled() {
        assert_eq!(
            filing_name("1", "?*:", "pdf").unwrap(),
            "DKT 0001 - ---.pdf"
        );
        assert_eq!(
            filing_name("1", " . ", "pdf").unwrap(),
            "DKT 0001 - Untitled.pdf"
        );
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
