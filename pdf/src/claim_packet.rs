//! A Legal Plan claim packet (LAW-585): the plan's claim form, an itemized
//! invoice, and the proof the plan asks for, as one PDF.
//!
//! Every value is caller text, injected as a Typst string literal through
//! [`escape_typst_string`], never as markup. The proof documents are the
//! filed PDFs themselves, appended after a page that lists them.

use crate::{append_document, escape_typst_string, render, PdfError};

/// One page of label and value rows.
#[derive(Debug, Clone, Default)]
pub struct PacketPage {
    pub title: String,
    /// Lines printed under the title, before the rows.
    pub lead: Vec<String>,
    pub rows: Vec<(String, String)>,
}

/// The itemized invoice: its header rows, one line per block of work, and
/// the totals.
#[derive(Debug, Clone, Default)]
pub struct PacketInvoice {
    pub page: PacketPage,
    /// Date, timekeeper, minutes, description, amount.
    pub lines: Vec<[String; 5]>,
    pub totals: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct ClaimPacketParams {
    pub claim_form: PacketPage,
    pub invoice: PacketInvoice,
    /// The proof page: what follows, by filename.
    pub proof: PacketPage,
}

fn literal(value: &str) -> String {
    format!("\"{}\"", escape_typst_string(value))
}

fn row(label: &str, value: &str) -> String {
    format!(
        "  text(weight: \"bold\")[#{}], {},",
        literal(label),
        literal(value)
    )
}

fn page(page: &PacketPage) -> Vec<String> {
    let mut lines = vec![format!("#heading(level: 1)[#{}]", literal(&page.title))];
    lines.extend(page.lead.iter().map(|line| format!("#{}\n", literal(line))));
    if !page.rows.is_empty() {
        lines.push("#table(columns: (2fr, 3fr), stroke: 0.5pt + luma(180), inset: 6pt,".into());
        lines.extend(page.rows.iter().map(|(label, value)| row(label, value)));
        lines.push(")".into());
    }
    lines
}

/// Render the packet and append each proof PDF after the proof page.
///
/// # Errors
/// [`PdfError`] when Typst fails or a proof is not a readable PDF.
pub fn render_claim_packet(
    params: &ClaimPacketParams,
    proofs: &[Vec<u8>],
) -> Result<Vec<u8>, PdfError> {
    let mut lines: Vec<String> = vec![
        "#set page(paper: \"us-letter\", margin: 0.8in)".into(),
        "#set text(font: \"Noto Serif\", size: 10.5pt)".into(),
        "#show heading.where(level: 1): it => block(below: 1em, text(size: 16pt, it.body))".into(),
    ];
    lines.extend(page(&params.claim_form));
    lines.push("#pagebreak()".into());
    lines.extend(page(&params.invoice.page));
    if !params.invoice.lines.is_empty() {
        lines.push(
            "#v(0.8em)\n#table(columns: (auto, 1.4fr, auto, 3fr, auto), stroke: 0.5pt + luma(180), inset: 5pt,".into(),
        );
        lines.push(
            "  table.header([*Date*], [*Timekeeper*], [*Minutes*], [*Description*], [*Amount*]),"
                .into(),
        );
        lines.extend(params.invoice.lines.iter().map(|line| {
            let cells = line.iter().map(|cell| literal(cell)).collect::<Vec<_>>();
            format!("  {},", cells.join(", "))
        }));
        lines.push(")".into());
    }
    if !params.invoice.totals.is_empty() {
        lines.push("#v(0.8em)\n#align(right, table(columns: 2, stroke: none, inset: 4pt,".into());
        lines.extend(
            params
                .invoice
                .totals
                .iter()
                .map(|(label, value)| row(label, value)),
        );
        lines.push("))".into());
    }
    lines.push("#pagebreak()".into());
    lines.extend(page(&params.proof));
    let mut packet = render(&lines.join("\n"))?;
    for proof in proofs {
        packet = append_document(&packet, proof)?;
    }
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> ClaimPacketParams {
        ClaimPacketParams {
            claim_form: PacketPage {
                title: "Synthetic Plan claim form".into(),
                lead: vec!["File with claims@example.com".into()],
                rows: vec![
                    ("Plan Case Number".into(), "CASE-1".into()),
                    ("Member ID".into(), "#text(red)[injected]".into()),
                ],
            },
            invoice: PacketInvoice {
                page: PacketPage {
                    title: "Itemized invoice".into(),
                    lead: Vec::new(),
                    rows: vec![("Billed by".into(), "Synthetic Firm PC".into())],
                },
                lines: vec![[
                    "2026-11-20".into(),
                    "Synthetic Lawyer".into(),
                    "18".into(),
                    "Call \"with\" counsel".into(),
                    "$36.00".into(),
                ]],
                totals: vec![
                    ("Covered by the plan".into(), "$36.00".into()),
                    ("Member share".into(), "$0.00".into()),
                ],
            },
            proof: PacketPage {
                title: "Proof".into(),
                lead: vec!["Signed engagement letter".into()],
                rows: Vec::new(),
            },
        }
    }

    #[test]
    fn the_packet_holds_the_form_the_invoice_and_each_proof_in_order() {
        let proof = render("Synthetic signed letter page").unwrap();
        let packet = render_claim_packet(&params(), &[proof]).unwrap();
        assert_eq!(crate::passage::page_count(&packet).unwrap(), 4);
        let page_of = |needle: &str| {
            crate::locate(&packet, needle, 1)
                .unwrap_or_else(|error| panic!("{needle}: {error}"))
                .rects[0]
                .page_index
        };
        assert_eq!(page_of("Synthetic Plan claim form"), 0);
        assert_eq!(page_of("CASE-1"), 0);
        assert_eq!(page_of("Itemized invoice"), 1);
        assert_eq!(page_of("Member share"), 1);
        assert_eq!(page_of("Signed engagement letter"), 2);
        assert_eq!(page_of("Synthetic signed letter page"), 3);
        assert_eq!(
            crate::occurrence_count(&packet, "#text(red)[injected]").unwrap(),
            1,
            "caller text stays literal"
        );
    }

    #[test]
    fn a_proof_that_is_not_a_pdf_is_refused() {
        assert!(render_claim_packet(&params(), &[b"not a pdf".to_vec()]).is_err());
    }
}
