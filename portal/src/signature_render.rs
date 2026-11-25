//! Expand signature placeholders in a Typst document body.
//!
//! A template body carries two kinds of `{{ … }}` placeholder. *Data*
//! placeholders (`{{client_name}}`, no dot) are string-substituted with
//! questionnaire answers before this stage runs. *Signature*
//! placeholders (`{{client.signature}}`, a dot) are handled here: each
//! expands into a visible Typst signature block **plus** an invisible
//! anchor token in the PDF's text layer that the e-signature provider
//! keys its field off of.
//!
//! We reuse [`rules::f107::signature_placeholders`] — the same grammar
//! the N107 validator enforces — so a placeholder that lints clean is a
//! placeholder we know how to render. The returned
//! [`SignatureField`]s become the manifest the provider turns into
//! anchored tabs (see [`crate::signature`]).

use std::collections::BTreeMap;

use crate::signature::{SignatureField, SignatureFieldKind};

/// Map a placeholder's `field` half to its typed kind. Unknown kinds
/// return `None`; the caller leaves such tokens untouched (N107 would
/// already have flagged them at validation time).
fn parse_kind(field: &str) -> Option<SignatureFieldKind> {
    match field {
        "signature" => Some(SignatureFieldKind::Signature),
        "initials" => Some(SignatureFieldKind::Initials),
        "date" => Some(SignatureFieldKind::Date),
        _ => None,
    }
}

/// Human display name for a signer role.
fn signer_label(signer: &str, firm_legal_name: Option<&str>) -> String {
    match signer {
        "client" => "Client".to_string(),
        "firm" => firm_legal_name.unwrap_or("Firm").to_string(),
        other => other
            .split('_')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// The caption rendered under the signature line.
fn block_label(signer: &str, kind: SignatureFieldKind, firm_legal_name: Option<&str>) -> String {
    match (signer, kind) {
        ("firm", SignatureFieldKind::Signature) => signer_label(signer, firm_legal_name),
        (_, SignatureFieldKind::Signature) => {
            format!("{} signature", signer_label(signer, firm_legal_name))
        }
        (_, SignatureFieldKind::Initials) => {
            format!("{} initials", signer_label(signer, firm_legal_name))
        }
        (_, SignatureFieldKind::Date) => "Date signed".to_string(),
    }
}

/// The Typst markup one signature block expands to: an underlined box
/// holding the invisible (white, tiny) anchor text, then the caption on
/// the line below. The anchor sits *inside* the signing line so the
/// provider drops its field right on the line.
fn render_block(
    signer: &str,
    kind: SignatureFieldKind,
    anchor: &str,
    firm_legal_name: Option<&str>,
) -> String {
    let width = match kind {
        SignatureFieldKind::Initials => "1in",
        SignatureFieldKind::Date => "2in",
        SignatureFieldKind::Signature => "3in",
    };
    let label = pdf::escape_typst_markup(&block_label(signer, kind, firm_legal_name));
    let anchor = anchor.replace('_', "\\_");
    format!(
        "#box(stroke: (bottom: navigator-signature-stroke), width: {width}, height: 1.4em)\
         [#text(fill: white, size: 4pt)[{anchor}]] \\\n#navigator-signature-caption[{label}]"
    )
}

/// Deterministic anchor string for the `n`-th occurrence of a given
/// `(signer, field)` pair. Unique per occurrence so the provider places
/// exactly one field per placeholder.
fn anchor_string(signer: &str, field: &str, n: usize) -> String {
    word::paper::signature_anchor(signer, field, n)
}

/// Replace every recognized signature placeholder in `body` with its
/// Typst block + invisible anchor, returning the rewritten Typst source
/// and the ordered list of fields placed. Unrecognized dotted tokens
/// are left verbatim (a valid template has none — N107 guards that).
#[must_use]
pub fn expand_signatures(body: &str) -> (String, Vec<SignatureField>) {
    expand_signatures_for_firm(body, None)
}

/// Render the Firm caption from the document's resolved Firm legal name.
pub(crate) fn expand_signatures_for_firm(
    body: &str,
    firm_legal_name: Option<&str>,
) -> (String, Vec<SignatureField>) {
    let placeholders = rules::f107::signature_placeholders(body);
    let mut out = String::with_capacity(body.len());
    let mut fields = Vec::new();
    let mut counters: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut cursor = 0usize;

    for ph in placeholders {
        let Some(kind) = parse_kind(&ph.field) else {
            continue; // leave unknown tokens in place
        };

        // Markdown-to-Typst escapes underscores; the manifest and anchor use
        // the declared role's spelling.
        let signer = ph.signer.replace("\\_", "_");

        // Emit everything between the previous token and this one.
        out.push_str(&body[cursor..ph.offset]);

        let n = {
            let c = counters
                .entry((signer.clone(), ph.field.clone()))
                .or_insert(0);
            *c += 1;
            *c
        };
        let anchor = anchor_string(&signer, &ph.field, n);
        out.push_str(&render_block(&signer, kind, &anchor, firm_legal_name));
        fields.push(SignatureField {
            recipient_role: signer,
            kind,
            anchor,
        });
        cursor = ph.end;
    }
    out.push_str(&body[cursor..]);
    (out, fields)
}

#[cfg(test)]
mod tests {
    use super::{expand_signatures, expand_signatures_for_firm};
    use crate::signature::SignatureFieldKind;

    #[test]
    fn leaves_a_body_without_signature_placeholders_untouched() {
        let body = "Dear Libra, welcome to the matter.";
        let (out, fields) = expand_signatures(body);
        assert_eq!(out, body);
        assert!(fields.is_empty());
    }

    #[test]
    fn expands_each_placeholder_to_an_anchor_and_records_the_field() {
        let body = "Sign: {{client.signature}} on {{client.date}}\nFirm: {{firm.signature}}";
        let (out, fields) = expand_signatures_for_firm(body, Some("Example Legal Group, PC"));

        // Placeholders are gone; anchors are present in the text.
        assert!(!out.contains("{{client.signature}}"));
        assert!(out.contains("nlsig-client-signature-1"));
        assert!(out.contains("nlsig-client-date-1"));
        assert!(out.contains("nlsig-firm-signature-1"));
        assert!(out.ends_with("#navigator-signature-caption[Example Legal Group, PC]"));
        let (without_firm, fields_without_firm) = expand_signatures(body);
        assert!(without_firm.ends_with("#navigator-signature-caption[Firm]"));
        assert_eq!(fields, fields_without_firm);

        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].recipient_role, "client");
        assert_eq!(fields[0].kind, SignatureFieldKind::Signature);
        assert_eq!(fields[0].anchor, "nlsig-client-signature-1");
        assert_eq!(fields[2].recipient_role, "firm");
    }

    #[test]
    fn anchors_are_unique_per_occurrence() {
        let body = "{{client.initials}} ... {{client.initials}}";
        let (out, fields) = expand_signatures(body);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].anchor, "nlsig-client-initials-1");
        assert_eq!(fields[1].anchor, "nlsig-client-initials-2");
        assert!(out.contains("nlsig-client-initials-1"));
        assert!(out.contains("nlsig-client-initials-2"));
    }

    #[test]
    fn data_placeholders_are_not_touched() {
        // No dot → not a signature placeholder, passes straight through.
        let body = "Hello {{client_name}}.";
        let (out, fields) = expand_signatures(body);
        assert_eq!(out, body);
        assert!(fields.is_empty());
    }

    #[test]
    fn expanded_body_compiles_through_the_typst_renderer() {
        // The real safety net: the emitted Typst markup must compile.
        let body = "Engagement terms above.\n\n\
                    {{client.signature}}\n\n{{client.date}}\n\n\
                    Initial here: {{client.initials}}\n\n{{firm.signature}}\n";
        let (typst_source, _fields) =
            expand_signatures_for_firm(body, Some("Example & Sample [Legal] #1"));
        let pdf = pdf::render(&typst_source).expect("expanded signature blocks must compile");
        assert!(pdf.starts_with(b"%PDF"), "renders a real PDF");
    }
}
