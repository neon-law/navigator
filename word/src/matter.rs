//! The shared facts of a litigation matter, and the caption they compose.
//!
//! A pleading's caption is not authored per filing. The court, the case
//! number, the judicial officer, the parties, and the counsel block are one
//! [`Matter`], declared once under `matter:` in the Project's
//! `navigator.yaml`; what changes from paper to paper is a [`Filing`]
//! — the caption title, the footer title, and an optional hearing — read
//! from the notation's frontmatter. [`compose`] joins the two into the
//! [`Caption`] both the PDF and the Word render already print, so a case
//! number or counsel address changed in the manifest changes every paper
//! on the next render.
//!
//! Every field is checked by [`Matter::missing`] and [`Filing::missing`],
//! which name the field by the path an author writes, so a refusal says
//! what to add rather than that something is wrong.

use serde::{Deserialize, Serialize};

use crate::paper::Caption;

/// One party, printed in the left cell of the caption.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Party {
    #[serde(default)]
    pub name: String,
    /// The party's role as the court prints it: `Plaintiff`, `Defendant`.
    #[serde(default)]
    pub role: String,
}

/// The lawyer of record, or the party appearing for themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counsel {
    #[serde(default)]
    pub name: String,
    /// The jurisdiction of the bar, as a name (`California`, `Nevada`).
    #[serde(default)]
    pub bar_jurisdiction: String,
    #[serde(default)]
    pub bar_number: String,
    #[serde(default)]
    pub firm: String,
    #[serde(default)]
    pub address: Vec<String>,
    #[serde(default)]
    pub telephone: String,
    #[serde(default)]
    pub email: String,
    /// Who counsel appears for: `Plaintiff SAMPLE PLAINTIFF`.
    #[serde(default)]
    pub appears_for: String,
    /// No lawyer has appeared; the block is the party's own, with no bar
    /// number and an "in pro per" last line.
    #[serde(default)]
    pub pro_per: bool,
}

/// The facts every filing in one matter shares.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matter {
    /// The court's title, one printed line per entry (name, then branch).
    #[serde(default)]
    pub court: Vec<String>,
    #[serde(default)]
    pub case_number: String,
    #[serde(default)]
    pub judicial_officer: String,
    #[serde(default)]
    pub department: String,
    /// `YYYY-MM-DD`.
    #[serde(default)]
    pub filed_on: String,
    /// `YYYY-MM-DD`; absent prints "None set".
    #[serde(default)]
    pub trial_on: Option<String>,
    #[serde(default)]
    pub parties: Vec<Party>,
    #[serde(default)]
    pub counsel: Counsel,
}

/// What differs from one filing to the next: read from the notation's
/// frontmatter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filing {
    /// The full title in the caption's right cell.
    pub caption_title: String,
    /// The short title in the footer (CRC 2.110).
    pub footer_title: String,
    /// `YYYY-MM-DD`.
    pub hearing_date: Option<String>,
    pub hearing_time: Option<String>,
    pub reservation_id: Option<String>,
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A `YYYY-MM-DD` date as the court prints it (`August 14, 2026`), or
/// `None` when the text is not a real calendar date.
#[must_use]
pub fn display_date(iso: &str) -> Option<String> {
    let mut parts = iso.trim().split('-');
    let year: u32 = parts.next()?.parse().ok()?;
    let month: usize = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || year < 1000 || !(1..=12).contains(&month) {
        return None;
    }
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let last = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=last)
        .contains(&day)
        .then(|| format!("{} {day}, {year}", MONTHS[month - 1]))
}

fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

fn single_line(text: &str) -> bool {
    !text.contains('\n') && !text.contains('\r')
}

impl Matter {
    /// Whether the block declares nothing at all: `matter: {}`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The case's short title, `A CORP v. B CORP, et al.`: the first party
    /// of each role in declared order, with `et al.` after a role that has
    /// more. Empty when no party is named.
    #[must_use]
    pub fn short_title(&self) -> String {
        let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
        for party in self.parties.iter().filter(|p| !blank(&p.name)) {
            match groups.last_mut() {
                Some((role, names)) if *role == party.role => names.push(party.name.trim()),
                _ => groups.push((party.role.as_str(), vec![party.name.trim()])),
            }
        }
        groups
            .iter()
            .map(|(_, names)| {
                if names.len() > 1 {
                    format!("{}, et al.", names[0])
                } else {
                    names[0].to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" v. ")
    }

    /// Every required field that is absent or malformed, by the path an
    /// author writes it under `matter:`. Empty when the matter can caption
    /// a pleading.
    #[must_use]
    pub fn missing(&self) -> Vec<String> {
        let mut missing = Vec::new();
        let mut need = |ok: bool, path: &str| {
            if !ok {
                missing.push(format!("matter.{path}"));
            }
        };
        need(
            !self.court.is_empty() && self.court.iter().all(|l| !blank(l) && single_line(l)),
            "court",
        );
        need(!blank(&self.case_number), "case_number");
        need(!blank(&self.judicial_officer), "judicial_officer");
        need(!blank(&self.department), "department");
        need(display_date(&self.filed_on).is_some(), "filed_on");
        if let Some(trial) = &self.trial_on {
            need(display_date(trial).is_some(), "trial_on");
        }
        need(
            self.parties
                .iter()
                .all(|p| !blank(&p.name) && !blank(&p.role))
                && self.parties.len() >= 2,
            "parties",
        );
        let c = &self.counsel;
        need(!blank(&c.name), "counsel.name");
        need(!blank(&c.appears_for), "counsel.appears_for");
        need(
            !c.address.is_empty() && c.address.iter().all(|l| !blank(l)),
            "counsel.address",
        );
        need(!blank(&c.telephone), "counsel.telephone");
        need(!blank(&c.email), "counsel.email");
        if !c.pro_per {
            need(!blank(&c.bar_jurisdiction), "counsel.bar_jurisdiction");
            need(!blank(&c.bar_number), "counsel.bar_number");
        }
        missing
    }
}

impl Filing {
    /// Every required frontmatter field that is absent or malformed.
    #[must_use]
    pub fn missing(&self) -> Vec<String> {
        let mut missing = Vec::new();
        if blank(&self.caption_title) {
            missing.push("caption_title".to_string());
        }
        if blank(&self.footer_title) {
            missing.push("footer_title".to_string());
        }
        if let Some(date) = &self.hearing_date {
            if display_date(date).is_none() {
                missing.push("hearing_date".to_string());
            }
        }
        missing
    }

    fn has_hearing(&self) -> bool {
        self.hearing_date.is_some() || self.hearing_time.is_some() || self.reservation_id.is_some()
    }
}

/// The text a court-paper line carries when a hearing field is unset.
const FILL: &str = "__________";

fn bar_label(jurisdiction: &str) -> String {
    if jurisdiction.eq_ignore_ascii_case("california") {
        "SBN".to_string()
    } else {
        format!("{jurisdiction} Bar No.")
    }
}

fn counsel_lines(counsel: &Counsel) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(if counsel.pro_per && blank(&counsel.bar_number) {
        counsel.name.clone()
    } else {
        format!(
            "{} ({} {})",
            counsel.name,
            bar_label(&counsel.bar_jurisdiction),
            counsel.bar_number
        )
    });
    if !blank(&counsel.firm) {
        lines.push(counsel.firm.clone());
    }
    lines.extend(counsel.address.iter().cloned());
    lines.push(format!("Telephone: {}", counsel.telephone));
    lines.push(format!("Email: {}", counsel.email));
    lines.push(if counsel.pro_per {
        format!("{}, in pro per", counsel.appears_for)
    } else {
        format!("Attorneys for {}", counsel.appears_for)
    });
    lines
}

fn party_lines(parties: &[Party]) -> Vec<String> {
    let mut groups: Vec<(&str, Vec<&Party>)> = Vec::new();
    for party in parties {
        match groups.last_mut() {
            Some((role, members)) if *role == party.role => members.push(party),
            _ => groups.push((party.role.as_str(), vec![party])),
        }
    }
    let last = groups.len().saturating_sub(1);
    let mut lines = Vec::new();
    for (index, (role, members)) in groups.iter().enumerate() {
        lines.extend(members.iter().map(|party| format!("{},", party.name)));
        let plural = if members.len() > 1 { "s" } else { "" };
        let stop = if index == last { "." } else { "," };
        lines.push(format!("{role}{plural}{stop}"));
        if index != last {
            lines.push("v.".to_string());
        }
    }
    lines
}

/// Join a matter and a filing into the caption both renders print.
///
/// The caption's `footer` is the filing's short title. The right cell runs
/// case number, department, judicial officer, the filing's title, any
/// hearing, then the action-filed and trial dates. A hearing prints when
/// any of its three fields is declared, with each unset one a blank fill
/// line; a filing declaring none prints no hearing lines.
#[must_use]
pub fn compose(matter: &Matter, filing: &Filing) -> Caption {
    let mut case = vec![
        format!("Case No. {}", matter.case_number),
        format!("Dept. {}", matter.department),
        format!("Judge: {}", matter.judicial_officer),
        filing.caption_title.clone(),
    ];
    if filing.has_hearing() {
        let shown = |value: &Option<String>| value.clone().unwrap_or_else(|| FILL.to_string());
        let date = filing
            .hearing_date
            .as_deref()
            .and_then(display_date)
            .unwrap_or_else(|| FILL.to_string());
        case.push(format!("Hearing Date: {date}"));
        case.push(format!("Time: {}", shown(&filing.hearing_time)));
        case.push(format!("Reservation ID: {}", shown(&filing.reservation_id)));
    }
    case.push(format!(
        "Action Filed: {}",
        display_date(&matter.filed_on).unwrap_or_default()
    ));
    case.push(format!(
        "Trial Date: {}",
        matter
            .trial_on
            .as_deref()
            .and_then(display_date)
            .unwrap_or_else(|| "None set".to_string())
    ));
    Caption {
        counsel: counsel_lines(&matter.counsel),
        court: matter.court.clone(),
        parties: party_lines(&matter.parties),
        case,
        footer: Some(filing.footer_title.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn party(name: &str, role: &str) -> Party {
        Party {
            name: name.into(),
            role: role.into(),
        }
    }

    fn matter() -> Matter {
        Matter {
            court: vec!["SUPERIOR COURT".into(), "COUNTY OF SAMPLE".into()],
            case_number: "00CV000000".into(),
            judicial_officer: "Hon. Sample Jurist".into(),
            department: "12".into(),
            filed_on: "2026-02-02".into(),
            trial_on: None,
            parties: vec![
                party("A CORP", "Plaintiff"),
                party("B CORP", "Defendant"),
                party("C CORP", "Defendant"),
            ],
            counsel: Counsel {
                name: "Pat Sample".into(),
                bar_jurisdiction: "California".into(),
                bar_number: "000000".into(),
                firm: "SAMPLE LAW".into(),
                address: vec!["1 Example Way".into(), "Sampleton, CA 90000".into()],
                telephone: "(000) 555-0100".into(),
                email: "pat@example.invalid".into(),
                appears_for: "Plaintiff A CORP".into(),
                pro_per: false,
            },
        }
    }

    fn filing() -> Filing {
        Filing {
            caption_title: "NOTICE".into(),
            footer_title: "NOTICE".into(),
            ..Filing::default()
        }
    }

    #[test]
    fn the_short_title_names_the_first_party_of_each_side() {
        assert_eq!(matter().short_title(), "A CORP v. B CORP, et al.");
        assert_eq!(Matter::default().short_title(), "");
        assert!(Matter::default().is_empty());
        assert!(!matter().is_empty());
    }

    #[test]
    fn a_complete_matter_and_filing_miss_nothing() {
        assert!(matter().missing().is_empty());
        assert!(filing().missing().is_empty());
    }

    #[test]
    fn each_missing_field_is_named_by_its_manifest_path() {
        let mut m = matter();
        m.case_number.clear();
        m.counsel.email = " ".into();
        m.filed_on = "2026-02-30".into();
        m.parties.truncate(1);
        assert_eq!(
            m.missing(),
            [
                "matter.case_number",
                "matter.filed_on",
                "matter.parties",
                "matter.counsel.email"
            ]
        );
        let none = Filing::default();
        assert_eq!(none.missing(), ["caption_title", "footer_title"]);
    }

    #[test]
    fn a_pro_per_block_needs_no_bar_number() {
        let mut m = matter();
        m.counsel.pro_per = true;
        m.counsel.bar_number.clear();
        m.counsel.bar_jurisdiction.clear();
        assert!(m.missing().is_empty());
        let lines = compose(&m, &filing()).counsel;
        assert_eq!(lines[0], "Pat Sample");
        assert_eq!(lines.last().unwrap(), "Plaintiff A CORP, in pro per");
    }

    #[test]
    fn the_counsel_block_is_seven_lines_with_the_bar_label_of_its_jurisdiction() {
        let caption = compose(&matter(), &filing());
        assert_eq!(caption.counsel.len(), 7);
        assert_eq!(caption.counsel[0], "Pat Sample (SBN 000000)");
        assert_eq!(caption.counsel[6], "Attorneys for Plaintiff A CORP");
        let mut nv = matter();
        nv.counsel.bar_jurisdiction = "Nevada".into();
        assert_eq!(
            compose(&nv, &filing()).counsel[0],
            "Pat Sample (Nevada Bar No. 000000)"
        );
    }

    #[test]
    fn parties_print_each_role_once_with_the_versus_line_between() {
        assert_eq!(
            compose(&matter(), &filing()).parties,
            [
                "A CORP,",
                "Plaintiff,",
                "v.",
                "B CORP,",
                "C CORP,",
                "Defendants."
            ]
        );
    }

    #[test]
    fn dates_print_as_a_court_writes_them_and_bad_dates_are_refused() {
        assert_eq!(
            display_date("2026-08-14").as_deref(),
            Some("August 14, 2026")
        );
        assert_eq!(
            display_date("2028-02-29").as_deref(),
            Some("February 29, 2028")
        );
        assert_eq!(display_date("2026-02-29"), None);
        assert_eq!(display_date("14 August 2026"), None);
        assert_eq!(display_date("2026-13-01"), None);
    }

    #[test]
    fn the_right_cell_runs_case_filing_title_hearing_then_dates() {
        let mut f = filing();
        f.hearing_date = Some("2026-08-14".into());
        let caption = compose(&matter(), &f);
        assert_eq!(
            caption.case,
            [
                "Case No. 00CV000000",
                "Dept. 12",
                "Judge: Hon. Sample Jurist",
                "NOTICE",
                "Hearing Date: August 14, 2026",
                "Time: __________",
                "Reservation ID: __________",
                "Action Filed: February 2, 2026",
                "Trial Date: None set",
            ]
        );
        assert_eq!(caption.footer.as_deref(), Some("NOTICE"));
    }

    #[test]
    fn a_filing_without_a_hearing_prints_no_hearing_lines() {
        let caption = compose(&matter(), &filing());
        assert!(caption.case.iter().all(|l| !l.contains("Hearing")));
    }
}
