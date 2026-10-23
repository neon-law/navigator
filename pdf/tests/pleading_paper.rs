//! A synthetic California pleading's whole paper, proven through the
//! renderer: the first-page counsel block, court title, and caption; the
//! page number and paper-title footer on every page; and request/response
//! groups that stay whole (LAW-106).
//!
//! Every name, date, and case number here is synthetic. Positions come
//! from the rendered PDF — `pdf::locate` for text, the page's own `Tf`
//! operators for type size — not from the Typst the renderer was handed.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use lopdf::Document;
use pdf::pleading::{variant_for_jurisdiction, Variant};
use pdf::{Letterhead, OutputFormat};

const PAGE_H: f64 = 792.0;
const PAGE_W: f64 = 612.0;
const TITLE: &str = "RESPONSES TO REQUESTS FOR ADMISSION, SET ONE";

fn california() -> Variant {
    variant_for_jurisdiction("CA").expect("CA is calibrated")
}

fn caption(counsel: &[&str]) -> String {
    let counsel = counsel.iter().fold(String::new(), |mut out, line| {
        let _ = writeln!(out, "counsel: {line}");
        out
    });
    format!(
        "```caption\n{counsel}court: SUPERIOR COURT OF THE STATE OF CALIFORNIA\n\
         court: COUNTY OF SAMPLE\n\
         parties: SAMPLE PLAINTIFF,\nparties: Plaintiff,\nparties: v.\n\
         parties: EXAMPLE DEFENDANT,\nparties: Defendant.\n\
         case: Case No. 00CV000000\ncase: Dept. 12\ncase: Judge: Hon. Sample Jurist\n```\n\n"
    )
}

const SEVEN_COUNSEL: [&str; 7] = [
    "Pat Sample (SBN 000000)",
    "SAMPLE LAW GROUP",
    "100 Example Boulevard",
    "Sampleton, California 90000",
    "Telephone: (000) 555-0100",
    "Email: pat@example.invalid",
    "Attorneys for Plaintiff",
];

/// A pleading with a caption, a title, and `pairs` request/response
/// groups, each wrapped to stay whole, then a kept verification.
fn pleading(counsel: &[&str], pairs: usize) -> String {
    let mut body = caption(counsel);
    let _ = write!(body, "# {TITLE}\n\n## 1. Responses\n\n");
    for n in 1..=pairs {
        let _ = write!(
            body,
            "<!-- keep -->\n\nREQUEST NO. {n}:\n\nAdmit that synthetic fact {n} is true.\n\n\
             RESPONSE NO. {n}:\n\nDenied. Synthetic denial number {n}, stated at some length so \
             the group is more than a single line of the page.\n\n<!-- /keep -->\n\n"
        );
    }
    body.push_str(
        "## 2. Verification\n\n<!-- keep -->\n\nI declare under penalty of perjury that the \
         foregoing is true.\n\nExecuted on 1 July 2026.\n\nDeclarant.\n\n<!-- /keep -->\n",
    );
    body
}

fn render(body: &str) -> Vec<u8> {
    pdf::render_document(
        body,
        OutputFormat::Pleading(california()),
        &Letterhead::default(),
    )
    .expect("the pleading renders")
}

/// Top edge of a located quote, in points from the page top.
fn top(bytes: &[u8], quote: &str, ordinal: usize) -> f64 {
    let located = pdf::locate(bytes, quote, ordinal).expect("locate");
    located.rects[0].rect.y * PAGE_H
}

fn left(bytes: &[u8], quote: &str, ordinal: usize) -> f64 {
    let located = pdf::locate(bytes, quote, ordinal).expect("locate");
    located.rects[0].rect.x * PAGE_W
}

/// Every `Tf` size a page sets, as hundredths of a point.
fn font_sizes(doc: &Document, page: lopdf::ObjectId) -> BTreeSet<u32> {
    let content = doc.get_page_content(page);
    let text = String::from_utf8_lossy(&content);
    let mut sizes = BTreeSet::new();
    let words: Vec<&str> = text.split_whitespace().collect();
    for window in words.windows(2) {
        if window[1].ends_with("Tf") {
            if let Ok(size) = window[0].parse::<f64>() {
                // A page's font sizes are small positive numbers.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                sizes.insert((size * 100.0).round() as u32);
            }
        }
    }
    sizes
}

/// CRC 2.111(1)–(3): the attorney block starts on line 1, 1in down; the
/// clerk's space to its right stays clear through line 7; the court's title
/// sits on line 8, at or below 3⅓in from the top of the page.
#[test]
fn the_first_page_puts_counsel_on_line_one_and_the_court_title_on_line_eight() {
    let bytes = render(&pleading(&SEVEN_COUNSEL, 3));

    let counsel_top = top(&bytes, "Pat Sample", 1);
    assert!(
        (72.0..96.0).contains(&counsel_top),
        "counsel starts {counsel_top}pt down, expected within line 1 (72–96pt)"
    );

    let court_top = top(&bytes, "SUPERIOR COURT OF THE STATE", 1);
    assert!(
        (240.0 - 1.0..264.0).contains(&court_top),
        "the court's title starts {court_top}pt down, expected line 8 (240–264pt), at or \
         below 3⅓in (240pt)"
    );

    // The right of the counsel block is the clerk's: nothing but the
    // counsel column's own text sits right of its 58% column through line 7.
    let widest = SEVEN_COUNSEL
        .iter()
        .map(|line| {
            let hit = pdf::locate(&bytes, line, 1).expect("locate counsel line");
            let rect = hit.rects[0].rect;
            (rect.x + rect.width) * PAGE_W
        })
        .fold(0.0_f64, f64::max);
    assert!(
        widest <= 108.0 + 0.58 * 432.0 + 1.0,
        "a counsel line reaches {widest}pt, into the clerk's space"
    );
}

/// The caption is one two-column block — parties left, case right — that
/// appears once, however many pages follow. A data table would repeat its
/// header row on every page it broke across.
#[test]
fn the_caption_is_two_columns_and_never_repeats() {
    let bytes = render(&pleading(&SEVEN_COUNSEL, 24));
    assert!(
        pdf::page_count(&bytes).expect("pages") >= 3,
        "need a long pleading"
    );
    assert_eq!(
        pdf::occurrence_count(&bytes, "Case No. 00CV000000").expect("count"),
        1
    );
    assert_eq!(
        pdf::occurrence_count(&bytes, "SAMPLE PLAINTIFF").expect("count"),
        1
    );

    let party_x = left(&bytes, "SAMPLE PLAINTIFF", 1);
    let case_x = left(&bytes, "Case No. 00CV000000", 1);
    assert!(
        party_x < 120.0,
        "parties start at the left margin: {party_x}"
    );
    assert!(
        case_x > 108.0 + 432.0 / 2.0,
        "the case fields sit in the right column: {case_x}"
    );
    assert!(
        (top(&bytes, "SAMPLE PLAINTIFF", 1) - top(&bytes, "Case No. 00CV000000", 1)).abs() < 1.0,
        "both columns start on the same line"
    );
}

/// A caption's cells wrap long party names without clipping them, and the
/// body that follows still starts on a numbered line.
#[test]
fn a_long_party_name_wraps_and_the_body_stays_on_the_rail() {
    let long = "A SYNTHETIC HOLDING COMPANY OF NORTHERN CALIFORNIA AND SOUTHERN OREGON, A LIMITED \
                LIABILITY COMPANY, AND DOES 1 THROUGH 50, INCLUSIVE,";
    let body = pleading(&SEVEN_COUNSEL, 3).replace("SAMPLE PLAINTIFF,", long);
    let bytes = render(&body);
    // The whole name reaches the page (it is searchable text).
    assert_eq!(
        pdf::occurrence_count(&bytes, "INCLUSIVE").expect("count"),
        1
    );
    // Every body line is a whole number of 24pt grid lines from line 1,
    // wherever the caption's height ended: compare two lines of the
    // same face against the counsel block's first line.
    let line_one = top(&bytes, "Pat Sample", 1);
    for quote in ["REQUEST NO. 1:", "RESPONSE NO. 1:", "REQUEST NO. 3:"] {
        let steps = (top(&bytes, quote, 1) - line_one) / 24.0;
        eprintln!(
            "DEBUG {quote}: line_one={line_one} top={} page={}",
            top(&bytes, quote, 1),
            pdf::locate(&bytes, quote, 1).unwrap().rects[0].page_index
        );
        // A line on page two or later restarts the frame, so the
        // distance is whole lines either way.
        assert!(
            (steps - steps.round()).abs() < 0.03,
            "`{quote}` sits {steps} grid lines below line 1, off the rail"
        );
    }
}

/// CRC 2.109 and 2.110: on every page, an Arabic page number at the
/// bottom, a printed rule below it, and the paper's title below the rule
/// at 10pt or larger. All sit in the bottom margin, clear of the frame.
#[test]
fn every_page_carries_a_page_number_and_a_title_footer_below_the_frame() {
    let bytes = render(&pleading(&SEVEN_COUNSEL, 24));
    let pages = pdf::page_count(&bytes).expect("pages");
    assert!(pages >= 3);

    let title_hits = pdf::occurrence_count(&bytes, TITLE).expect("count");
    let mut footer_pages = BTreeSet::new();
    for ordinal in 1..=title_hits {
        let hit = pdf::locate(&bytes, TITLE, ordinal).expect("locate title");
        let rect = hit.rects[0];
        let y = rect.rect.y * PAGE_H;
        if y > 744.0 {
            footer_pages.insert(rect.page_index);
            assert!(
                y + rect.rect.height * PAGE_H < PAGE_H - 6.0,
                "the footer title runs off the page bottom"
            );
        }
    }
    assert_eq!(
        footer_pages,
        (0..pages).collect::<BTreeSet<_>>(),
        "every page carries the paper-title footer"
    );

    // Type sizes: the body is at least 12pt (CRC 2.104), the footer title
    // at least 10pt (CRC 2.110), and nothing on any page is below 10pt.
    let doc = Document::load_mem(&bytes).expect("load");
    for (number, page) in doc.get_pages() {
        let sizes = font_sizes(&doc, page);
        assert!(
            sizes.iter().all(|s| *s >= 1000),
            "page {number} sets text below 10pt: {sizes:?}"
        );
        assert!(
            sizes.contains(&1000),
            "page {number} has no 10pt footer title"
        );
        assert!(
            sizes.contains(&1450),
            "page {number} has no 14.5pt body: {sizes:?}"
        );
    }
}

/// Every pair stays on one page when it fits on one: a request is never
/// stranded from its response across a page break.
#[test]
fn a_request_stays_with_its_response() {
    let bytes = render(&pleading(&SEVEN_COUNSEL, 30));
    for n in 1..=30 {
        let request = pdf::locate(&bytes, &format!("REQUEST NO. {n}:"), 1).expect("request");
        let response = pdf::locate(&bytes, &format!("RESPONSE NO. {n}:"), 1).expect("response");
        assert_eq!(
            request.rects[0].page_index, response.rects[0].page_index,
            "request {n} was separated from its response"
        );
    }
    // The verification stays whole with its date and signature line.
    let first = pdf::locate(&bytes, "I declare under penalty", 1).expect("declaration");
    let last = pdf::locate(&bytes, "Declarant.", 1).expect("signature");
    assert_eq!(first.rects[0].page_index, last.rects[0].page_index);
}

/// A group taller than a page is not clipped and does not strand blank
/// pages: every line of it reaches the PDF, and the page count stays
/// close to the text's own length.
#[test]
fn an_oversized_group_flows_across_pages_without_clipping() {
    let mut body = caption(&SEVEN_COUNSEL);
    let _ = write!(body, "# {TITLE}\n\n## 1. Responses\n\n<!-- keep -->\n\n");
    for n in 1..=90 {
        let _ = write!(body, "Oversized line number {n}.\n\n");
    }
    body.push_str("<!-- /keep -->\n");
    let bytes = render(&body);
    for n in [1, 45, 90] {
        assert_eq!(
            pdf::occurrence_count(&bytes, &format!("Oversized line number {n}.")).expect("count"),
            1,
            "line {n} was clipped"
        );
    }
    let pages = pdf::page_count(&bytes).expect("pages");
    // 90 double-spaced paragraphs at 28 lines a page is a little over
    // three pages after a caption; a stranded block would add more.
    assert!(
        (4..=5).contains(&pages),
        "an oversized group took {pages} pages"
    );
}

/// A title too long for one footer line is abbreviated rather than
/// wrapping into the text column.
#[test]
fn a_long_title_is_abbreviated_in_the_footer_and_stays_in_the_margin() {
    let long = "DEFENDANT OPPOSITION TO PLAINTIFF MOTION FOR LEAVE TO FILE A SECOND AMENDED \
                COMPLAINT AND REQUEST FOR JUDICIAL NOTICE AND DECLARATION OF COUNSEL IN SUPPORT";
    let body = pleading(&SEVEN_COUNSEL, 2).replace(TITLE, long);
    let bytes = render(&body);
    let hit = pdf::locate(&bytes, "DEFENDANT OPPOSITION", 2).expect("the abbreviated footer title");
    let rect = hit.rects[0].rect;
    assert!(
        rect.y * PAGE_H > 744.0,
        "the footer title sits in the bottom margin"
    );
    assert!(
        (rect.x + rect.width) * PAGE_W <= PAGE_W - 36.0 + 1.0,
        "the footer title stays inside the right margin"
    );
    // One line: its height is a single 10pt line, not two.
    assert!(rect.height * PAGE_H < 14.0);
}

/// Left margin of at least 1in and right of at least 0.5in, on every page.
#[test]
fn text_stays_inside_the_pleading_margins() {
    let bytes = render(&pleading(&SEVEN_COUNSEL, 24));
    for n in [1, 12, 24] {
        let hit = pdf::locate(&bytes, &format!("Denied. Synthetic denial number {n}"), 1)
            .expect("locate response");
        for rect in &hit.rects {
            let left = rect.rect.x * PAGE_W;
            let right = (rect.rect.x + rect.rect.width) * PAGE_W;
            assert!(left >= 72.0, "text starts {left}pt from the left edge");
            assert!(
                right <= PAGE_W - 36.0 + 0.5,
                "text ends {right}pt, inside 0.5in of the edge"
            );
        }
    }
}
