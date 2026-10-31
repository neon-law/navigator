//! The server half of the litigation board's sections: pure functions from
//! stored rows to display-ready views, scoped to one lens.
//!
//! Nothing here queries the store and nothing here invents a date. A
//! Docket Entry's date is the one the court set (`scheduled_on`) or the one
//! it was filed on; a Deadline's is its stored due instant. The side an item
//! sits on is derived from the filer and the matter's participation, never
//! typed.

use chrono::{DateTime, Utc};

use super::records::{DeadlineCard, ReceiptRow, RosterGroup, RosterView};
use super::timeline::{CaptionView, DocumentLink, FactStripView, Side, TimelineItem};
use super::SectionRow;

/// How many dated items `key_dates_strip` shows.
pub const KEY_DATES: usize = 5;

/// What the docket sections are built from.
pub struct DocketInputs<'a> {
    pub cases: &'a [store::cases::Case],
    pub entries: &'a [store::cases::DocketEntry],
    pub deadlines: &'a [store::project_deadlines::Deadline],
    /// The Project's live Documents, for each entry's link and visibility.
    pub assets: &'a [store::assets::Asset],
    /// Names that file for our side: the client Entity and the matter's
    /// client and lawyer participants.
    pub our_names: &'a [String],
    pub code: &'a str,
    pub client_lens: bool,
    pub now: DateTime<Utc>,
}

/// A word with its first letter capitalised and underscores spaced.
fn label(raw: &str) -> String {
    let spaced = raw.replace('_', " ");
    let mut chars = spaced.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// The case masthead.
///
/// Each fact the synced `matter:` block declares wins; the stored case row
/// fills the rest, and the Project's name is the title when neither names
/// the case. The posture and status come from the case row only.
#[must_use]
pub fn caption(
    cases: &[store::cases::Case],
    matter: Option<&word::matter::Matter>,
    project_name: &str,
) -> CaptionView {
    let case = cases
        .iter()
        .find(|case| case.status == "open")
        .or_else(|| cases.first());
    let mut view = case.map_or_else(
        || CaptionView {
            caption: project_name.to_string(),
            ..CaptionView::default()
        },
        |case| CaptionView {
            caption: case.caption.clone(),
            court: joined(&[case.forum.as_deref(), case.jurisdiction.as_deref()]),
            case_number: case.docket_number.clone().unwrap_or_default(),
            judge: case.judge.clone().unwrap_or_default(),
            posture: label(&case.posture),
            status: label(&case.status),
            ..CaptionView::default()
        },
    );
    let Some(matter) = matter else {
        return view;
    };
    let prefer = |field: &mut String, synced: &str| {
        if !synced.trim().is_empty() {
            *field = synced.trim().to_string();
        }
    };
    prefer(&mut view.caption, &matter.short_title());
    let court: Vec<Option<&str>> = matter
        .court
        .iter()
        .map(|line| Some(line.as_str()))
        .collect();
    prefer(&mut view.court, &joined(&court));
    prefer(&mut view.case_number, &matter.case_number);
    prefer(&mut view.judge, &matter.judicial_officer);
    prefer(&mut view.department, &matter.department);
    prefer(&mut view.filed_on, &matter.filed_on);
    prefer(
        &mut view.trial_on,
        matter.trial_on.as_deref().unwrap_or_default(),
    );
    view
}

/// The non-blank parts, joined with a middle dot.
fn joined(parts: &[Option<&str>]) -> String {
    parts
        .iter()
        .flatten()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Which side filed `entry`.
///
/// An order, a hearing, or a trial is the court's act and a stipulation is
/// both sides'. Otherwise the filer is ours when it names one of
/// `our_names`, or when it leads with the role the case records for the
/// client (`posture`, e.g. `Plaintiff`); any other named filer is the other
/// side. An entry recorded with no filer stays unattributed rather than
/// guessed.
#[must_use]
pub fn side_of(entry: &store::cases::DocketEntry, posture: &str, our_names: &[String]) -> Side {
    match entry.kind.as_str() {
        "order" | "hearing" | "trial" => return Side::Court,
        "stipulation" => return Side::Joint,
        _ => {}
    }
    let party = entry.party.as_deref().map(str::trim).unwrap_or_default();
    if party.is_empty() {
        return Side::Unattributed;
    }
    if names_ours(party, posture, our_names) {
        Side::Ours
    } else {
        Side::Theirs
    }
}

/// Whether a named party is our side: it names one of `our_names`, or it
/// leads with the role the case records for the client (`posture`, e.g.
/// `Plaintiff`). The one test the timeline's sides and the discovery
/// section's responding party share.
#[must_use]
pub fn names_ours(party: &str, posture: &str, our_names: &[String]) -> bool {
    let party = party.trim().to_lowercase();
    if party.is_empty() {
        return false;
    }
    let ours_by_name = our_names
        .iter()
        .map(|name| name.trim().to_lowercase())
        .filter(|name| name.len() >= 3)
        .any(|name| party.contains(&name));
    let role = posture
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let ours_by_role = !role.is_empty() && party.starts_with(&role);
    ours_by_name || ours_by_role
}

/// `scheduled_on` as the timeline prints it.
fn instant(when: DateTime<Utc>) -> String {
    when.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// A free-text filed date as `YYYY-MM-DD` when it parses, else as recorded.
fn filed(raw: &str) -> (String, Option<DateTime<Utc>>) {
    let raw = raw.trim();
    let date = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .or_else(|_| chrono::NaiveDate::parse_from_str(raw, "%m/%d/%Y"))
        .or_else(|_| chrono::NaiveDate::parse_from_str(raw, "%m/%d/%y"))
        .ok();
    match date {
        Some(date) => (
            date.format("%Y-%m-%d").to_string(),
            date.and_hms_opt(0, 0, 0).map(|naive| naive.and_utc()),
        ),
        None => (raw.to_string(), None),
    }
}

/// One timeline item with the instant it sorts by.
struct Dated {
    at: Option<DateTime<Utc>>,
    item: TimelineItem,
}

/// The docket sections' data for one lens.
#[derive(Debug, Default)]
pub struct Docket {
    /// Every item, in date order.
    pub items: Vec<TimelineItem>,
    pub facts: FactStripView,
    /// The next [`KEY_DATES`] items still ahead, soonest first.
    pub key_dates: Vec<SectionRow>,
}

/// The timeline, its fact strip, and the key dates for one lens.
///
/// The lawyer lens sees every entry and every open Deadline. The client lens
/// sees an entry only when its Document is client-visible, and a Deadline
/// only when it carries a client lead time — the rule the show page's
/// Deadlines panel applies. Items run in date order; an undated entry sorts
/// first so it is never hidden below the fold.
#[must_use]
pub fn docket(inputs: &DocketInputs<'_>) -> Docket {
    let mut rows = entry_rows(inputs);
    rows.extend(deadline_rows(inputs));
    rows.sort_by_key(|row| row.at);
    let facts = fact_strip(&rows, inputs.now);
    let key_dates = ahead(&rows, inputs.now)
        .take(KEY_DATES)
        .map(|row| SectionRow {
            label: row.item.date.clone(),
            detail: if row.item.number.is_empty() {
                row.item.title.clone()
            } else {
                format!("D.I. {} · {}", row.item.number, row.item.title)
            },
            status: if row.item.deadline {
                if row.item.category.is_empty() {
                    row.item.state.clone()
                } else {
                    format!("{} · {}", row.item.category, row.item.state)
                }
            } else {
                row.item.kind.clone()
            },
        })
        .collect();
    Docket {
        items: rows.into_iter().map(|row| row.item).collect(),
        facts,
        key_dates,
    }
}

/// Whether a Deadline's authority binds the date: `Hard` for a statute,
/// court rule, court order, or contract; `Soft` for an internal target or an
/// authority the row does not state.
#[must_use]
pub fn binding(authority_kind: &str) -> &'static str {
    match authority_kind {
        "statute" | "court_rule" | "court_order" | "contract" => "Hard",
        _ => "Soft",
    }
}

/// The deadline board for one lens: the open Deadlines it may see, soonest
/// first, each marked hard or soft. The client lens keeps only rows with a
/// client lead time and measures *due soon* against that lead.
#[must_use]
pub fn deadline_board(
    deadlines: &[store::project_deadlines::Deadline],
    now: DateTime<Utc>,
    client_lens: bool,
) -> Vec<DeadlineCard> {
    crate::project_deadlines::deadline_rows(deadlines, now, client_lens)
        .into_iter()
        .map(|row| {
            let kind = deadlines
                .iter()
                .find(|deadline| deadline.id.to_string() == row.id)
                .map(|deadline| deadline.authority_kind.as_str())
                .unwrap_or_default();
            DeadlineCard {
                title: row.title,
                description: row.description,
                visibility: row.visibility,
                due: row.due,
                binding: binding(kind).to_string(),
                authority: row.authority,
                role: row.role,
                state: row.state,
                overdue: row.overdue,
            }
        })
        .collect()
}

/// What the party roster is built from.
pub struct RosterInputs<'a> {
    /// The synced `matter:` block: its caption parties and counsel of record.
    pub matter: Option<&'a word::matter::Matter>,
    /// The Entity the matter opened against.
    pub client_entity: &'a str,
    /// Each participant as `(name, participation)`.
    pub team: &'a [(String, String)],
    /// The timeline this lens sees; its named filers are parties.
    pub items: &'a [TimelineItem],
    pub services: &'a [store::project_service::ProjectService],
    /// Each Project Reference as `(name, reason)`.
    pub references: &'a [(String, String)],
    pub client_lens: bool,
}

/// The roster's parties: the synced caption's, then the client Entity, then
/// each named filer this lens sees, each name once.
fn roster_parties(inputs: &RosterInputs<'_>) -> Vec<SectionRow> {
    let mut seen = std::collections::HashSet::new();
    let caption = inputs
        .matter
        .map(|m| m.parties.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|party| (party.name.trim(), party.role.trim().to_string()));
    let client = std::iter::once((inputs.client_entity.trim(), "Client".to_string()));
    let filers = inputs
        .items
        .iter()
        .filter(|item| !item.deadline && !matches!(item.side, Side::Court | Side::Unattributed))
        .map(|item| (item.filer.trim(), item.side.label().to_string()));
    caption
        .chain(client)
        .chain(filers)
        .filter(|(name, _)| !name.is_empty() && seen.insert(name.to_lowercase()))
        .map(|(name, detail)| SectionRow {
            label: name.to_string(),
            detail,
            status: String::new(),
        })
        .collect()
}

/// The roster's counsel and team: the synced counsel of record, then the
/// participants this lens may see, each name once.
fn roster_team(inputs: &RosterInputs<'_>) -> Vec<SectionRow> {
    let mut listed = std::collections::HashSet::new();
    let counsel = inputs
        .matter
        .map(|m| &m.counsel)
        .filter(|c| !c.name.trim().is_empty())
        .map(|counsel| {
            let appears_for = counsel.appears_for.trim();
            let detail = match (counsel.pro_per, appears_for.is_empty()) {
                (true, true) => "In pro per".to_string(),
                (true, false) => format!("{appears_for}, in pro per"),
                (false, true) => "Counsel of record".to_string(),
                (false, false) => format!("Counsel of record for {appears_for}"),
            };
            (counsel.name.trim().to_string(), detail)
        });
    let team = inputs
        .team
        .iter()
        .filter(|(_, participation)| {
            !inputs.client_lens || matches!(participation.as_str(), "client" | "lawyer")
        })
        .map(|(name, participation)| (name.trim().to_string(), label(participation)));
    counsel
        .into_iter()
        .chain(team)
        .filter(|(name, _)| listed.insert(name.to_lowercase()))
        .map(|(label, detail)| SectionRow {
            label,
            detail,
            status: String::new(),
        })
        .collect()
}

/// The party roster for one lens.
///
/// Every lens reads the parties (the synced caption's parties with their
/// roles, the client Entity, and each named filer on the timeline it sees),
/// the synced counsel of record, and the matter's client and lawyer
/// participants. A name already listed is not listed again.
/// The service list and the Project References are the firm's conflicts
/// record, so only the lawyer lens reads them; a reference is labelled as
/// carrying no standing on the matter.
#[must_use]
pub fn roster(inputs: &RosterInputs<'_>) -> RosterView {
    let parties = roster_parties(inputs);
    let team = roster_team(inputs);
    let mut groups = vec![
        RosterGroup {
            heading: "Parties".to_string(),
            rows: parties,
        },
        RosterGroup {
            heading: "Counsel and team".to_string(),
            rows: team,
        },
    ];
    if !inputs.client_lens {
        let mut served = std::collections::HashSet::new();
        let service: Vec<SectionRow> = inputs
            .services
            .iter()
            .filter(|row| served.insert((row.recipient.clone(), row.counsel_for.clone())))
            .map(|row| {
                let mut detail = Vec::new();
                if let Some(party) = row.counsel_for.as_deref().filter(|p| !p.is_empty()) {
                    detail.push(format!("for {party}"));
                }
                detail.push(label(&row.method));
                if !row.address.trim().is_empty() {
                    detail.push(row.address.clone());
                }
                SectionRow {
                    label: row.recipient.clone(),
                    detail: detail.join(" · "),
                    status: String::new(),
                }
            })
            .collect();
        groups.push(RosterGroup {
            heading: "Service list".to_string(),
            rows: service,
        });
        groups.push(RosterGroup {
            heading: "Referenced, no standing on the matter".to_string(),
            rows: inputs
                .references
                .iter()
                .map(|(name, reason)| SectionRow {
                    label: name.clone(),
                    detail: label(reason),
                    status: String::new(),
                })
                .collect(),
        });
    }
    groups.retain(|group| !group.rows.is_empty());
    RosterView { groups }
}

/// The e-filing receipts for one lens, most recently changed first.
///
/// The client lens sees a submission only when its submitted or conformed
/// Document is client-visible, and never the firm's note on it.
#[must_use]
pub fn receipts(
    transactions: &[store::efiling_transactions::EfilingTransaction],
    assets: &[store::assets::Asset],
    client_lens: bool,
) -> Vec<ReceiptRow> {
    let asset = |id: Option<uuid::Uuid>| id.and_then(|id| assets.iter().find(|a| a.id == id));
    let visible = |id: Option<uuid::Uuid>| {
        asset(id).is_some_and(|a| a.visibility == store::documents::visibility::CLIENT)
    };
    transactions
        .iter()
        .filter(|tx| {
            !client_lens || visible(tx.submitted_document_id) || visible(tx.conformed_document_id)
        })
        .map(|tx| {
            let title = asset(tx.submitted_document_id)
                .or_else(|| asset(tx.conformed_document_id))
                .and_then(|a| a.filename.clone().or_else(|| a.slug.clone()))
                .unwrap_or_else(|| format!("{} · {}", tx.court, tx.case_number));
            let submitted = tx
                .filed_on
                .clone()
                .unwrap_or_else(|| tx.inserted_at.chars().take(10).collect());
            let note = if client_lens {
                String::new()
            } else if tx.note.is_empty() && tx.status == store::efiling_transactions::STATUS_UNKNOWN
            {
                store::efiling_transactions::UNKNOWN_NOTE.to_string()
            } else {
                tx.note.clone()
            };
            ReceiptRow {
                submitted,
                title,
                transaction: tx.transaction_id.clone(),
                status: label(&tx.status),
                note,
            }
        })
        .collect()
}

/// The Docket Entries this lens sees, each with its document and chain.
fn entry_rows(inputs: &DocketInputs<'_>) -> Vec<Dated> {
    let posture = inputs
        .cases
        .first()
        .map(|case| case.posture.as_str())
        .unwrap_or_default();
    let by_id: std::collections::HashMap<uuid::Uuid, &store::cases::DocketEntry> = inputs
        .entries
        .iter()
        .map(|entry| (entry.id, entry))
        .collect();
    let successor: std::collections::HashMap<uuid::Uuid, &store::cases::DocketEntry> = inputs
        .entries
        .iter()
        .filter_map(|entry| entry.supersedes.map(|prior| (prior, entry)))
        .collect();
    let asset = |id: uuid::Uuid| inputs.assets.iter().find(|asset| asset.id == id);
    let client_visible = |entry: &store::cases::DocketEntry| {
        entry
            .document_asset_id
            .and_then(asset)
            .is_some_and(|asset| asset.visibility == store::documents::visibility::CLIENT)
    };
    inputs
        .entries
        .iter()
        .filter(|entry| !inputs.client_lens || client_visible(entry))
        .map(|entry| {
            let (date, at) = match entry.scheduled_on {
                Some(when) => (instant(when), Some(when)),
                None => entry
                    .filed_or_served_on
                    .as_deref()
                    .map(filed)
                    .unwrap_or_default(),
            };
            let document = entry.document_asset_id.map(|id| DocumentLink {
                label: asset(id)
                    .and_then(|asset| asset.filename.clone().or_else(|| asset.slug.clone()))
                    .unwrap_or_else(|| "Document on file".to_string()),
                href: format!("/app/projects/{}/documents/{id}", inputs.code),
            });
            let mut chain = Vec::new();
            if let Some(next) = successor.get(&entry.id) {
                let when = next.scheduled_on.map(instant).unwrap_or_default();
                chain.push(format!("Continued to {when} (D.I. {})", next.entry_number));
            }
            if let Some(prior) = entry.supersedes.and_then(|id| by_id.get(&id)) {
                chain.push(format!("Continues D.I. {}", prior.entry_number));
            }
            let item = TimelineItem {
                key: entry.id.to_string(),
                number: entry.entry_number.clone(),
                kind: label(&entry.kind),
                title: entry.title.clone(),
                filer: entry.party.clone().unwrap_or_default(),
                side: side_of(entry, posture, inputs.our_names),
                date,
                scheduled: entry.scheduled_on.map(instant).unwrap_or_default(),
                document,
                superseded: successor.contains_key(&entry.id),
                chain: chain.join(" · "),
                deadline: false,
                state: String::new(),
                category: String::new(),
            };
            Dated { at, item }
        })
        .collect()
}

/// The open Deadlines this lens sees, as future cards.
fn deadline_rows(inputs: &DocketInputs<'_>) -> Vec<Dated> {
    let states: std::collections::HashMap<String, crate::project_deadlines::DeadlineRow> =
        crate::project_deadlines::deadline_rows(inputs.deadlines, inputs.now, inputs.client_lens)
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
    inputs
        .deadlines
        .iter()
        .filter_map(|deadline| {
            let row = states.get(&deadline.id.to_string())?;
            let at = DateTime::parse_from_rfc3339(&deadline.due_at)
                .ok()
                .map(|due| due.with_timezone(&Utc));
            let side = match deadline.calendar_role.as_str() {
                "firm" => Side::Ours,
                "opponent" => Side::Theirs,
                _ => Side::Court,
            };
            Some(Dated {
                at,
                item: TimelineItem {
                    key: row.id.clone(),
                    kind: "Deadline".to_string(),
                    title: row.authority.clone(),
                    filer: row.role.clone(),
                    side,
                    date: row.due.clone(),
                    deadline: true,
                    state: row.state.clone(),
                    category: crate::project_deadlines::category_label(
                        deadline.category.as_deref().unwrap_or_default(),
                    )
                    .to_string(),
                    ..TimelineItem::default()
                },
            })
        })
        .collect()
}

/// The rows still ahead of `now`: dated, after now, and not moved by a
/// later entry. Soonest first, since `rows` is in date order.
fn ahead(rows: &[Dated], now: DateTime<Utc>) -> impl Iterator<Item = &Dated> {
    rows.iter()
        .filter(move |row| !row.item.superseded && row.at.is_some_and(|at| at > now))
}

fn fact_strip(rows: &[Dated], now: DateTime<Utc>) -> FactStripView {
    let opened = rows
        .iter()
        .filter(|row| !row.item.deadline)
        .filter_map(|row| row.at)
        .min()
        .map(|at| at.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let on_file = rows
        .iter()
        .filter(|row| !row.item.deadline && row.item.document.is_some())
        .count();
    let next = ahead(rows, now)
        .next()
        .map(|row| format!("{} · {}", row.item.date, row.item.title))
        .unwrap_or_default();
    FactStripView {
        opened,
        on_file: u32::try_from(on_file).unwrap_or(u32::MAX),
        ahead: u32::try_from(ahead(rows, now).count()).unwrap_or(u32::MAX),
        next,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        binding, caption, deadline_board, docket, receipts, roster, side_of, Docket, DocketInputs,
        RosterInputs,
    };
    use crate::sections::timeline::Side;
    use store::cases::{Case, DocketEntry};
    use store::project_deadlines::Deadline;
    use uuid::Uuid;

    pub(crate) fn entry(number: &str, kind: &str, party: Option<&str>) -> DocketEntry {
        DocketEntry {
            id: Uuid::now_v7(),
            case_id: Uuid::nil(),
            entry_number: number.to_string(),
            kind: kind.to_string(),
            title: format!("Synthetic entry {number}"),
            party: party.map(str::to_string),
            filed_or_served_on: Some("2026-03-05".to_string()),
            scheduled_on: None,
            supersedes: None,
            document_asset_id: None,
            notation_id: None,
            stamp_case_number: None,
            stamp_entry_number: None,
            stamp_filed_on: None,
            integrity_checked_at: None,
            integrity_source_sha256_hex: None,
            integrity_matched: None,
            inserted_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn case(posture: &str) -> Case {
        Case {
            id: Uuid::now_v7(),
            project_id: Uuid::nil(),
            caption: "Alpha v. Beta".to_string(),
            forum: Some("Synthetic District Court".to_string()),
            jurisdiction: None,
            docket_number: Some("A-26-000001".to_string()),
            judge: None,
            posture: posture.to_string(),
            status: "open".to_string(),
            inserted_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn deadline(due_at: &str, role: &str, client_lead: Option<i64>) -> Deadline {
        Deadline {
            id: Uuid::now_v7(),
            project_id: Uuid::nil(),
            title: None,
            description: None,
            due_at: due_at.to_string(),
            authority_kind: "court_order".to_string(),
            authority: format!("Synthetic {role} order"),
            source: "lawyer".to_string(),
            source_document_id: None,
            owner_person_id: None,
            status: "open".to_string(),
            calendar_role: role.to_string(),
            category: Some("court".to_string()),
            internal_lead_days: Some(7),
            client_lead_days: client_lead,
            replay_key: None,
            inserted_at: String::new(),
            updated_at: String::new(),
            calendar_sequence: None,
        }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        "2026-06-01T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn the_side_follows_the_court_kinds_our_names_and_the_posture() {
        let ours = vec!["Alpha Example".to_string()];
        assert_eq!(
            side_of(&entry("1", "order", None), "plaintiff", &ours),
            Side::Court
        );
        assert_eq!(
            side_of(&entry("2", "stipulation", Some("Both")), "plaintiff", &ours),
            Side::Joint
        );
        assert_eq!(
            side_of(&entry("3", "motion", Some("Alpha Example")), "", &ours),
            Side::Ours
        );
        assert_eq!(
            side_of(
                &entry("4", "pleading", Some("Plaintiff A. Example")),
                "plaintiff",
                &[]
            ),
            Side::Ours
        );
        assert_eq!(
            side_of(
                &entry("5", "motion", Some("Defendant Beta Co.")),
                "plaintiff",
                &ours
            ),
            Side::Theirs
        );
        assert_eq!(
            side_of(&entry("6", "motion", None), "plaintiff", &ours),
            Side::Unattributed
        );
    }

    #[test]
    fn a_superseded_hearing_is_struck_and_chained_to_its_continuance() {
        let mut first = entry("31", "hearing", None);
        first.filed_or_served_on = None;
        first.scheduled_on = Some("2026-07-14T16:00:00Z".parse().unwrap());
        let mut moved = entry("34", "hearing", None);
        moved.filed_or_served_on = None;
        moved.scheduled_on = Some("2026-08-11T16:00:00Z".parse().unwrap());
        moved.supersedes = Some(first.id);
        let cases = [case("plaintiff")];
        let Docket { items, facts, .. } = docket(&DocketInputs {
            cases: &cases,
            entries: &[first, moved],
            deadlines: &[],
            assets: &[],
            our_names: &[],
            code: "m",
            client_lens: false,
            now: now(),
        });
        assert!(items[0].superseded);
        assert_eq!(
            items[0].chain,
            "Continued to 2026-08-11 16:00 UTC (D.I. 34)"
        );
        assert!(!items[1].superseded);
        assert_eq!(items[1].chain, "Continues D.I. 31");
        assert_eq!(items[1].side, Side::Court);
        // Only the current date is still ahead.
        assert_eq!(facts.ahead, 1);
        assert!(facts.next.starts_with("2026-08-11"), "{facts:?}");
    }

    #[test]
    fn the_client_lens_sees_client_visible_entries_and_client_dated_deadlines_only() {
        let cases = [case("plaintiff")];
        let entries = [entry("12", "motion", Some("Defendant Beta Co."))];
        let deadlines = [
            deadline("2026-11-02T17:00:00-08:00", "firm", None),
            deadline("2026-10-02T17:00:00-08:00", "hearing", Some(3)),
        ];
        let inputs = |client_lens| DocketInputs {
            cases: &cases,
            entries: &entries,
            deadlines: &deadlines,
            assets: &[],
            our_names: &[],
            code: "m",
            client_lens,
            now: now(),
        };
        let Docket {
            items: lawyer,
            facts,
            key_dates,
        } = docket(&inputs(false));
        assert_eq!(lawyer.len(), 3);
        assert_eq!(lawyer[0].number, "12");
        assert!(lawyer[1].deadline && lawyer[2].deadline);
        assert_eq!(facts.opened, "2026-03-05");
        assert_eq!(facts.on_file, 0);
        assert_eq!(facts.ahead, 2);
        assert_eq!(key_dates.len(), 2, "the filed motion is behind us");
        let client = docket(&inputs(true));
        assert_eq!(client.items.len(), 1, "{client:?}");
        assert_eq!(client.items[0].title, "Synthetic hearing order");
        assert_eq!(client.key_dates.len(), 1);
        assert_eq!(client.key_dates[0].detail, "Synthetic hearing order");
    }

    #[test]
    fn the_key_dates_strip_names_a_deadlines_category() {
        let cases = [case("plaintiff")];
        let deadlines = [deadline("2026-11-02T17:00:00-08:00", "firm", None), {
            let mut uncategorized = deadline("2026-12-02T17:00:00-08:00", "firm", None);
            uncategorized.category = None;
            uncategorized
        }];
        let Docket { key_dates, .. } = docket(&DocketInputs {
            cases: &cases,
            entries: &[],
            deadlines: &deadlines,
            assets: &[],
            our_names: &[],
            code: "m",
            client_lens: false,
            now: now(),
        });
        assert_eq!(key_dates.len(), 2);
        assert_eq!(key_dates[0].status, "Court · Upcoming");
        assert_eq!(key_dates[1].status, "Upcoming");
    }

    #[test]
    fn the_caption_reads_the_case_row_or_falls_back_to_the_project() {
        let view = caption(&[case("plaintiff")], None, "Sample matter");
        assert_eq!(view.caption, "Alpha v. Beta");
        assert_eq!(view.case_number, "A-26-000001");
        assert_eq!(view.posture, "Plaintiff");
        let fallback = caption(&[], None, "Sample matter");
        assert_eq!(fallback.caption, "Sample matter");
        assert!(fallback.case_number.is_empty());
    }

    pub(crate) fn synced_matter() -> word::matter::Matter {
        word::matter::Matter {
            court: vec!["SYNTHETIC SUPERIOR COURT".into(), "COUNTY OF SAMPLE".into()],
            case_number: "00CV000777".into(),
            judicial_officer: "Hon. Sample Jurist".into(),
            department: "12".into(),
            filed_on: "2026-02-02".into(),
            trial_on: None,
            parties: vec![
                word::matter::Party {
                    name: "GAMMA LLC".into(),
                    role: "Plaintiff".into(),
                },
                word::matter::Party {
                    name: "DELTA INC".into(),
                    role: "Defendant".into(),
                },
            ],
            counsel: word::matter::Counsel {
                name: "Pat Sample".into(),
                appears_for: "Plaintiff GAMMA LLC".into(),
                ..word::matter::Counsel::default()
            },
        }
    }

    #[test]
    fn the_synced_matter_wins_over_the_case_row_field_by_field() {
        let matter = synced_matter();
        let view = caption(&[case("plaintiff")], Some(&matter), "Sample matter");
        assert_eq!(view.caption, "GAMMA LLC v. DELTA INC");
        assert_eq!(view.court, "SYNTHETIC SUPERIOR COURT · COUNTY OF SAMPLE");
        assert_eq!(view.case_number, "00CV000777");
        assert_eq!(view.judge, "Hon. Sample Jurist");
        assert_eq!(view.department, "12");
        assert_eq!(view.filed_on, "2026-02-02");
        assert!(view.trial_on.is_empty(), "no trial date is set");
        assert_eq!(
            view.posture, "Plaintiff",
            "the posture stays the case row's"
        );

        let partial = word::matter::Matter {
            judicial_officer: "Hon. Other Jurist".into(),
            ..word::matter::Matter::default()
        };
        let view = caption(&[case("plaintiff")], Some(&partial), "Sample matter");
        assert_eq!(
            view.caption, "Alpha v. Beta",
            "an unnamed party leaves the title"
        );
        assert_eq!(view.case_number, "A-26-000001");
        assert_eq!(view.judge, "Hon. Other Jurist");

        let no_case = caption(&[], Some(&matter), "Sample matter");
        assert_eq!(no_case.caption, "GAMMA LLC v. DELTA INC");
    }

    #[test]
    fn the_roster_lists_the_synced_parties_and_counsel_first() {
        let matter = synced_matter();
        let items = vec![crate::sections::TimelineItem {
            filer: "DELTA INC".to_string(),
            side: Side::Theirs,
            ..Default::default()
        }];
        let team = vec![
            ("Pat Sample".to_string(), "lawyer".to_string()),
            ("Sample Client".to_string(), "client".to_string()),
        ];
        let view = roster(&RosterInputs {
            matter: Some(&matter),
            client_entity: "GAMMA LLC",
            team: &team,
            items: &items,
            services: &[],
            references: &[],
            client_lens: true,
        });
        let rows = |heading: &str| -> Vec<(String, String)> {
            view.groups
                .iter()
                .find(|g| g.heading == heading)
                .map(|g| {
                    g.rows
                        .iter()
                        .map(|r| (r.label.clone(), r.detail.clone()))
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(
            rows("Parties"),
            [
                ("GAMMA LLC".to_string(), "Plaintiff".to_string()),
                ("DELTA INC".to_string(), "Defendant".to_string()),
            ]
        );
        assert_eq!(
            rows("Counsel and team"),
            [
                (
                    "Pat Sample".to_string(),
                    "Counsel of record for Plaintiff GAMMA LLC".to_string()
                ),
                ("Sample Client".to_string(), "Client".to_string()),
            ]
        );
    }

    #[test]
    fn the_deadline_board_marks_binding_authority_hard_and_internal_targets_soft() {
        let mut internal = deadline("2026-07-01T17:00:00-07:00", "firm", None);
        internal.authority_kind = "internal".to_string();
        let ordered = deadline("2026-08-01T17:00:00-07:00", "firm", Some(2));
        let board = deadline_board(&[internal, ordered], now(), false);
        let cells: Vec<&str> = board.iter().map(|row| row.binding.as_str()).collect();
        assert_eq!(cells, ["Soft", "Hard"]);
        let client = deadline_board(
            &[deadline("2026-08-01T17:00:00-07:00", "firm", None)],
            now(),
            true,
        );
        assert!(client.is_empty(), "no client lead time, no client row");
        assert_eq!(binding("statute"), "Hard");
        assert_eq!(binding(""), "Soft");
    }

    #[test]
    fn the_roster_keeps_the_conflicts_record_off_the_client_lens() {
        let items = vec![crate::sections::TimelineItem {
            filer: "Defendant Beta Co.".to_string(),
            side: Side::Theirs,
            ..Default::default()
        }];
        let team = vec![
            ("Sample Counsel".to_string(), "lawyer".to_string()),
            ("Sample Clerk".to_string(), "clerk".to_string()),
        ];
        let references = vec![(
            "Synthetic Agent".to_string(),
            "registered_agent".to_string(),
        )];
        let inputs = |client_lens| RosterInputs {
            matter: None,
            client_entity: "Alpha Example",
            team: &team,
            items: &items,
            services: &[],
            references: &references,
            client_lens,
        };
        let lawyer = roster(&inputs(false));
        let headings: Vec<&str> = lawyer.groups.iter().map(|g| g.heading.as_str()).collect();
        assert_eq!(
            headings,
            [
                "Parties",
                "Counsel and team",
                "Referenced, no standing on the matter"
            ]
        );
        assert_eq!(lawyer.groups[0].rows[1].label, "Defendant Beta Co.");
        let client = roster(&inputs(true));
        assert_eq!(client.groups.len(), 2);
        assert!(!format!("{client:?}").contains("Synthetic Agent"));
        assert!(!format!("{client:?}").contains("Sample Clerk"));
        let empty = roster(&RosterInputs {
            matter: None,
            client_entity: "",
            team: &[],
            items: &[],
            services: &[],
            references: &[],
            client_lens: false,
        });
        assert!(empty.groups.is_empty());
    }

    #[test]
    fn receipts_name_the_submitted_document_and_hide_notes_from_a_client() {
        let tx = store::efiling_transactions::EfilingTransaction {
            id: Uuid::now_v7(),
            project_id: Uuid::nil(),
            transaction_id: "TX-0001".to_string(),
            efsp: "synthetic".to_string(),
            court: "Synthetic Court".to_string(),
            case_number: "A-26-000001".to_string(),
            submitted_document_id: None,
            status: "unknown".to_string(),
            filed_on: None,
            accepted_on: None,
            conformed_document_id: None,
            needs_entered_order: false,
            note: String::new(),
            inserted_at: "2026-03-04T10:00:00Z".to_string(),
            updated_at: String::new(),
        };
        let lawyer = receipts(std::slice::from_ref(&tx), &[], false);
        assert_eq!(lawyer[0].submitted, "2026-03-04");
        assert_eq!(lawyer[0].title, "Synthetic Court · A-26-000001");
        assert_eq!(lawyer[0].status, "Unknown");
        assert_eq!(lawyer[0].note, store::efiling_transactions::UNKNOWN_NOTE);
        assert!(
            receipts(&[tx], &[], true).is_empty(),
            "no client-visible document"
        );
    }
}
