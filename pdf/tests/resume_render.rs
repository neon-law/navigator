//! The `resume` render frame: a person's own record of experience, dense
//! enough to hold one page, with no firm marks and a centered name and
//! contact header. Proven through the renderer, as `contract_render.rs` does.

use std::fmt::Write as _;

use pdf::{Letterhead, OutputFormat};

/// A synthetic one-page resume: the name and contact line above the first
/// section, then the dense bullet lists a resume is made of.
fn body() -> String {
    let mut body = String::from(
        "# Alex Example\n\nalex@example.com · 555-0100 · example.com\n\n## Experience\n\n",
    );
    for role in 1..=4 {
        let _ = write!(body, "### Role {role}, Example Organization, 2020–2024\n\n");
        for item in 1..=6 {
            let _ = writeln!(
                body,
                "- Led a synthetic workstream {item} that delivered a measurable outcome."
            );
        }
        body.push('\n');
    }
    body.push_str("## Education\n\nJ.D., Example School of Law, 2019\n");
    body
}

fn render(format: OutputFormat) -> Vec<u8> {
    pdf::render_document(&body(), format, &Letterhead::default()).expect("renders")
}

#[test]
fn the_resume_frame_carries_no_firm_letterhead() {
    let resume = render(OutputFormat::Resume);
    let letterhead = Letterhead::default();
    for mark in [letterhead.email.as_str(), letterhead.web.as_str()] {
        assert_eq!(
            pdf::occurrence_count(&resume, mark).expect("scan the rendered pdf"),
            0,
            "the resume frame must not print the firm contact line (`{mark}`)"
        );
    }
}

#[test]
fn a_dense_resume_fits_the_page_the_letter_frame_spills() {
    // The defect this frame exists for: the airy letter geometry pushed a
    // one-page resume onto a second page.
    let letter = render(OutputFormat::ClientLetter(pdf::LetterBlocks::default()));
    assert!(pdf::page_count(&letter).expect("count") > 1);
    let resume = render(OutputFormat::Resume);
    assert_eq!(pdf::page_count(&resume).expect("count"), 1);
}

#[test]
fn the_header_above_the_first_section_is_centered() {
    let source = OutputFormat::Resume.document_source(&body());
    let (header, rest) = source
        .split_once("#align(center)[")
        .map(|(_, after)| {
            after
                .split_once("\n]\n")
                .expect("the centered block closes")
        })
        .expect("the header is centered");
    assert!(header.contains("Alex Example"), "{header}");
    assert!(header.contains("alex\\@example.com") || header.contains("alex@example.com"));
    assert!(!header.contains("Experience"), "{header}");
    assert!(rest.contains("Experience"));
}
