//! `C004` — a document's filename must be `YYYY.MM.DD_name.ext`.
//!
//! A matter folder does not sort chronologically when a document keeps
//! whatever name its uploader typed, and an agent filing a new document has
//! to guess both a name and a date. The convention is the dotted calendar
//! date, one underscore, and a short lowercase hyphenated name —
//! `2026.10.12_complaint-annulment.pdf` — so the folder sorts by date and
//! the name survives any filesystem.
//!
//! The rules engine never sees the documents (they are the git-ignored
//! `documents/` bytes: PDFs, images, DOCX), so the check does not run
//! through `ClassifiedRuleEngine` like `C003`'s does. The CLI's document
//! walk (`project gate --check`, `project documents check`) enforces it on
//! every file and reports this code.

use chrono::NaiveDate;

/// The rule code the CLI's document walk reports.
pub const CODE: &str = "C004";

/// True when `filename` is `YYYY.MM.DD_name.ext`: a real calendar date with
/// dots, exactly one underscore, and a lowercase hyphenated name.
///
/// The date is parsed strictly, so an impossible date (Feb 31, Feb 29 off a
/// leap year) is rejected rather than passing the gate and unsorting the
/// folder it lands in. The name is `[a-z0-9-]+` with no leading, trailing,
/// or doubled hyphen. The one underscore separates the date from the name —
/// except the `_signed` suffix, which composes with the convention: a
/// signed copy is `YYYY.MM.DD_name_signed.pdf` (the signed-copy rule
/// requires that exact suffix on `onboarding/` and `offboarding/` bytes).
#[must_use]
pub fn is_conventional_filename(filename: &str) -> bool {
    let Some((stem, extension)) = filename.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || extension.is_empty() {
        return false;
    }
    let Some((date_part, name)) = stem.split_once('_') else {
        return false;
    };
    if NaiveDate::parse_from_str(date_part, "%Y.%m.%d").is_err() {
        return false;
    }
    let name = name.strip_suffix("_signed").unwrap_or(name);
    is_hyphenated_name(name)
}

/// True when `filename` is Markdown work product: an `.md` or `.markdown`
/// file. A memo or draft is the lawyer's editable source, not a filed
/// document, so the convention does not govern it.
#[must_use]
pub fn is_markdown_work_product(filename: &str) -> bool {
    matches!(
        filename.rsplit_once('.').map(|(_, extension)| extension),
        Some("md" | "markdown")
    )
}

/// True when `dir_name` carries the date itself — `2026-09-05` or
/// `2026.09.05` — as exhibit and discovery folders do. A file inside one
/// is dated by its folder; the convention does not ask for the date twice.
#[must_use]
pub fn is_date_named_directory(dir_name: &str) -> bool {
    NaiveDate::parse_from_str(dir_name, "%Y-%m-%d").is_ok()
        || NaiveDate::parse_from_str(dir_name, "%Y.%m.%d").is_ok()
}

/// True when `name` is a non-empty run of lowercase letters and digits
/// joined by single hyphens.
fn is_hyphenated_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The conventional name for a document dated by an ISO `matter:` block
/// (`YYYY-MM-DD`): the stem as a lowercase hyphenated slug and the
/// extension unchanged —
/// `proposed_filename("2026-10-12", "complaint-annulment-2026-10-12.pdf")`
/// is `2026.10.12_complaint-annulment.pdf`: a date embedded in the stem is
/// folded into the prefix rather than kept twice. `None` when neither the
/// stem nor `dated_on` carries a parsable date, or nothing of the stem
/// survives the slug, so a caller proposes only when it can say so and
/// never falls back to a guess.
#[must_use]
pub fn proposed_filename(dated_on: &str, filename: &str) -> Option<String> {
    // An already-conventional name proposes itself — its dotted date is
    // its own, and a slug would only mangle it.
    if is_conventional_filename(filename) {
        return Some(filename.to_string());
    }
    let (stem, extension) = filename.rsplit_once('.')?;
    // The file's own ISO date is the more specific one: a leading or
    // trailing `YYYY-MM-DD` in the stem becomes the prefix rather than
    // living in the name too. The matter's `filed_on` names the day only
    // when the stem carries no date of its own.
    let (stem, date) = match embedded_date(stem) {
        Some((stem, date)) => (stem, date),
        None => (
            stem.to_string(),
            NaiveDate::parse_from_str(dated_on.trim(), "%Y-%m-%d").ok()?,
        ),
    };
    let slug = name_slug(&stem)?;
    Some(format!("{}_{slug}.{extension}", date.format("%Y.%m.%d")))
}

/// The stem without a leading or trailing `YYYY-MM-DD` segment and its
/// separator, paired with the date it carried, when one is there. A stem
/// that *is* a date is left alone — it keeps nothing, and the proposal
/// declines. `None` when no embedded date is there.
fn embedded_date(stem: &str) -> Option<(String, NaiveDate)> {
    if stem.len() >= 11 {
        let (head, tail) = stem.split_at(10);
        if let Ok(date) = NaiveDate::parse_from_str(head, "%Y-%m-%d") {
            if let Some(rest) = tail.strip_prefix(['-', '_', ' ']) {
                if !rest.is_empty() {
                    return Some((rest.to_string(), date));
                }
            }
        }
    }
    let position = stem.len().checked_sub(10)?;
    if !stem.is_char_boundary(position) {
        return None;
    }
    let (head, tail) = stem.split_at(position);
    let date = NaiveDate::parse_from_str(tail, "%Y-%m-%d").ok()?;
    Some((head.trim_end_matches(['-', '_', ' ']).to_string(), date))
}

/// The stem as a path-safe slug: ASCII letters and digits survive
/// lowercased, every run of other characters becomes one hyphen, and the
/// result is trimmed and capped at 64 characters — the same shape the
/// saved-mail name derives from its subject. `None` when nothing survives.
fn name_slug(stem: &str) -> Option<String> {
    let mut slug = String::new();
    let mut trailing_separator = true;
    for character in stem.chars() {
        if slug.len() >= 64 {
            break;
        }
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            trailing_separator = false;
        } else if !trailing_separator {
            slug.push('-');
            trailing_separator = true;
        }
    }
    let slug = slug.trim_matches('-');
    (!slug.is_empty()).then(|| slug.to_string())
}

/// The gate finding for `filename`: names this code, the convention, and
/// the shape to rename to.
#[must_use]
pub fn finding_message(filename: &str) -> String {
    format!(
        "filename `{filename}` does not follow the document filename convention \
         `YYYY.MM.DD_name.ext` ({CODE}) — a dotted calendar date, one underscore, \
         and a lowercase hyphenated name, e.g. `2026.10.12_complaint-annulment.pdf`"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        finding_message, is_conventional_filename, is_date_named_directory,
        is_markdown_work_product, proposed_filename,
    };

    #[test]
    fn accepts_the_conventional_shape() {
        assert!(is_conventional_filename(
            "2026.10.12_complaint-annulment.pdf"
        ));
        assert!(is_conventional_filename("2026.01.02_letter.eml"));
        assert!(is_conventional_filename("2024.02.29_leap-day.png"));
        // Extensions are the document's own; the convention governs the stem.
        assert!(is_conventional_filename("2026.10.12_exhibit-1.PNG"));
    }

    /// A signed copy is the convention with `_signed` appended, so the
    /// suffix composes with it (`…_engagement-letter_signed.pdf`). A file
    /// actually named `signed` reads as the name and is fine too; an
    /// ordinary name may not borrow an underscore for a second word.
    #[test]
    fn a_signed_suffix_composes_with_the_convention() {
        assert!(is_conventional_filename(
            "2026.10.16_engagement-letter_signed.pdf"
        ));
        assert!(is_conventional_filename("2026.10.12_signed.pdf"));
        assert!(!is_conventional_filename("2026.10.12_two_words.pdf"));
    }

    #[test]
    fn rejects_a_missing_or_misplaced_underscore() {
        assert!(!is_conventional_filename("complaint-annulment.pdf"));
        assert!(!is_conventional_filename("2026.10.12"));
        assert!(!is_conventional_filename("2026.10.12-annulment.pdf"));
        assert!(!is_conventional_filename("20261012_complaint.pdf"));
    }

    #[test]
    fn rejects_a_non_date_or_impossible_date_prefix() {
        assert!(!is_conventional_filename(
            "complaint-annulment-2026-10-12.pdf"
        ));
        assert!(!is_conventional_filename("2026.13.01_complaint.pdf"));
        assert!(!is_conventional_filename("2026.02.31_complaint.pdf"));
        assert!(!is_conventional_filename("2027.02.29_complaint.pdf"));
        // 2024 is a leap year; 2027 is not.
        assert!(is_conventional_filename("2024.02.29_complaint.pdf"));
    }

    #[test]
    fn rejects_an_empty_stem_extension_or_name() {
        assert!(!is_conventional_filename(".pdf"));
        assert!(!is_conventional_filename("2026.10.12_.pdf"));
        assert!(!is_conventional_filename("2026.10.12_complaint."));
        assert!(!is_conventional_filename("no-extension"));
    }

    #[test]
    fn rejects_a_name_that_is_not_lowercase_hyphenated() {
        assert!(!is_conventional_filename("2026.10.12_Complaint.pdf"));
        assert!(!is_conventional_filename(
            "2026.10.12_complaint--annulment.pdf"
        ));
        assert!(!is_conventional_filename("2026.10.12_-complaint.pdf"));
        assert!(!is_conventional_filename("2026.10.12_complaint-.pdf"));
        assert!(!is_conventional_filename(
            "2026.10.12_complaint annulment.pdf"
        ));
    }

    #[test]
    fn proposes_the_dotted_date_and_slugged_name() {
        // A trailing embedded date folds into the prefix, not kept twice.
        assert_eq!(
            proposed_filename("2026-10-12", "complaint-annulment-2026-10-12.pdf").as_deref(),
            Some("2026.10.12_complaint-annulment.pdf")
        );
        // A leading embedded date folds too — and is the more specific date.
        assert_eq!(
            proposed_filename("2026-10-01", "2026-10-09-call.md").as_deref(),
            Some("2026.10.09_call.md")
        );
        assert_eq!(
            proposed_filename("2026-10-12", "Client Screenshot IMG-2645.png").as_deref(),
            Some("2026.10.12_client-screenshot-img-2645.png")
        );
        // An already-conventional name proposes itself.
        assert_eq!(
            proposed_filename("2026-10-12", "2026.10.12_complaint-annulment.pdf").as_deref(),
            Some("2026.10.12_complaint-annulment.pdf")
        );
    }

    #[test]
    fn a_proposal_needs_a_real_date_and_a_surviving_slug() {
        assert_eq!(proposed_filename("2026-10-12", "no-extension"), None);
        assert_eq!(proposed_filename("not a date", "a.pdf"), None);
        assert_eq!(proposed_filename("2026-13-01", "a.pdf"), None);
        assert_eq!(proposed_filename("2026-10-12", "!!!.pdf"), None);
    }

    #[test]
    fn the_finding_names_the_code_and_the_convention() {
        let message = finding_message("complaint-annulment-2026-10-12.pdf");
        assert!(message.contains("C004"), "{message}");
        assert!(message.contains("YYYY.MM.DD_name.ext"), "{message}");
        assert!(
            message.contains("2026.10.12_complaint-annulment.pdf"),
            "{message}"
        );
    }

    #[test]
    fn markdown_work_product_is_not_a_filed_document() {
        assert!(is_markdown_work_product("memo.md"));
        assert!(is_markdown_work_product("agreement.markdown"));
        assert!(!is_markdown_work_product("memo.txt"));
        assert!(!is_markdown_work_product(
            "2026.10.12_complaint-annulment.pdf"
        ));
        assert!(!is_markdown_work_product("no-extension"));
    }

    #[test]
    fn a_date_named_directory_dates_its_own_files() {
        assert!(is_date_named_directory("2026-09-05"));
        assert!(is_date_named_directory("2026.09.05"));
        assert!(!is_date_named_directory("exhibits"));
        assert!(!is_date_named_directory("2026-13-05"));
        assert!(!is_date_named_directory("2026-09-05-morning"));
    }
}
