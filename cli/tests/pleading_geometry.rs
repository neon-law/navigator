//! Every page of the synthetic California pleadings, measured against the
//! court rules the issue names (LAW-106): US Letter, a numbered rail on a
//! 24pt pitch restarting at 1, body clear of the rail and the margins, the
//! page number with a printed rule and a 10pt paper title below it, and a
//! first page whose counsel block, clerk's space, court title, and
//! two-column caption sit where CRC 2.111 puts them. Word is held to the
//! same numbers in its OOXML.
//!
//! The PDF is measured from `pdftotext -bbox` word boxes, so the check is
//! of the rendered page and not of the Typst that produced it. The tests
//! skip, saying so, where poppler is not installed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use tempfile::TempDir;

const FIXTURE: &str = "tests/fixtures/synthetic_matter";
const PAGE_W: f64 = 612.0;
const PAGE_H: f64 = 792.0;
/// The text column: 1.5in left margin to 0.5in right margin.
const COLUMN_LEFT: f64 = 108.0;
const COLUMN_WIDTH: f64 = PAGE_W - COLUMN_LEFT - 36.0;

fn matter_tree() -> TempDir {
    let work = TempDir::new().unwrap();
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE),
        work.path(),
    );
    work
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn render(work: &TempDir, template: &str, format: &str) -> PathBuf {
    let extension = if format == "word" { "docx" } else { "pdf" };
    let out = work.path().join(format!("{template}.{extension}"));
    let result = Command::new(cargo_bin("navigator"))
        .args(["notation", format])
        .arg(work.path().join("templates").join(format!("{template}.md")))
        .arg("--out")
        .arg(&out)
        .args([
            "--answer",
            "person__client=SAMPLE SYNTHETIC HOLDINGS OF NORTHERN CALIFORNIA",
            "--answer",
            "custom_datetime__verification_date=1 July 2026",
        ])
        .output()
        .expect("run navigator");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    out
}

#[derive(Debug, Clone)]
struct Word {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    text: String,
}

fn poppler() -> bool {
    Command::new("pdftotext").arg("-v").output().is_ok()
}

/// Each page's words, from `pdftotext -bbox`.
fn pages(pdf: &Path) -> Vec<Vec<Word>> {
    let out = Command::new("pdftotext")
        .args(["-bbox"])
        .arg(pdf)
        .arg("-")
        .output()
        .expect("pdftotext");
    let xml = String::from_utf8_lossy(&out.stdout).into_owned();
    let attr = |tag: &str, name: &str| -> f64 {
        let key = format!("{name}=\"");
        let start = tag.find(&key).unwrap() + key.len();
        tag[start..start + tag[start..].find('"').unwrap()]
            .parse()
            .unwrap()
    };
    xml.split("<page ")
        .skip(1)
        .map(|page| {
            assert!(
                (attr(page, "width") - PAGE_W).abs() < 0.01
                    && (attr(page, "height") - PAGE_H).abs() < 0.01,
                "every page is US Letter"
            );
            page.split("<word ")
                .skip(1)
                .map(|word| {
                    let open_end = word.find('>').unwrap();
                    let text = &word[open_end + 1..word.find("</word>").unwrap()];
                    Word {
                        x0: attr(word, "xMin"),
                        y0: attr(word, "yMin"),
                        x1: attr(word, "xMax"),
                        y1: attr(word, "yMax"),
                        text: text.replace("&amp;", "&").replace("&#39;", "'"),
                    }
                })
                .collect()
        })
        .collect()
}

fn is_rail(word: &Word) -> bool {
    word.x1 < 100.0 && word.text.chars().all(|c| c.is_ascii_digit())
}

/// The numbered rail restarts at 1 on every page, runs 1 through 28, and
/// holds a 24pt pitch (CRC 2.108: at least three numbers per inch).
fn assert_rail(page_number: usize, words: &[Word]) {
    let rail: Vec<&Word> = words.iter().filter(|w| is_rail(w)).collect();
    let numbers: Vec<u32> = rail.iter().map(|w| w.text.parse().unwrap()).collect();
    assert_eq!(
        numbers,
        (1..=28).collect::<Vec<u32>>(),
        "page {page_number}: rail"
    );
    for pair in rail.windows(2) {
        let pitch = pair[1].y0 - pair[0].y0;
        assert!(
            (pitch - 24.0).abs() < 0.05,
            "page {page_number}: rail pitch {pitch}pt"
        );
    }
    assert!(
        (rail[0].y0 - 72.0).abs() < 2.0,
        "page {page_number}: line 1 sits an inch down"
    );
}

/// Body text clears the rail by the 1.5in left margin and the right edge by
/// 0.5in (CRC 2.107); the footer sits below the frame.
fn assert_margins(page_number: usize, words: &[Word]) {
    for word in words.iter().filter(|w| !is_rail(w)) {
        assert!(
            word.x0 >= 108.0 - 0.5 || word.y0 > 744.0,
            "page {page_number}: `{}` starts {}pt from the left",
            word.text,
            word.x0
        );
        assert!(
            word.x1 <= PAGE_W - 36.0 + 0.5,
            "page {page_number}: `{}` ends {}pt from the left",
            word.text,
            word.x1
        );
        assert!(
            word.y1 <= PAGE_H - 6.0,
            "page {page_number}: `{}` runs off the bottom",
            word.text
        );
    }
}

/// The bottom Arabic page number, then the paper title below it (CRC 2.109,
/// 2.110), both centred in the text column.
fn assert_footer(page_number: usize, words: &[Word], title_first_word: &str) {
    let footer: Vec<&Word> = words.iter().filter(|w| w.y0 > 744.0).collect();
    let number = footer
        .iter()
        .find(|w| w.text == page_number.to_string())
        .unwrap_or_else(|| panic!("page {page_number}: no page number"));
    let middle = f64::midpoint(number.x0, number.x1);
    assert!(
        (middle - (COLUMN_LEFT + COLUMN_WIDTH / 2.0)).abs() < 1.0,
        "page {page_number}: the number is centred in the text column, at {middle}pt"
    );
    let title: Vec<&&Word> = footer.iter().filter(|w| w.y0 > number.y1).collect();
    assert_eq!(
        title.first().map(|w| w.text.as_str()),
        Some(title_first_word),
        "page {page_number}: footer title"
    );
    assert!(title.iter().all(|w| w.y0 > number.y1 + 3.0));
}

fn bbox<'a>(words: &'a [Word], text: &str) -> &'a Word {
    words
        .iter()
        .find(|w| !is_rail(w) && w.text == text)
        .unwrap_or_else(|| panic!("`{text}` is on the page"))
}

#[test]
fn every_pdf_page_meets_the_rail_margin_and_footer_geometry() {
    if !poppler() {
        eprintln!("skipped: pdftotext is not installed");
        return;
    }
    let work = matter_tree();
    for (template, first_title_word) in [
        ("rfa_responses", "PLAINTIFF’S"),
        ("notice_of_compliance", "PLAINTIFF’S"),
    ] {
        let pdf = render(&work, template, "pdf");
        let pages = pages(&pdf);
        assert!(!pages.is_empty());
        for (index, words) in pages.iter().enumerate() {
            let number = index + 1;
            assert_rail(number, words);
            assert_margins(number, words);
            assert_footer(number, words, first_title_word);
        }
    }
}

#[test]
fn the_first_page_places_counsel_the_clerks_space_the_court_and_the_caption() {
    if !poppler() {
        eprintln!("skipped: pdftotext is not installed");
        return;
    }
    let work = matter_tree();
    let pdf = render(&work, "rfa_responses", "pdf");
    let first = &pages(&pdf)[0];

    // CRC 2.111(1): the attorney block starts on line 1, an inch down.
    let counsel = bbox(first, "Pat");
    assert!(
        (72.0..96.0).contains(&counsel.y0),
        "counsel at {}pt",
        counsel.y0
    );

    // The block is seven lines; the clerk's space to its right stays clear
    // through line 7 (the first 7 * 24pt below the top margin).
    let clerk_top = 72.0;
    let clerk_bottom = 72.0 + 7.0 * 24.0;
    let counsel_right = COLUMN_LEFT + 0.58 * COLUMN_WIDTH + 1.0;
    for word in first.iter().filter(|w| !is_rail(w)) {
        if word.y0 >= clerk_top && word.y1 <= clerk_bottom {
            assert!(
                word.x1 <= counsel_right,
                "`{}` reaches {}pt into the clerk's space",
                word.text,
                word.x1
            );
        }
    }

    // CRC 2.111(3): the court's title sits at or below 3⅓ inches (240pt),
    // after the whole counsel block.
    let court = bbox(first, "SUPERIOR");
    assert!(court.y0 >= 240.0 - 1.0, "court title at {}pt", court.y0);
    let last_counsel = bbox(first, "Attorneys");
    assert!(court.y0 > last_counsel.y1);

    // The caption is two columns, ruled, and never carries a header row.
    assert!(first
        .iter()
        .all(|w| w.text != "Parties" && w.text != "information"));
    let case_no = bbox(first, "Case");
    let party = bbox(first, "SAMPLE");
    assert!(
        case_no.x0 > COLUMN_LEFT + COLUMN_WIDTH / 2.0 - 1.0,
        "case cell is the right column"
    );
    assert!(party.x0 < 108.0 + 5.0, "party cell is the left column");
    assert!(
        case_no.y0 >= court.y1,
        "caption sits below the court's title"
    );
}

fn docx_part(path: &Path, name: &str) -> String {
    use std::io::Read as _;
    let bytes = fs::read(path).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("docx is a zip");
    let mut xml = String::new();
    archive
        .by_name(name)
        .unwrap_or_else(|_| panic!("docx carries {name}"))
        .read_to_string(&mut xml)
        .unwrap();
    xml
}

/// Every numeric `w:<attr>="N"` after `marker`, in document order.
fn numbers_after(xml: &str, marker: &str, attr: &str) -> Vec<u32> {
    let key = format!("{attr}=\"");
    xml.split(marker)
        .skip(1)
        .filter_map(|rest| {
            let start = rest.find(&key)? + key.len();
            rest[start..start + rest[start..].find('"')?].parse().ok()
        })
        .collect()
}

/// The Word document holds the PDF's numbers in its OOXML: page, margins,
/// type floor, rail, first-page caption, and footer.
#[test]
fn the_word_pleading_carries_the_same_page_rail_caption_and_footer() {
    let work = matter_tree();
    for (template, hearing) in [("rfa_responses", true), ("notice_of_compliance", false)] {
        let docx = render(&work, template, "word");
        let document = docx_part(&docx, "word/document.xml");

        // US Letter; 1in top, 1.5in left, 0.5in right; the bottom margin
        // holds the footer.
        assert!(document.contains(r#"<w:pgSz w:w="12240" w:h="15840"/>"#));
        let margins = document.split("<w:pgMar ").nth(1).unwrap();
        assert!(
            margins.starts_with(r#"w:top="1440" w:right="720""#),
            "{margins}"
        );
        assert!(margins.contains(r#"w:left="2160""#));
        let bottom = numbers_after(&document, "<w:pgMar ", "w:bottom")[0];
        assert!(
            bottom >= 960,
            "bottom margin {bottom} twips holds the footer"
        );

        // Body type is Tinos at 12pt or larger on a 24pt exact pitch.
        let styles = docx_part(&docx, "word/styles.xml");
        let normal = styles.split(r#"w:styleId="Normal""#).nth(1).unwrap();
        let normal = &normal[..normal.find("</w:style>").unwrap()];
        assert!(normal.contains(r#"w:ascii="Tinos""#), "{normal}");
        assert!(
            numbers_after(normal, "<w:sz ", "w:val")[0] >= 24,
            "{normal}"
        );
        assert!(normal.contains(r#"w:line="480" w:lineRule="exact""#));

        // The rail: 28 numbers in order, one 24pt line each, from line 1
        // an inch down.
        let header = docx_part(&docx, "word/header1.xml");
        let rail: Vec<u32> = header
            .split("<w:t>")
            .skip(1)
            .map(|t| t[..t.find("</w:t>").unwrap()].parse().unwrap())
            .collect();
        assert_eq!(rail, (1..=28).collect::<Vec<u32>>());
        assert_eq!(numbers_after(&header, "<w:framePr ", "w:y")[0], 1440);
        assert!(header.contains(r#"w:line="480" w:lineRule="exact""#));

        // First page: counsel block first, seven 24pt lines tall so the
        // court's title starts at or below 3⅓in (4800 twips) and the clerk's
        // space beside it stays an empty cell.
        let tables: Vec<&str> = document.split("<w:tbl>").skip(1).collect();
        assert_eq!(tables.len(), 2, "counsel block and caption");
        assert!(document.find("<w:tbl>").unwrap() < document.find("SUPERIOR").unwrap());
        let counsel_rows = numbers_after(tables[0], "<w:trHeight ", "w:val");
        assert!(counsel_rows[0] >= 7 * 480, "{counsel_rows:?}");
        assert!(1440 + counsel_rows[0] >= 4800);
        let columns = numbers_after(tables[0], "<w:gridCol ", "w:w");
        assert_eq!(columns[0], 9360 * 58 / 100);
        // The caption is one row of two ruled cells: no header row to repeat.
        assert_eq!(
            tables[1]
                .split("</w:tbl>")
                .next()
                .unwrap()
                .matches("<w:tr>")
                .count(),
            1
        );
        assert!(!document.contains("w:tblHeader"));
        assert!(!document.contains("Case information") && !document.contains(">Parties<"));
        assert_eq!(document.contains("Hearing Date:"), hearing);
        assert!(!document.contains('['), "no bracketed placeholder");

        // Footer: the page number, a printed rule, then the title at 10pt.
        let footer = docx_part(&docx, "word/footer1.xml");
        let number = footer.find(" PAGE ").unwrap();
        let rule = footer.find(r#"<w:pBdr><w:top w:val="single""#).unwrap();
        let title = footer.find("PLAINTIFF").unwrap();
        assert!(number < rule && rule < title);
        assert!(numbers_after(&footer[rule..], "<w:sz ", "w:val")[0] >= 20);
    }
}
