//! Facts read out of incoming correspondence: the dates a message names
//! (and whether its wording makes one a deadline) and the case numbers it
//! cites.
//!
//! A deterministic reading of message text, run each time a thread is
//! shown, so a fact is never stale against the transcript it came from. It
//! writes nothing: a detected date is a prompt for Lawyer Review, never a
//! Deadline on its own.

use chrono::NaiveDate;

/// One calendar date a message names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DetectedDate {
    pub date: NaiveDate,
    /// Whether the words just before the date read as a deadline or a
    /// setting ("due", "no later than", "set for").
    pub deadline: bool,
}

/// What [`read`] found, each list deduplicated in first-seen order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorrespondenceFacts {
    pub dates: Vec<DetectedDate>,
    pub case_numbers: Vec<String>,
}

impl CorrespondenceFacts {
    /// Fold another message's facts into these, keeping first-seen order and
    /// letting a deadline reading of a date win over a plain one.
    pub fn merge(&mut self, other: Self) {
        for detected in other.dates {
            match self
                .dates
                .iter_mut()
                .find(|seen| seen.date == detected.date)
            {
                Some(seen) => seen.deadline |= detected.deadline,
                None => self.dates.push(detected),
            }
        }
        for number in other.case_numbers {
            if !self.case_numbers.contains(&number) {
                self.case_numbers.push(number);
            }
        }
    }
}

/// Full month names; a three- or four-letter prefix ("dec", "sept") reads too.
const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// Words that, shortly before a date, make it a deadline or a setting.
const DEADLINE_CUES: [&str; 11] = [
    "due",
    "set for",
    "scheduled for",
    "deadline",
    "no later than",
    "on or before",
    "by",
    "hearing",
    "respond",
    "expires",
    "until",
];

/// Labels a case number follows.
const CASE_LABELS: [&str; 6] = [
    "case no",
    "case number",
    "cause no",
    "docket no",
    "civil action no",
    "index no",
];

/// Read the dates and case numbers out of one message body.
#[must_use]
pub fn read(text: &str) -> CorrespondenceFacts {
    let mut facts = CorrespondenceFacts::default();
    let lower = text.to_ascii_lowercase();
    let words = words(&lower);
    for (index, &(start, word)) in words.iter().enumerate() {
        let date = month_date(&words, index).or_else(|| numeric_date(word));
        if let Some(date) = date {
            let detected = DetectedDate {
                date,
                deadline: deadline_cue(&lower[..start]),
            };
            facts.merge(CorrespondenceFacts {
                dates: vec![detected],
                case_numbers: Vec::new(),
            });
        }
    }
    for number in case_numbers(text, &lower) {
        if !facts.case_numbers.contains(&number) {
            facts.case_numbers.push(number);
        }
    }
    facts
}

/// Whitespace-separated words with their byte offsets, trailing commas and
/// periods kept so "Dec." and "4," still read.
fn words(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (index, ch) in text.char_indices() {
        match (ch.is_whitespace(), start) {
            (true, Some(begin)) => {
                out.push((begin, &text[begin..index]));
                start = None;
            }
            (false, None) => start = Some(index),
            _ => {}
        }
    }
    if let Some(begin) = start {
        out.push((begin, &text[begin..]));
    }
    out
}

/// "December 4, 2026" / "Dec. 4 2026" starting at `words[index]`.
fn month_date(words: &[(usize, &str)], index: usize) -> Option<NaiveDate> {
    let month_word = words[index]
        .1
        .trim_matches(|c: char| !c.is_ascii_alphabetic());
    let month = MONTHS.iter().position(|full| {
        month_word.len() >= 3
            && (*full == month_word || full.starts_with(month_word) && month_word.len() <= 4)
    })?;
    let day: u32 = words
        .get(index + 1)?
        .1
        .trim_end_matches([',', '.'])
        .trim_end_matches(char::is_alphabetic)
        .parse()
        .ok()?;
    let year: i32 = words
        .get(index + 2)?
        .1
        .trim_end_matches(|c: char| !c.is_ascii_digit())
        .parse()
        .ok()
        .filter(|year: &i32| (1900..=2200).contains(year))?;
    NaiveDate::from_ymd_opt(year, u32::try_from(month).ok()? + 1, day)
}

/// "12/4/2026" or "2026-12-04", trimmed of surrounding punctuation.
fn numeric_date(word: &str) -> Option<NaiveDate> {
    let word = word.trim_matches(|c: char| !c.is_ascii_digit());
    NaiveDate::parse_from_str(word, "%m/%d/%Y")
        .or_else(|_| NaiveDate::parse_from_str(word, "%Y-%m-%d"))
        .ok()
}

/// Whether the ~40 bytes before a date carry a deadline cue as a whole word.
fn deadline_cue(before: &str) -> bool {
    let from = before
        .char_indices()
        .map(|(index, _)| index)
        .find(|&index| before.len() - index <= 40)
        .unwrap_or(before.len());
    let window = &before[from..];
    DEADLINE_CUES.iter().any(|cue| {
        window.match_indices(cue).any(|(at, _)| {
            let left = window[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
            let right = window[at + cue.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
            left && right
        })
    })
}

/// The token after each case label ("Case No. A-24-123456-D"), plus any
/// federal-style number ("2:24-cv-01234") written without one.
fn case_numbers(text: &str, lower: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for label in CASE_LABELS {
        for (at, _) in lower.match_indices(label) {
            let rest = text[at + label.len()..].trim_start_matches(['.', ':', '#', ' ']);
            let token = case_token(rest);
            if token.chars().any(|c| c.is_ascii_digit()) && !out.iter().any(|seen| seen == token) {
                out.push(token.to_string());
            }
        }
    }
    for (_, word) in words(text) {
        let token = case_token(word.trim_start_matches(|c: char| !c.is_ascii_alphanumeric()));
        if is_federal_number(token) && !out.iter().any(|seen| seen == token) {
            out.push(token.to_string());
        }
    }
    out
}

/// The leading run of case-number characters, without trailing punctuation.
fn case_token(rest: &str) -> &str {
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | ':' | '.' | '/')))
        .unwrap_or(rest.len());
    rest[..end].trim_end_matches(['.', ':', '/', '-'])
}

/// `D:YY-tt-NNNNN`, the federal district-court shape, judge initials
/// optional after it.
fn is_federal_number(token: &str) -> bool {
    let mut parts = token.split('-');
    let (Some(head), Some(kind), Some(serial)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let Some((office, year)) = head.split_once(':') else {
        return false;
    };
    office.len() == 1
        && office.chars().all(|c| c.is_ascii_digit())
        && year.len() == 2
        && year.chars().all(|c| c.is_ascii_digit())
        && (2..=3).contains(&kind.len())
        && kind.chars().all(|c| c.is_ascii_alphabetic())
        && serial.len() >= 4
        && serial.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn a_court_notice_yields_its_case_number_and_a_deadline() {
        let facts = read(
            "Re: Case No. A-24-123456-D. Your opposition is due no later than \
             December 4, 2026. The hearing is set for 12/18/2026 at 9:00 a.m.",
        );
        assert_eq!(facts.case_numbers, vec!["A-24-123456-D".to_string()]);
        assert_eq!(
            facts.dates,
            vec![
                DetectedDate {
                    date: date(2026, 12, 4),
                    deadline: true
                },
                DetectedDate {
                    date: date(2026, 12, 18),
                    deadline: true
                },
            ]
        );
    }

    #[test]
    fn a_date_without_a_cue_is_not_a_deadline() {
        let facts =
            read("Thanks for meeting on Nov. 3, 2026; the letter dated 2026-10-30 is attached.");
        assert_eq!(
            facts.dates,
            vec![
                DetectedDate {
                    date: date(2026, 11, 3),
                    deadline: false
                },
                DetectedDate {
                    date: date(2026, 10, 30),
                    deadline: false
                },
            ]
        );
        assert!(facts.case_numbers.is_empty());
    }

    #[test]
    fn a_cue_inside_another_word_does_not_count() {
        let facts = read("Abby wrote on March 2, 2026.");
        assert_eq!(
            facts.dates,
            vec![DetectedDate {
                date: date(2026, 3, 2),
                deadline: false
            }]
        );
    }

    #[test]
    fn federal_numbers_read_without_a_label_and_invalid_dates_do_not() {
        let facts =
            read("Filed in 2:24-cv-01234-JCM, see Docket No: 57; not a date: February 30, 2026.");
        assert_eq!(
            facts.case_numbers,
            vec!["57".to_string(), "2:24-cv-01234-JCM".to_string()]
        );
        assert!(facts.dates.is_empty());
    }

    #[test]
    fn merging_keeps_first_seen_order_and_upgrades_a_deadline() {
        let mut facts = read("We spoke on 12/4/2026 about Case No. 25STCV00001.");
        facts.merge(read(
            "Reply due 12/4/2026 in Case No. 25STCV00001, and Cause No. 2025-12345.",
        ));
        assert_eq!(
            facts.dates,
            vec![DetectedDate {
                date: date(2026, 12, 4),
                deadline: true
            }]
        );
        assert_eq!(
            facts.case_numbers,
            vec!["25STCV00001".to_string(), "2025-12345".to_string()]
        );
    }
}
