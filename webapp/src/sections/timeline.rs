//! The litigation board's case sections: `case_caption`, `fact_strip`, and
//! `docket_timeline`.
//!
//! Every value here is display-ready and wasm-safe; the server half that
//! builds them from Docket Entries, Documents, and Deadlines is
//! [`super::derive`]. The timeline's side filter is a plain link per side
//! (`?side=ours`), so it works without JavaScript.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::PageLens;
use crate::components::{
    Badge, BadgeTone, Empty, FactCard, FactGrid, Panel, ToggleGroup, ToggleOption,
};

/// The case masthead. An empty `caption` is the empty state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptionView {
    pub caption: String,
    pub court: String,
    pub case_number: String,
    pub judge: String,
    pub department: String,
    /// When the action was filed, `YYYY-MM-DD`.
    pub filed_on: String,
    /// The trial date the court set, `YYYY-MM-DD`.
    pub trial_on: String,
    /// Which side the client is on, as recorded on the case.
    pub posture: String,
    pub status: String,
}

/// The case in four figures. All-empty is the empty state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactStripView {
    /// The earliest dated docket entry, `YYYY-MM-DD`.
    pub opened: String,
    /// Entries this reader sees with a document on file.
    pub on_file: u32,
    /// Dates this reader sees that are still ahead.
    pub ahead: u32,
    /// The soonest of those, as `date · title`.
    pub next: String,
}

impl FactStripView {
    fn is_empty(&self) -> bool {
        self.opened.is_empty() && self.on_file == 0 && self.ahead == 0 && self.next.is_empty()
    }
}

/// Whose act a timeline item is, derived from the filer and the matter's
/// participation, never typed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// The client, or the firm acting for the client.
    Ours,
    /// Anyone else who files.
    #[default]
    Theirs,
    /// An order, a hearing, or a trial: the court's act.
    Court,
    /// A stipulation: both sides at once.
    Joint,
    /// An entry recorded with no filer: shown, never guessed.
    Unattributed,
}

impl Side {
    /// The side in plain words.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ours => "Our side",
            Self::Theirs => "Other side",
            Self::Court => "Court",
            Self::Joint => "Both sides",
            Self::Unattributed => "Filer not recorded",
        }
    }

    /// The `data-side` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ours => "ours",
            Self::Theirs => "theirs",
            Self::Court => "court",
            Self::Joint => "joint",
            Self::Unattributed => "unattributed",
        }
    }
}

/// The timeline's side filter, read from `?side=`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SideFilter {
    /// Every item.
    #[default]
    Both,
    Ours,
    Theirs,
    Court,
}

impl SideFilter {
    /// Every filter, in the order the toggle lists them.
    pub const ALL: [SideFilter; 4] = [Self::Both, Self::Ours, Self::Theirs, Self::Court];

    /// Parse a `side` query value; anything unknown is [`SideFilter::Both`].
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "ours" => Self::Ours,
            "theirs" => Self::Theirs,
            "court" => Self::Court,
            _ => Self::Both,
        }
    }

    /// The filter's `side` query value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Ours => "ours",
            Self::Theirs => "theirs",
            Self::Court => "court",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Both => "Both",
            Self::Ours => "Our side",
            Self::Theirs => "Other side",
            Self::Court => "Court",
        }
    }

    /// Whether an item on `side` shows under this filter. A stipulation is
    /// both sides' act, so it shows under either.
    #[must_use]
    pub fn admits(self, side: Side) -> bool {
        match self {
            Self::Both => true,
            Self::Ours => matches!(side, Side::Ours | Side::Joint),
            Self::Theirs => matches!(side, Side::Theirs | Side::Joint),
            Self::Court => side == Side::Court,
        }
    }
}

/// A Document a timeline item links to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentLink {
    pub label: String,
    pub href: String,
}

/// One card on the timeline: a Docket Entry or an open Deadline.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineItem {
    /// The entry or deadline id.
    pub key: String,
    /// The court's entry number; empty for a deadline.
    pub number: String,
    /// The kind badge: `Motion`, `Hearing`, `Deadline`.
    pub kind: String,
    pub title: String,
    /// Who filed it, or whose date it is.
    pub filer: String,
    pub side: Side,
    /// The display date: the date the court set, or the date filed.
    pub date: String,
    /// A hearing or trial's `scheduled_on`.
    pub scheduled: String,
    /// The Document on file; `None` on an entry is *source pending*.
    pub document: Option<DocumentLink>,
    /// A later entry moved this appearance; the card is struck through.
    pub superseded: bool,
    /// `Continued to … (D.I. n)` on a superseded appearance, or
    /// `Continues D.I. n` on the entry that moved it.
    pub chain: String,
    /// An open Deadline: drawn as a dashed future card.
    pub deadline: bool,
    /// A deadline's state chip: `Overdue`, `Due soon`, `Upcoming`.
    pub state: String,
    /// A deadline's category, in plain words; empty on a docket entry.
    pub category: String,
}

impl TimelineItem {
    fn source_pending(&self) -> bool {
        !self.deadline && self.document.is_none()
    }
}

/// The `docket_timeline` section's data, already scoped to the lens.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineView {
    pub lens: Option<PageLens>,
    pub filter: SideFilter,
    /// The page's own path, which each filter link extends.
    pub page_href: String,
    /// Every item this lens sees, in date order.
    pub items: Vec<TimelineItem>,
}

/// The `case_caption` section.
#[component]
pub fn CaseCaptionSection(view: CaptionView) -> Element {
    rsx! {
        section {
            class: "matter-section matter-caption",
            "data-section": "case_caption",
            aria_label: "Case caption",
            if view.caption.is_empty() {
                div { class: "matter-section__empty",
                    Empty { title: "No case is recorded on this matter yet.".to_string() }
                }
            } else {
                Panel {
                    title: view.caption.clone(),
                    note: (!view.court.is_empty()).then(|| view.court.clone()),
                    dl { class: "matter-caption__facts",
                        if !view.case_number.is_empty() {
                            dt { "Case number" }
                            dd { "{view.case_number}" }
                        }
                        if !view.judge.is_empty() {
                            dt { "Judge" }
                            dd { "{view.judge}" }
                        }
                        if !view.department.is_empty() {
                            dt { "Department" }
                            dd { "{view.department}" }
                        }
                        if !view.filed_on.is_empty() {
                            dt { "Action filed" }
                            dd { "{view.filed_on}" }
                        }
                        if !view.trial_on.is_empty() {
                            dt { "Trial date" }
                            dd { "{view.trial_on}" }
                        }
                        if !view.posture.is_empty() {
                            dt { "Our client" }
                            dd { "{view.posture}" }
                        }
                        if !view.status.is_empty() {
                            dt { "Status" }
                            dd { "{view.status}" }
                        }
                    }
                }
            }
        }
    }
}

/// The `fact_strip` section.
#[component]
pub fn FactStripSection(view: FactStripView) -> Element {
    let opened = if view.opened.is_empty() {
        "Not yet".to_string()
    } else {
        view.opened.clone()
    };
    let next = if view.next.is_empty() {
        "Nothing scheduled".to_string()
    } else {
        view.next.clone()
    };
    rsx! {
        section {
            class: "matter-section",
            "data-section": "fact_strip",
            aria_label: "The case at a glance",
            if view.is_empty() {
                div { class: "matter-section__empty",
                    Empty { title: "Nothing is on the docket yet.".to_string() }
                }
            } else {
                FactGrid {
                    FactCard { title: "Case opened".to_string(), p { "{opened}" } }
                    FactCard { title: "On file".to_string(), p { "{view.on_file}" } }
                    FactCard { title: "Still ahead".to_string(), p { "{view.ahead}" } }
                    FactCard { title: "Next".to_string(), p { "{next}" } }
                }
            }
        }
    }
}

/// The `docket_timeline` section.
#[component]
pub fn DocketTimelineSection(view: TimelineView) -> Element {
    let lawyer = view.lens == Some(PageLens::Lawyer);
    let options: Vec<ToggleOption> = SideFilter::ALL
        .iter()
        .map(|filter| {
            let href = match filter {
                SideFilter::Both => view.page_href.clone(),
                other => format!("{}?side={}", view.page_href, other.as_str()),
            };
            ToggleOption::new(filter.label(), href, *filter == view.filter)
        })
        .collect();
    let shown: Vec<TimelineItem> = view
        .items
        .iter()
        .filter(|item| view.filter.admits(item.side))
        .cloned()
        .collect();
    let empty_title = if view.items.is_empty() {
        "Nothing is on the docket yet."
    } else {
        "Nothing on the docket matches this side."
    };
    rsx! {
        section {
            class: "matter-section",
            "data-section": "docket_timeline",
            "data-filter": view.filter.as_str(),
            aria_labelledby: "section-docket_timeline-title",
            Panel {
                id: "section-docket_timeline".to_string(),
                title: "Case timeline".to_string(),
                note: "Every date is the court's or the stored deadline's, never retyped.".to_string(),
                actions: rsx! { ToggleGroup { label: "Filter by side".to_string(), options } },
                if shown.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: empty_title.to_string() }
                    }
                } else {
                    ol { class: "matter-timeline",
                        for item in shown.iter() {
                            TimelineCard { key: "{item.key}", item: item.clone(), lawyer }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn TimelineCard(item: TimelineItem, lawyer: bool) -> Element {
    let mut class = String::from("matter-timeline__item");
    if item.deadline {
        class.push_str(" matter-timeline__item--ahead");
    }
    if item.superseded {
        class.push_str(" matter-timeline__item--superseded");
    }
    let tone = if item.deadline {
        BadgeTone::Next
    } else if item.superseded {
        BadgeTone::Default
    } else if item.source_pending() {
        BadgeTone::Review
    } else {
        BadgeTone::Source
    };
    let heading = if item.number.is_empty() {
        item.title.clone()
    } else {
        format!("D.I. {} · {}", item.number, item.title)
    };
    let pending = item.source_pending();
    rsx! {
        li { class: "{class}", "data-side": item.side.as_str(),
            article { class: "matter-timeline__card",
                header { class: "matter-timeline__head",
                    Badge { tone, "{item.kind}" }
                    span { class: "matter-timeline__side", "{item.side.label()}" }
                    if !item.date.is_empty() {
                        time { class: "matter-timeline__date", "{item.date}" }
                    }
                }
                if item.superseded {
                    h3 { class: "matter-timeline__title", del { "{heading}" } }
                } else {
                    h3 { class: "matter-timeline__title", "{heading}" }
                }
                if !item.filer.is_empty() {
                    p { class: "matter-timeline__filer", "{item.filer}" }
                }
                if !item.scheduled.is_empty() {
                    p { class: "matter-timeline__scheduled",
                        if item.superseded {
                            "Was set for "
                            del { "{item.scheduled}" }
                        } else {
                            "Set for {item.scheduled}"
                        }
                    }
                }
                if !item.chain.is_empty() {
                    p { class: "matter-timeline__chain", "{item.chain}" }
                }
                if !item.state.is_empty() {
                    p { Badge { tone: BadgeTone::Next, "{item.state}" } }
                }
                if let Some(document) = &item.document {
                    p { class: "matter-timeline__document",
                        a { class: "nav-link", href: "{document.href}", "{document.label}" }
                    }
                } else if pending {
                    div { class: "matter-timeline__pending",
                        Badge { tone: BadgeTone::Review, "Source pending" }
                        if lawyer {
                            details { class: "matter-timeline__link-document",
                                summary { "Link document" }
                                p { "File the document under documents/, then link it to this entry:" }
                                pre {
                                    code {
                                        "navigator project docket link-document {item.key} --slug <path under documents/>"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CaptionView, DocumentLink, FactStripView, Side, SideFilter, TimelineItem, TimelineView,
    };
    use crate::sections::tests::render;
    use crate::sections::{PageLens, SectionView};

    fn filed(number: &str, side: Side) -> TimelineItem {
        TimelineItem {
            key: format!("entry-{number}"),
            number: number.to_string(),
            kind: "Motion".to_string(),
            title: format!("Synthetic motion {number}"),
            filer: "Defendant Beta Co.".to_string(),
            side,
            date: "2026-03-05".to_string(),
            document: Some(DocumentLink {
                label: format!("di-{number}.pdf"),
                href: format!("/app/projects/m/documents/{number}"),
            }),
            ..TimelineItem::default()
        }
    }

    fn timeline(lens: PageLens, filter: SideFilter, items: Vec<TimelineItem>) -> String {
        render(SectionView::DocketTimeline(TimelineView {
            lens: Some(lens),
            filter,
            page_href: "/app/projects/m/timeline".to_string(),
            items,
        }))
    }

    #[test]
    fn an_empty_matter_says_nothing_is_on_the_docket() {
        let html = timeline(PageLens::Lawyer, SideFilter::Both, Vec::new());
        assert!(html.contains("matter-section__empty"), "{html}");
        assert!(html.contains("Nothing is on the docket yet."), "{html}");
    }

    #[test]
    fn a_filed_entry_shows_number_kind_title_filer_date_and_its_document() {
        let html = timeline(
            PageLens::Lawyer,
            SideFilter::Both,
            vec![filed("12", Side::Theirs)],
        );
        assert!(html.contains("D.I. 12 · Synthetic motion 12"), "{html}");
        assert!(html.contains(">Motion<"), "{html}");
        assert!(html.contains("Defendant Beta Co."), "{html}");
        assert!(html.contains("2026-03-05"), "{html}");
        assert!(
            html.contains("href=\"/app/projects/m/documents/12\""),
            "{html}"
        );
        assert!(!html.contains("Source pending"), "{html}");
    }

    #[test]
    fn a_source_pending_entry_offers_the_link_action_on_the_lawyer_lens_only() {
        let mut pending = filed("13", Side::Ours);
        pending.document = None;
        let lawyer = timeline(PageLens::Lawyer, SideFilter::Both, vec![pending.clone()]);
        assert!(lawyer.contains("Source pending"), "{lawyer}");
        assert!(lawyer.contains("Link document"), "{lawyer}");
        assert!(
            lawyer.contains("navigator project docket link-document entry-13"),
            "{lawyer}"
        );
        let client = timeline(PageLens::Client, SideFilter::Both, vec![pending]);
        assert!(client.contains("Source pending"), "{client}");
        assert!(!client.contains("Link document"), "{client}");
    }

    #[test]
    fn a_superseded_hearing_is_struck_through_beside_its_continuance() {
        let first = TimelineItem {
            key: "h31".to_string(),
            number: "31".to_string(),
            kind: "Hearing".to_string(),
            title: "Hearing on synthetic motion".to_string(),
            side: Side::Court,
            date: "2026-07-14 16:00 UTC".to_string(),
            scheduled: "2026-07-14 16:00 UTC".to_string(),
            superseded: true,
            chain: "Continued to 2026-08-11 16:00 UTC (D.I. 34)".to_string(),
            ..TimelineItem::default()
        };
        let moved = TimelineItem {
            key: "h34".to_string(),
            number: "34".to_string(),
            superseded: false,
            date: "2026-08-11 16:00 UTC".to_string(),
            scheduled: "2026-08-11 16:00 UTC".to_string(),
            chain: "Continues D.I. 31".to_string(),
            ..first.clone()
        };
        let html = timeline(PageLens::Lawyer, SideFilter::Both, vec![first, moved]);
        assert!(html.contains("matter-timeline__item--superseded"), "{html}");
        assert!(
            html.contains("<del>D.I. 31 · Hearing on synthetic motion</del>"),
            "{html}"
        );
        assert!(html.contains("<del>2026-07-14 16:00 UTC</del>"), "{html}");
        assert!(html.contains("Set for 2026-08-11 16:00 UTC"), "{html}");
        assert!(html.contains("Continues D.I. 31"), "{html}");
    }

    #[test]
    fn a_deadline_ahead_is_a_dashed_future_card() {
        let ahead = TimelineItem {
            key: "d1".to_string(),
            kind: "Deadline".to_string(),
            title: "Synthetic briefing order".to_string(),
            filer: "Our deadline".to_string(),
            side: Side::Ours,
            date: "2026-11-02 17:00 -08:00".to_string(),
            deadline: true,
            state: "Upcoming".to_string(),
            ..TimelineItem::default()
        };
        let html = timeline(PageLens::Client, SideFilter::Both, vec![ahead]);
        assert!(html.contains("matter-timeline__item--ahead"), "{html}");
        assert!(html.contains("Upcoming"), "{html}");
        assert!(!html.contains("Source pending"), "{html}");
    }

    #[test]
    fn the_side_filter_is_links_and_shows_only_the_chosen_side() {
        let items = vec![
            filed("1", Side::Ours),
            filed("2", Side::Theirs),
            filed("3", Side::Joint),
            filed("4", Side::Court),
            filed("5", Side::Unattributed),
        ];
        let html = timeline(PageLens::Lawyer, SideFilter::Ours, items.clone());
        assert!(html.contains("data-filter=\"ours\""), "{html}");
        assert!(
            html.contains("href=\"/app/projects/m/timeline?side=theirs\""),
            "{html}"
        );
        assert!(html.contains("href=\"/app/projects/m/timeline\""), "{html}");
        assert!(html.contains("aria-current=\"true\""), "{html}");
        assert!(html.contains("Synthetic motion 1"), "{html}");
        assert!(
            html.contains("Synthetic motion 3"),
            "a stipulation is ours too"
        );
        assert!(!html.contains("Synthetic motion 2"), "{html}");
        assert!(!html.contains("Synthetic motion 4"), "{html}");
        assert!(!html.contains("Synthetic motion 5"), "{html}");
        let court = timeline(PageLens::Lawyer, SideFilter::Court, items.clone());
        assert!(court.contains("Synthetic motion 4"), "{court}");
        assert!(!court.contains("Synthetic motion 1"), "{court}");
        let both = timeline(PageLens::Lawyer, SideFilter::Both, items);
        assert!(both.contains("Synthetic motion 5"), "{both}");
        let none = timeline(
            PageLens::Lawyer,
            SideFilter::Court,
            vec![filed("6", Side::Ours)],
        );
        assert!(
            none.contains("Nothing on the docket matches this side."),
            "{none}"
        );
        assert_eq!(SideFilter::parse("theirs"), SideFilter::Theirs);
        assert_eq!(SideFilter::parse("<script>"), SideFilter::Both);
    }

    #[test]
    fn the_caption_and_fact_strip_render_their_facts() {
        let html = render(SectionView::CaseCaption(CaptionView {
            caption: "Alpha v. Beta".to_string(),
            court: "Synthetic District Court".to_string(),
            case_number: "A-26-000001".to_string(),
            department: "12".to_string(),
            trial_on: "2027-01-11".to_string(),
            ..CaptionView::default()
        }));
        assert!(html.contains("Alpha v. Beta"), "{html}");
        assert!(html.contains("A-26-000001"), "{html}");
        assert!(html.contains("Department"), "{html}");
        assert!(html.contains("2027-01-11"), "{html}");
        assert!(
            !html.contains("Action filed"),
            "an empty fact is not printed: {html}"
        );
        assert!(!html.contains("matter-section__empty"), "{html}");
        let facts = render(SectionView::FactStrip(FactStripView {
            opened: "2026-03-05".to_string(),
            on_file: 2,
            ahead: 1,
            next: "2026-11-02 · Synthetic order".to_string(),
        }));
        for text in [
            "Case opened",
            "On file",
            "Still ahead",
            "Next",
            "2026-11-02 · Synthetic order",
        ] {
            assert!(facts.contains(text), "{text}: {facts}");
        }
    }
}
