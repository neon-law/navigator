//! The shared court-derived name for a filing and its Pointer.

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FilingNameError(String);

/// `4` becomes `0004` and `29-1` becomes `0029-1`: the leading digits are
/// zero-padded to four and an attachment suffix is kept as the court prints it.
fn dkt_number(entry_number: &str) -> Result<String, FilingNameError> {
    let (lead, suffix) = match entry_number.split_once('-') {
        Some((lead, suffix)) => (lead, Some(suffix)),
        None => (entry_number, None),
    };
    if lead.is_empty() || !lead.bytes().all(|b| b.is_ascii_digit()) {
        return Err(FilingNameError(format!(
            "entry_number `{entry_number}` does not start with digits"
        )));
    }
    if suffix.is_some_and(|value| value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit())) {
        return Err(FilingNameError(format!(
            "entry_number `{entry_number}` has an invalid attachment suffix"
        )));
    }
    Ok(match suffix {
        Some(suffix) => format!("{lead:0>4}-{suffix}"),
        None => format!("{lead:0>4}"),
    })
}

/// Minimal path-safety: each of `/ \ : * ? " < > |` and every control
/// character becomes `-`, runs of whitespace collapse to one space, and
/// leading or trailing dots and spaces are dropped. An empty result reads
/// `Untitled`. Nothing else about the title changes; the entry's own `title`
/// field in the YAML stays verbatim.
fn safe_title(title: &str) -> String {
    let replaced: String = title
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '-'
            } else {
                c
            }
        })
        .collect();
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|c| c == '.' || c == ' ');
    if trimmed.is_empty() {
        "Untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The character limit for a filing title; the filesystem byte limit also applies.
const TITLE_MAX_CHARS: usize = 120;

/// `DKT NNNN[-att] - <title>.<ext>`: the name of a Docket Filings document.
/// Unsupported filename characters become `-`; titles fit both 120 characters
/// and the filesystem's 255-byte filename limit.
///
/// # Errors
/// Rejects a nonnumeric entry number or attachment suffix, or a number and
/// extension that exceed the filename byte limit.
pub fn filing_name(entry_number: &str, title: &str, ext: &str) -> Result<String, FilingNameError> {
    let number = dkt_number(entry_number.trim())?;
    let safe = safe_title(title);
    let overhead = format!("DKT {number} - .{ext}").len();
    let budget = 255_usize.checked_sub(overhead).ok_or_else(|| {
        FilingNameError("DKT Number and extension exceed filename length".to_string())
    })?;
    let mut cut = String::new();
    for character in safe.chars().take(TITLE_MAX_CHARS) {
        if cut.len() + character.len_utf8() > budget {
            break;
        }
        cut.push(character);
    }
    let cut = cut.trim_end_matches(['.', ' ']);
    Ok(format!("DKT {number} - {cut}.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filing_names_preserve_court_numbers_and_safe_titles() {
        assert_eq!(
            filing_name("29-1", "Motion re: A/B", "pdf").unwrap(),
            "DKT 0029-1 - Motion re- A-B.pdf"
        );
        assert!(filing_name("4-../../x", "Order", "pdf").is_err());
        assert!(filing_name("4-", "Order", "pdf").is_err());
        assert!(filing_name("x", "Order", "pdf").is_err());
        assert!(filing_name("1", &"🗂".repeat(300), "pdf").unwrap().len() <= 255);
    }
}
