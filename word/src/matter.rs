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
    /// Blank when the court assigns none: the caption leaves the line out.
    #[serde(default)]
    pub judicial_officer: String,
    /// Blank when the court assigns none: the caption leaves the line out.
    #[serde(default)]
    pub department: String,
    /// `YYYY-MM-DD`; blank leaves the action-filed and trial lines out.
    #[serde(default)]
    pub filed_on: String,
    /// `YYYY-MM-DD`; absent prints "None set" under a filed date.
    #[serde(default)]
    pub trial_on: Option<String>,
    /// `YYYY-MM-DD`; the date the signature block's "Dated this …" line
    /// carries (LAW-242). A draft without one prints a highlighted
    /// placeholder, and a final render without one is refused.
    #[serde(default)]
    pub signed_on: Option<String>,
    /// What an "In re" caption is about — `the Marriage of` — for a case
    /// captioned by its subject rather than as one side against another.
    /// Blank captions the parties `A v. B`.
    #[serde(default)]
    pub in_re: String,
    #[serde(default)]
    pub parties: Vec<Party>,
    #[serde(default)]
    pub counsel: Counsel,
    /// The court's clerk wants the total page count at the top left of
    /// page 1 (a county local rule).
    #[serde(default)]
    pub page_count: bool,
}

/// How a court lays out the caption [`compose`] builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionStyle {
    /// Counsel block top left, `Case No.`, department, judge, and the
    /// action-filed and trial dates (California, Nevada, federal).
    Trial,
    /// Washington superior court (GR 14): no counsel block above the
    /// caption — counsel's office prints in every page's footer — and a
    /// right cell of `No.` and the title.
    Washington,
}

impl CaptionStyle {
    /// The style a template's `jurisdiction:` code selects.
    #[must_use]
    pub fn for_jurisdiction(code: &str) -> Self {
        if code.trim() == "WA" {
            Self::Washington
        } else {
            Self::Trial
        }
    }
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

/// `Dated this 11th day of November, 2026.` — the line a pleading's
/// signature block carries (LAW-242), sourced only from the matter's
/// `signed_on`. `None` when that is not a real calendar date.
#[must_use]
pub fn date_line(signed_on: &str) -> Option<String> {
    let date = display_date(signed_on)?;
    let (month, rest) = date.split_once(' ')?;
    let (day, year) = rest.split_once(',')?;
    let day: u32 = day.parse().ok()?;
    Some(format!(
        "Dated this {} day of {month},{year}.",
        ordinal(day)
    ))
}

/// A day of the month as a court writes it: `11th`, `1st`, `23rd`. The
/// teens (11th–13th) all end in `th`.
fn ordinal(day: u32) -> String {
    let teen = (11..=13).contains(&day);
    match (day % 10, teen) {
        (1, false) => format!("{day}st"),
        (2, false) => format!("{day}nd"),
        (3, false) => format!("{day}rd"),
        _ => format!("{day}th"),
    }
}

fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// The value a matter declares for a department or judicial officer the court
/// has not yet assigned. It satisfies `Y019` and the caption prints it as
/// "To be assigned"; a guessed name would be worse than a stated gap.
pub const PENDING: &str = "pending";

fn is_pending(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case(PENDING)
}

/// A department or judge as the caption prints it.
fn assignment(text: &str) -> &str {
    if is_pending(text) {
        "To be assigned"
    } else {
        text
    }
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
        need(
            blank(&self.filed_on) || display_date(&self.filed_on).is_some(),
            "filed_on",
        );
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

/// The text a court-paper line carries when a field is unset: a hearing
/// field, or the case number of a paper filed before the clerk assigns one.
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
    // The Nevada convention puts the name and the bar number on their own
    // lines, not parenthesized into one (LAW-242). A self-represented party
    // with no bar number is not Esq. and carries no bar line.
    if counsel.pro_per && blank(&counsel.bar_number) {
        lines.push(counsel.name.clone());
    } else {
        lines.push(format!("{}, Esq.", counsel.name));
        lines.push(format!(
            "{} {}",
            bar_label(&counsel.bar_jurisdiction),
            counsel.bar_number
        ));
    }
    if !blank(&counsel.firm) {
        lines.push(counsel.firm.clone());
    }
    lines.extend(counsel.address.iter().cloned());
    lines.push(format!("Telephone: {}", counsel.telephone));
    lines.push(format!("Email: {}", counsel.email));
    // One lawyer signs; the singular is always right here.
    lines.push(if counsel.pro_per {
        format!("{}, in pro per", counsel.appears_for)
    } else {
        format!("Attorney for {}", counsel.appears_for)
    });
    lines
}

/// An "In re" caption: what the case is about, then each side's role and
/// names, the second side introduced by "And".
fn in_re_lines(in_re: &str, parties: &[Party]) -> Vec<String> {
    let mut lines = vec![format!("In re {}:", in_re.trim())];
    let mut role: Option<&str> = None;
    for party in parties {
        if role != Some(party.role.as_str()) {
            let and = if role.is_some() { "And " } else { "" };
            lines.push(format!("{and}{}:", party.role));
            role = Some(party.role.as_str());
        }
        lines.push(party.name.clone());
    }
    lines
}

/// The office lines a Washington paper prints in its footer: the firm (or
/// counsel's own name when appearing alone), the address, and how to reach
/// counsel.
fn office_lines(counsel: &Counsel) -> Vec<String> {
    let mut lines = vec![if blank(&counsel.firm) {
        counsel.name.clone()
    } else {
        counsel.firm.clone()
    }];
    lines.extend(counsel.address.iter().cloned());
    lines.push(format!("Tel. {} · {}", counsel.telephone, counsel.email));
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

/// Join a matter and a filing into the caption both renders print, laid
/// out as `style` lays a caption out.
///
/// The caption's `footer` is the filing's short title. The right cell runs
/// case number — a blank fill line before the clerk assigns one —
/// department, judicial officer, the filing's title, any hearing, then (in
/// the trial style) the action-filed and trial dates. A department or
/// judicial officer the matter leaves blank prints no line, and a blank
/// filed date prints neither date line: many courts never assign the first
/// two. A hearing prints when any of its three fields is declared, with
/// each unset one a blank fill line; a filing declaring none prints no
/// hearing lines.
#[must_use]
pub fn compose(matter: &Matter, filing: &Filing, style: CaptionStyle) -> Caption {
    let number = if blank(&matter.case_number) {
        FILL
    } else {
        matter.case_number.as_str()
    };
    let mut case = vec![match style {
        CaptionStyle::Trial => format!("Case No. {number}"),
        CaptionStyle::Washington => format!("No. {number}"),
    }];
    if !blank(&matter.department) {
        case.push(format!("Dept. {}", assignment(&matter.department)));
    }
    if !blank(&matter.judicial_officer) {
        case.push(format!("Judge: {}", assignment(&matter.judicial_officer)));
    }
    case.push(filing.caption_title.clone());
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
    let filed = display_date(&matter.filed_on).filter(|_| style == CaptionStyle::Trial);
    if let Some(filed) = filed {
        case.push(format!("Action Filed: {filed}"));
        case.push(format!(
            "Trial Date: {}",
            matter
                .trial_on
                .as_deref()
                .and_then(display_date)
                .unwrap_or_else(|| "None set".to_string())
        ));
    }
    let parties = if blank(&matter.in_re) {
        party_lines(&matter.parties)
    } else {
        in_re_lines(&matter.in_re, &matter.parties)
    };
    let (counsel, office) = match style {
        CaptionStyle::Trial => (counsel_lines(&matter.counsel), Vec::new()),
        CaptionStyle::Washington => (Vec::new(), office_lines(&matter.counsel)),
    };
    Caption {
        counsel,
        court: matter.court.clone(),
        parties,
        case,
        footer: Some(filing.footer_title.clone()),
        office,
        page_count: matter.page_count,
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
            signed_on: None,
            in_re: String::new(),
            page_count: false,
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
    fn a_pending_assignment_gates_and_captions_as_to_be_assigned() {
        let mut m = matter();
        m.department = "pending".into();
        m.judicial_officer = " Pending ".into();
        assert!(m.missing().is_empty());
        let caption = compose(&m, &filing(), CaptionStyle::Trial);
        assert!(caption.case.contains(&"Dept. To be assigned".to_string()));
        assert!(caption.case.contains(&"Judge: To be assigned".to_string()));
    }

    /// LAW-361: a Washington family case may never get a judicial officer or
    /// department, and counsel appearing in an existing case may not yet know
    /// the filed date. Blank, they gate clean and the caption leaves them out.
    #[test]
    fn an_unassigned_court_leaves_department_judge_and_dates_out_of_the_caption() {
        let mut m = matter();
        m.department.clear();
        m.judicial_officer = " ".into();
        m.filed_on.clear();
        assert!(m.missing().is_empty(), "{:?}", m.missing());
        let caption = compose(&m, &filing(), CaptionStyle::Trial);
        assert_eq!(caption.case, ["Case No. 00CV000000", "NOTICE"]);
        m.filed_on = "2026-02-02".into();
        assert_eq!(
            compose(&m, &filing(), CaptionStyle::Trial).case,
            [
                "Case No. 00CV000000",
                "NOTICE",
                "Action Filed: February 2, 2026",
                "Trial Date: None set",
            ]
        );
    }

    #[test]
    fn a_complete_matter_and_filing_miss_nothing() {
        assert!(matter().missing().is_empty());
        assert!(filing().missing().is_empty());
    }

    #[test]
    fn each_missing_field_is_named_by_its_manifest_path() {
        let mut m = matter();
        m.counsel.email = " ".into();
        m.filed_on = "2026-02-30".into();
        m.parties.truncate(1);
        assert_eq!(
            m.missing(),
            ["matter.filed_on", "matter.parties", "matter.counsel.email"]
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
        let lines = compose(&m, &filing(), CaptionStyle::Trial).counsel;
        assert_eq!(lines[0], "Pat Sample");
        assert_eq!(lines.last().unwrap(), "Plaintiff A CORP, in pro per");
    }

    #[test]
    fn the_counsel_block_prints_the_bar_on_its_own_line_for_its_jurisdiction() {
        let caption = compose(&matter(), &filing(), CaptionStyle::Trial);
        assert_eq!(caption.counsel.len(), 8);
        assert_eq!(caption.counsel[0], "Pat Sample, Esq.");
        assert_eq!(caption.counsel[1], "SBN 000000");
        assert_eq!(caption.counsel[7], "Attorney for Plaintiff A CORP");
        let mut nv = matter();
        nv.counsel.bar_jurisdiction = "Nevada".into();
        assert_eq!(
            compose(&nv, &filing(), CaptionStyle::Trial).counsel[1],
            "Nevada Bar No. 000000"
        );
    }

    #[test]
    fn parties_print_each_role_once_with_the_versus_line_between() {
        assert_eq!(
            compose(&matter(), &filing(), CaptionStyle::Trial).parties,
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
    fn the_date_line_writes_the_ordinal_the_court_prints() {
        assert_eq!(
            date_line("2026-11-11").as_deref(),
            Some("Dated this 11th day of November, 2026.")
        );
        assert_eq!(
            date_line("2026-08-01").as_deref(),
            Some("Dated this 1st day of August, 2026.")
        );
        assert_eq!(
            date_line("2027-01-22").as_deref(),
            Some("Dated this 22nd day of January, 2027.")
        );
        assert_eq!(
            date_line("2026-10-23").as_deref(),
            Some("Dated this 23rd day of October, 2026.")
        );
        assert_eq!(
            date_line("2026-12-13").as_deref(),
            Some("Dated this 13th day of December, 2026.")
        );
        assert_eq!(
            date_line("2026-10-31").as_deref(),
            Some("Dated this 31st day of October, 2026.")
        );
        assert_eq!(date_line("2026-02-30"), None);
        assert_eq!(date_line("14 August 2026"), None);
        assert_eq!(date_line(""), None);
    }

    #[test]
    fn the_right_cell_runs_case_filing_title_hearing_then_dates() {
        let mut f = filing();
        f.hearing_date = Some("2026-08-14".into());
        let caption = compose(&matter(), &f, CaptionStyle::Trial);
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
        let caption = compose(&matter(), &filing(), CaptionStyle::Trial);
        assert!(caption.case.iter().all(|l| !l.contains("Hearing")));
    }

    /// LAW-327: a Washington superior-court caption — "In re the Marriage
    /// of", each side introduced by its role, `No.` with a fill line before
    /// the clerk assigns a number, no trial-court dates, and counsel's office
    /// in the footer instead of a block above the court's title.
    #[test]
    fn a_washington_caption_is_in_re_with_no_and_counsel_in_the_footer() {
        let mut m = matter();
        m.court = vec!["SUPERIOR COURT OF WASHINGTON, COUNTY OF SAMPLE".into()];
        m.in_re = "the Marriage of".into();
        m.parties = vec![
            party("PAT ONE", "Petitioner"),
            party("SAM TWO", "Respondent"),
        ];
        m.case_number.clear();
        m.department.clear();
        m.judicial_officer.clear();
        m.page_count = true;
        assert!(m.missing().is_empty(), "{:?}", m.missing());
        let caption = compose(&m, &filing(), CaptionStyle::Washington);
        assert!(caption.counsel.is_empty());
        assert_eq!(
            caption.parties,
            [
                "In re the Marriage of:",
                "Petitioner:",
                "PAT ONE",
                "And Respondent:",
                "SAM TWO"
            ]
        );
        assert_eq!(caption.case, ["No. __________", "NOTICE"]);
        assert_eq!(
            caption.office,
            [
                "SAMPLE LAW",
                "1 Example Way",
                "Sampleton, CA 90000",
                "Tel. (000) 555-0100 · pat@example.invalid"
            ]
        );
        assert!(caption.page_count);
        assert_eq!(
            CaptionStyle::for_jurisdiction("WA"),
            CaptionStyle::Washington
        );
        assert_eq!(CaptionStyle::for_jurisdiction("CA"), CaptionStyle::Trial);
        assert_eq!(
            compose(&m, &filing(), CaptionStyle::Trial).case[0],
            "Case No. __________"
        );
    }
}
