//! The Harvard outline's indentation, proven through the renderer: each
//! depth steps in one indent from its parent, a marker hangs, and a
//! wrapped line aligns to the text after the marker (`I.` → `A.` → `1.`
//! → `a.`).
//!
//! The assertions are x-offsets read from the rendered page with
//! `pdf::locate`, not substrings of the emitted Typst — see
//! `pdf/tests/contract_render.rs` for why.

use pdf::{Letterhead, OutputFormat};

/// Three levels under a depth-1 section. The lettered paragraph and the
/// numbered item are written long enough to wrap, so the continuation
/// line's x-offset is measurable.
const BODY: &str = "# Master Services Agreement\n\n\
## I. Scope\n\n\
> **A. Payment.** Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike \
november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu alpha bravo \
charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa.\n\
>\n\
> 1. Numbered one alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike \
november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu alpha bravo \
charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa.\n\
>    1. Nested quebec alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima \
mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu alpha \
bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa.\n\
> 2. Numbered two.\n\n\
## II. Term\n\n\
Plain.\n";

/// Prefixes of the two long paragraphs, each longer than one line, so the
/// located passage carries a rect for the wrapped line.
const LETTER_WRAPPING: &str = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee";
const NUMBER_WRAPPING: &str = "Numbered one alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray";

fn render() -> Vec<u8> {
    pdf::render_document(BODY, OutputFormat::Contract, &Letterhead::default()).expect("renders")
}

/// US Letter width in points: [`pdf::locate`] returns fractions of the page.
const PAGE_W: f64 = 612.0;
/// One indent step, in points. The frame's body type is 11 pt, so a step
/// of one em is the smallest a reader can see as a step.
const STEP: f64 = 11.0;
/// Rendering slack, in points.
const EPS: f64 = 0.75;

/// Where line `line` of the first occurrence of `text` starts, in points
/// from the page's left edge.
fn x_of(pdf: &[u8], text: &str, line: usize) -> f64 {
    let located = pdf::locate(pdf, text, 1).unwrap_or_else(|e| panic!("locate `{text}`: {e}"));
    located
        .rects
        .get(line)
        .unwrap_or_else(|| panic!("`{text}` has no line {line}"))
        .rect
        .x
        * PAGE_W
}

#[test]
fn each_outline_depth_steps_in_one_indent_from_its_parent() {
    let pdf = render();

    let section = x_of(&pdf, "I. Scope", 0);
    let letter = x_of(&pdf, "A.", 0);
    let letter_text = x_of(&pdf, "Payment.", 0);
    let number = x_of(&pdf, "1. Numbered one", 0);
    let number_text = x_of(&pdf, "Numbered one", 0);
    let nested = x_of(&pdf, "1. Nested", 0);

    assert!(
        letter >= section + STEP - EPS,
        "`A.` ({letter:.1}pt) must step in from the depth-1 heading ({section:.1}pt)"
    );
    assert!(
        number >= letter_text - EPS,
        "`1.` ({number:.1}pt) must step in from the text of `A.` ({letter_text:.1}pt)"
    );
    assert!(
        nested >= number_text - EPS,
        "the nested item ({nested:.1}pt) must step in from the text of `1.` ({number_text:.1}pt)"
    );
}

#[test]
fn a_wrapped_line_aligns_to_the_text_after_its_marker() {
    let pdf = render();

    let letter_text = x_of(&pdf, "Payment.", 0);
    let letter_wrap = x_of(&pdf, LETTER_WRAPPING, 1);
    assert!(
        (letter_wrap - letter_text).abs() <= EPS,
        "a wrapped line of `A.` ({letter_wrap:.1}pt) must align to its text ({letter_text:.1}pt)"
    );

    let number_text = x_of(&pdf, "Numbered one", 0);
    let number_wrap = x_of(&pdf, NUMBER_WRAPPING, 1);
    assert!(
        (number_wrap - number_text).abs() <= EPS,
        "a wrapped line of `1.` ({number_wrap:.1}pt) must align to its text ({number_text:.1}pt)"
    );
}
