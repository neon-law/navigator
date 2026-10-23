//! The certificate of service a pleading carries, composed from its
//! [Service](../../docs/glossary/service.md) records.
//!
//! A certificate states service that has happened: each recipient, the
//! method, the date, and the address or handle, under a declarant's
//! perjury declaration. It is never typed into a pleading body and never
//! rendered with a blank. [`compose`] builds the one Markdown block both the
//! PDF and the Word render print, so the two cannot disagree; with no
//! records there is nothing to compose, and the renderer decides whether
//! that omits the certificate (a preview) or refuses (a final render).

use std::fmt::Write as _;

/// One record as the certificate prints it. Display-ready: the method is
/// its certificate phrase and the date is already a court-style date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    pub recipient: String,
    pub counsel_for: Option<String>,
    /// `by mail`, `by electronic service`, …
    pub method: String,
    /// `March 5, 2026`.
    pub served_on: String,
    pub address: String,
}

/// What the declarant states about themself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declarant {
    pub name: String,
    /// The business address the declaration recites, one line.
    pub business_address: String,
    /// `March 5, 2026`: the date the declaration is executed.
    pub executed_on: String,
    /// `Sampleton, California`.
    pub executed_at: String,
}

/// The heading the certificate opens with. `rules::final_document` reads
/// this heading to refuse a certificate that still carries a blank.
pub const HEADING: &str = "PROOF OF SERVICE";

/// Compose the certificate for `document_title` served as `served` lists,
/// under `declarant`'s declaration. Returns `None` when there is nothing to
/// certify, so a caller never prints an empty certificate.
#[must_use]
pub fn compose(document_title: &str, served: &[Served], declarant: &Declarant) -> Option<String> {
    if served.is_empty() {
        return None;
    }
    let mut out = String::new();
    let _ = writeln!(out, "## {HEADING}\n");
    let _ = writeln!(
        out,
        "I, {}, declare: I am over the age of eighteen years and not a party to this action. \
         My business address is {}. On the dates stated below, I served the foregoing document, \
         described as **{}**, on the following persons by the method stated:\n",
        declarant.name, declarant.business_address, document_title
    );
    for record in served {
        let counsel = record
            .counsel_for
            .as_deref()
            .map(|party| format!(", counsel for {party}"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "- **{}**{counsel}: {} on {}, addressed to {}.",
            record.recipient, record.method, record.served_on, record.address
        );
    }
    let _ = writeln!(
        out,
        "\nI declare under penalty of perjury under the laws of the State of California that the \
         foregoing is true and correct. Executed on {} at {}.\n",
        declarant.executed_on, declarant.executed_at
    );
    let _ = writeln!(out, "{}\n", declarant.name);
    let _ = writeln!(out, "Signature: ______________________________");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{compose, Declarant, Served, HEADING};

    fn declarant() -> Declarant {
        Declarant {
            name: "Pat Sample".into(),
            business_address: "100 Example Boulevard, Suite 200, Sampleton, California 90000"
                .into(),
            executed_on: "March 6, 2026".into(),
            executed_at: "Sampleton, California".into(),
        }
    }

    #[test]
    fn two_recipients_by_two_methods_print_under_one_declaration() {
        let certificate = compose(
            "PLAINTIFF'S NOTICE OF SYNTHETIC COMPLIANCE",
            &[
                Served {
                    recipient: "Example Counsel".into(),
                    counsel_for: Some("Defendant Example Co.".into()),
                    method: "by mail".into(),
                    served_on: "March 5, 2026".into(),
                    address: "200 Sample Street, Sampleton, California 90000".into(),
                },
                Served {
                    recipient: "Other Counsel".into(),
                    counsel_for: None,
                    method: "by electronic service".into(),
                    served_on: "March 6, 2026".into(),
                    address: "other@example.com".into(),
                },
            ],
            &declarant(),
        )
        .expect("two records compose a certificate");
        assert!(certificate.starts_with(&format!("## {HEADING}\n")));
        assert!(certificate.contains(
            "**Example Counsel**, counsel for Defendant Example Co.: by mail on March 5, 2026"
        ));
        assert!(certificate.contains("**Other Counsel**: by electronic service on March 6, 2026"));
        assert!(certificate.contains("Executed on March 6, 2026 at Sampleton, California."));
        assert!(certificate.contains("Signature: ___"));
        assert_eq!(certificate.matches("penalty of perjury").count(), 1);
        assert!(!certificate.contains('['), "no bracketed marker");
    }

    #[test]
    fn no_records_compose_nothing() {
        assert_eq!(compose("TITLE", &[], &declarant()), None);
    }
}
