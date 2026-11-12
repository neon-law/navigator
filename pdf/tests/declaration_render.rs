//! The party-voice render frames (LAW-340) — what a `kind: declaration`
//! or `kind: affidavit` page may and may not carry, proven through the
//! renderer rather than through the emitted string.
//!
//! See `pdf/tests/pleading_render.rs` for why these assertions are
//! structural (counted glyph runs) rather than word-based: Typst's subset
//! fonts mean a naive text extraction returns glyph ids, not Unicode, so
//! `pdf::occurrence_count` (the `/ToUnicode` `CMap` route) is what
//! actually finds a quote.
//!
//! The frame contract: a declaration or affidavit is testimony the
//! *client* gives, so no frame in its vocabulary prints the firm's
//! letterhead. Its default frame is `Plain` — the same unbranded page an
//! instrument gets — and N109 refuses `output: letter` before a render
//! ever happens (see `rules/src/f109.rs`).

use pdf::{Letterhead, OutputFormat};

const BODY: &str = "# Declaration of Jane Client\n\n\
                    I declare under penalty of perjury that the foregoing is true and correct.\n";

#[test]
fn the_plain_frame_a_declaration_gets_carries_no_firm_letterhead() {
    // The default frame is unbranded end to end: no firm name, no contact
    // line, and none of the embedded logo bytes that make the letter frame
    // heavier than a plain page.
    let letterhead = Letterhead::default();
    let plain = pdf::render_document(BODY, OutputFormat::Plain, &letterhead).expect("renders");
    for mark in [
        letterhead.name.to_uppercase(),
        letterhead.email.clone(),
        letterhead.web.clone(),
    ] {
        assert_eq!(
            pdf::occurrence_count(&plain, &mark).expect("scan the rendered pdf"),
            0,
            "the party-voice frame must not print the firm mark (`{mark}`)"
        );
    }

    let letter = pdf::render_document(
        BODY,
        OutputFormat::Letter(pdf::LetterBlocks::default()),
        &letterhead,
    )
    .expect("letter renders");
    assert!(
        plain.len() < letter.len(),
        "a letterhead-free declaration ({}) must be smaller than the letter frame ({}) \
         — an embedded logo is still riding along",
        plain.len(),
        letter.len()
    );
}

#[test]
fn the_declaration_body_is_what_reaches_the_page() {
    // The frame adds nothing of its own: the party's title and the penalty
    // of perjury line print exactly as written, once.
    let plain =
        pdf::render_document(BODY, OutputFormat::Plain, &Letterhead::default()).expect("renders");
    for line in ["Declaration of Jane Client", "penalty of perjury"] {
        assert_eq!(
            pdf::occurrence_count(&plain, line).expect("scan the rendered pdf"),
            1,
            "`{line}` must reach the page exactly once"
        );
    }
}
