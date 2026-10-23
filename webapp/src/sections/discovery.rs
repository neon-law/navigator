//! The `discovery_pairs` section: each numbered item of a served
//! [Discovery Request](../../../docs/glossary/discovery-request.md), as
//! the request, the objection attributed to counsel, the answer attributed
//! to the responding party, and where that leaves us.
//!
//! Every value here is display-ready and wasm-safe. The server half
//! ([`derive`]) builds the pairs from `discovery_request` and
//! `discovery_item`, already scoped to the lens. Status and sufficiency are
//! derived from what is recorded (whether a response and objections exist,
//! and whether the set has been answered), never typed. The firm's "where
//! it leaves us" commentary is filled on the lawyer lens only, so it cannot
//! reach a client page. The filter is a plain link per view
//! (`?discovery=needs-you`), so it works without JavaScript.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::PageLens;
use crate::components::{Badge, BadgeTone, Empty, Panel, ToggleGroup, ToggleOption};

/// Where one item stands, derived from the record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoveryStatus {
    /// We respond, and no answer is drafted yet.
    #[default]
    NeedsResponse,
    /// They respond, and have not answered yet.
    AwaitingResponse,
    /// We respond, an answer is drafted, and the set is not yet answered:
    /// it waits on a signature.
    ReadyToSign,
    /// The set has been answered.
    Responded,
    /// The set is closed.
    Closed,
}

impl DiscoveryStatus {
    /// The status in plain words.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsResponse => "Needs a response",
            Self::AwaitingResponse => "Awaiting their response",
            Self::ReadyToSign => "Ready to sign",
            Self::Responded => "Responded",
            Self::Closed => "Closed",
        }
    }

    /// The `data-status` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsResponse => "needs-response",
            Self::AwaitingResponse => "awaiting-response",
            Self::ReadyToSign => "ready-to-sign",
            Self::Responded => "responded",
            Self::Closed => "closed",
        }
    }

    fn tone(self) -> BadgeTone {
        match self {
            Self::NeedsResponse => BadgeTone::Review,
            Self::AwaitingResponse => BadgeTone::Next,
            Self::ReadyToSign => BadgeTone::Active,
            Self::Responded | Self::Closed => BadgeTone::Ready,
        }
    }
}

/// What the record holds for one item: the sufficiency badge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sufficiency {
    /// Neither an answer nor an objection.
    #[default]
    NoResponse,
    /// An objection with no answer.
    ObjectionOnly,
    /// An answer given subject to an objection.
    AnsweredSubjectToObjection,
    /// An answer and no objection.
    Answered,
}

impl Sufficiency {
    /// The badge text.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NoResponse => "No response",
            Self::ObjectionOnly => "Objection only",
            Self::AnsweredSubjectToObjection => "Answered subject to objection",
            Self::Answered => "Answered",
        }
    }

    fn tone(self) -> BadgeTone {
        match self {
            Self::NoResponse => BadgeTone::Default,
            Self::ObjectionOnly => BadgeTone::Blocked,
            Self::AnsweredSubjectToObjection => BadgeTone::Review,
            Self::Answered => BadgeTone::Ready,
        }
    }
}

/// The section's filter, read from `?discovery=`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoveryFilter {
    /// Every item.
    #[default]
    All,
    /// Items we owe an answer to.
    NeedsYou,
    /// Items carrying an objection.
    Objecting,
    /// Drafted answers waiting on a signature.
    ReadyToSign,
}

impl DiscoveryFilter {
    /// Every filter, in the order the toggle lists them.
    pub const ALL: [DiscoveryFilter; 4] = [
        Self::All,
        Self::NeedsYou,
        Self::Objecting,
        Self::ReadyToSign,
    ];

    /// Parse a `discovery` query value; anything unknown is
    /// [`DiscoveryFilter::All`].
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "needs-you" => Self::NeedsYou,
            "objecting" => Self::Objecting,
            "ready-to-sign" => Self::ReadyToSign,
            _ => Self::All,
        }
    }

    /// The filter's `discovery` query value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::NeedsYou => "needs-you",
            Self::Objecting => "objecting",
            Self::ReadyToSign => "ready-to-sign",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::NeedsYou => "Needs you",
            Self::Objecting => "Objecting",
            Self::ReadyToSign => "Ready to sign",
        }
    }

    /// Whether `pair` shows under this filter.
    #[must_use]
    pub fn admits(self, pair: &DiscoveryPair) -> bool {
        match self {
            Self::All => true,
            Self::NeedsYou => pair.status == DiscoveryStatus::NeedsResponse,
            Self::Objecting => pair.objection.is_some(),
            Self::ReadyToSign => pair.status == DiscoveryStatus::ReadyToSign,
        }
    }
}

/// The `discovery` value of a page's query string, e.g.
/// `discovery=objecting`.
#[must_use]
pub fn discovery_filter(query: &str) -> DiscoveryFilter {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == "discovery")
        .map_or_else(DiscoveryFilter::default, |(_, value)| {
            DiscoveryFilter::parse(value)
        })
}

/// One numbered item and what has been said to it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryPair {
    /// The item id.
    pub key: String,
    /// `Interrogatories · Set 1 · No. 3`.
    pub kicker: String,
    /// The propounded request, verbatim.
    pub request: String,
    pub objection: Option<String>,
    /// Who asserted the objection: counsel for the responding party.
    pub objection_by: String,
    pub answer: Option<String>,
    /// Who gives the answer: the responding party, under oath for an
    /// interrogatory.
    pub answer_by: String,
    /// `Answer` for an interrogatory, `Response` otherwise.
    pub answer_heading: String,
    pub status: DiscoveryStatus,
    pub sufficiency: Sufficiency,
    /// The firm's "where it leaves us". Always empty off the lawyer lens.
    pub commentary: String,
}

/// The `discovery_pairs` section's data, already scoped to the lens.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryView {
    pub lens: Option<PageLens>,
    pub filter: DiscoveryFilter,
    /// The page's own path, which each filter link extends.
    pub page_href: String,
    /// Every item this lens sees, set by set in item order.
    pub pairs: Vec<DiscoveryPair>,
}

impl DiscoveryView {
    /// Items with an answer, and every item: the progress counter.
    #[must_use]
    pub fn progress(&self) -> super::CounterView {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        super::CounterView {
            done: count(self.pairs.iter().filter(|p| p.answer.is_some()).count()),
            total: count(self.pairs.len()),
        }
    }

    /// One read-only row per item: the item status, for the lawyer lens.
    /// No web write path sets a discovery item's status; it follows the
    /// recorded answer and objections.
    #[must_use]
    pub fn status_rows(&self) -> Vec<super::SectionRow> {
        self.pairs
            .iter()
            .map(|pair| super::SectionRow {
                label: pair.kicker.clone(),
                detail: "Read-only: follows the recorded answer and objections".to_string(),
                status: pair.status.label().to_string(),
            })
            .collect()
    }
}

/// The `discovery_pairs` section.
#[component]
pub fn DiscoveryPairsSection(view: DiscoveryView) -> Element {
    let options: Vec<ToggleOption> = DiscoveryFilter::ALL
        .iter()
        .map(|filter| {
            let href = match filter {
                DiscoveryFilter::All => view.page_href.clone(),
                other => format!("{}?discovery={}", view.page_href, other.as_str()),
            };
            let count = view.pairs.iter().filter(|p| filter.admits(p)).count();
            ToggleOption::new(
                format!("{} ({count})", filter.label()),
                href,
                *filter == view.filter,
            )
        })
        .collect();
    let shown: Vec<DiscoveryPair> = view
        .pairs
        .iter()
        .filter(|pair| view.filter.admits(pair))
        .cloned()
        .collect();
    let empty_title = if view.pairs.is_empty() {
        "No discovery requests are recorded yet."
    } else {
        "No discovery request matches this filter."
    };
    rsx! {
        section {
            class: "matter-section",
            "data-section": "discovery_pairs",
            "data-filter": view.filter.as_str(),
            aria_labelledby: "section-discovery_pairs-title",
            Panel {
                id: "section-discovery_pairs".to_string(),
                title: "Discovery".to_string(),
                note: "Each request with the objection counsel asserted and the answer the responding party gave.".to_string(),
                actions: rsx! { ToggleGroup { label: "Filter discovery".to_string(), options } },
                if shown.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: empty_title.to_string() }
                    }
                } else {
                    ol { class: "matter-discovery",
                        for pair in shown.iter() {
                            DiscoveryCard { key: "{pair.key}", pair: pair.clone() }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn DiscoveryCard(pair: DiscoveryPair) -> Element {
    rsx! {
        li { class: "matter-discovery__item", "data-status": pair.status.as_str(),
            article { class: "matter-discovery__card",
                div { class: "matter-discovery__head",
                    p { class: "matter-discovery__kicker", "{pair.kicker}" }
                    Badge { tone: pair.status.tone(), "{pair.status.label()}" }
                    Badge { tone: pair.sufficiency.tone(), "{pair.sufficiency.label()}" }
                }
                blockquote { class: "matter-discovery__request", "{pair.request}" }
                if let Some(objection) = pair.objection.as_ref() {
                    div { class: "matter-discovery__box matter-discovery__box--objection",
                        p { class: "matter-discovery__label", strong { "Objection" } " · {pair.objection_by}" }
                        p { "{objection}" }
                    }
                }
                if let Some(answer) = pair.answer.as_ref() {
                    div { class: "matter-discovery__box matter-discovery__box--answer",
                        p { class: "matter-discovery__label", strong { "{pair.answer_heading}" } " · {pair.answer_by}" }
                        p { "{answer}" }
                    }
                }
                if !pair.commentary.is_empty() {
                    aside { class: "matter-discovery__commentary", "data-firm-only": "true",
                        p { class: "matter-discovery__label", strong { "Where it leaves us" } " · firm only" }
                        p { "{pair.commentary}" }
                    }
                }
            }
        }
    }
}

/// The server half: pairs from `discovery_request` and `discovery_item`.
#[cfg(feature = "server")]
pub mod derive {
    use super::{DiscoveryPair, DiscoveryStatus, Sufficiency};
    use crate::sections::PageLens;
    use store::cases::{Case, DiscoveryItem, DiscoveryRequest};

    /// One served set and its items, as the loader read them.
    pub struct ServedSet<'a> {
        pub request: &'a DiscoveryRequest,
        pub items: &'a [DiscoveryItem],
        /// The case's recorded posture, e.g. `plaintiff`.
        pub posture: &'a str,
    }

    fn device_label(device: &str) -> &'static str {
        match device {
            "interrogatories" => "Interrogatories",
            "requests_for_production" => "Requests for production",
            "requests_for_admission" => "Requests for admission",
            _ => "Discovery",
        }
    }

    fn present(text: Option<&String>) -> Option<String> {
        text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
    }

    fn status(request: &DiscoveryRequest, ours: bool, answered: bool) -> DiscoveryStatus {
        match (request.status.as_str(), ours, answered) {
            ("closed", _, _) => DiscoveryStatus::Closed,
            ("responded", _, _) | (_, false, true) => DiscoveryStatus::Responded,
            (_, true, true) => DiscoveryStatus::ReadyToSign,
            (_, true, false) => DiscoveryStatus::NeedsResponse,
            (_, false, false) => DiscoveryStatus::AwaitingResponse,
        }
    }

    fn sufficiency(objection: bool, answer: bool) -> Sufficiency {
        match (objection, answer) {
            (false, false) => Sufficiency::NoResponse,
            (true, false) => Sufficiency::ObjectionOnly,
            (true, true) => Sufficiency::AnsweredSubjectToObjection,
            (false, true) => Sufficiency::Answered,
        }
    }

    /// The firm's working note on one item, from its state alone.
    fn commentary(
        status: DiscoveryStatus,
        sufficiency: Sufficiency,
        ours: bool,
        due: Option<&str>,
    ) -> String {
        let by = due.map_or_else(String::new, |due| format!(" by {due}"));
        match (status, sufficiency, ours) {
            (DiscoveryStatus::Closed, _, _) => "This set is closed.".to_string(),
            (DiscoveryStatus::NeedsResponse, Sufficiency::ObjectionOnly, _) => format!(
                "We object without answering. Unless the objection holds, an answer is still \
                 owed{by}."
            ),
            (DiscoveryStatus::NeedsResponse, _, _) => {
                format!("Nothing is drafted yet. A response or objection is owed{by}.")
            }
            (DiscoveryStatus::ReadyToSign, Sufficiency::AnsweredSubjectToObjection, _) => {
                "Drafted subject to our objection. Nothing is served until a lawyer signs."
                    .to_string()
            }
            (DiscoveryStatus::ReadyToSign, _, _) => {
                "Drafted. Nothing is served until a lawyer signs.".to_string()
            }
            (DiscoveryStatus::AwaitingResponse, Sufficiency::ObjectionOnly, _)
            | (DiscoveryStatus::Responded, Sufficiency::ObjectionOnly, false) => {
                "They objected without answering. The request stays open until the objection \
                 is resolved."
                    .to_string()
            }
            (DiscoveryStatus::AwaitingResponse, _, _) => {
                format!("Their response is outstanding{by}.")
            }
            (DiscoveryStatus::Responded, Sufficiency::AnsweredSubjectToObjection, false) => {
                "They answered subject to an objection. Check what the objection held back."
                    .to_string()
            }
            (DiscoveryStatus::Responded, _, false) => {
                "They answered. Compare the answer against the request.".to_string()
            }
            (DiscoveryStatus::Responded, _, true) => "Our response is served.".to_string(),
        }
    }

    /// Every item of every set, set by set in item order, for `lens`.
    /// `our_names` and each set's posture decide which side responds; the
    /// commentary is filled on the lawyer lens only.
    #[must_use]
    pub fn pairs(
        sets: &[ServedSet<'_>],
        our_names: &[String],
        lens: PageLens,
    ) -> Vec<DiscoveryPair> {
        let mut out = Vec::new();
        for set in sets {
            let request = set.request;
            let ours =
                super::super::derive::names_ours(&request.responding_party, set.posture, our_names);
            let interrogatories = request.device == "interrogatories";
            for item in set.items {
                let objection = present(item.objections.as_ref());
                let answer = present(item.response_text.as_ref());
                let status = status(request, ours, answer.is_some());
                let sufficiency = sufficiency(objection.is_some(), answer.is_some());
                out.push(DiscoveryPair {
                    key: item.id.to_string(),
                    kicker: format!(
                        "{} · Set {} · No. {}",
                        device_label(&request.device),
                        request.set_number,
                        item.item_number
                    ),
                    request: item.request_text.clone(),
                    objection,
                    objection_by: format!("Counsel for {}", request.responding_party),
                    answer,
                    answer_by: if interrogatories {
                        format!("{}, under oath", request.responding_party)
                    } else {
                        request.responding_party.clone()
                    },
                    answer_heading: if interrogatories {
                        "Answer"
                    } else {
                        "Response"
                    }
                    .to_string(),
                    status,
                    sufficiency,
                    commentary: if lens == PageLens::Lawyer {
                        commentary(
                            status,
                            sufficiency,
                            ours,
                            request.responses_due_on.as_deref(),
                        )
                    } else {
                        String::new()
                    },
                });
            }
        }
        out
    }

    /// Read every set on `cases` and its items, then build the pairs.
    ///
    /// # Errors
    /// Any store error, as text.
    pub async fn load(
        surreal: &store::surreal::SurrealDb,
        cases: &[Case],
        our_names: &[String],
        lens: PageLens,
    ) -> Result<Vec<DiscoveryPair>, String> {
        let mut read = Vec::new();
        for case in cases {
            for request in store::cases::discovery_requests(surreal, case.id)
                .await
                .map_err(|e| e.to_string())?
            {
                let items = store::cases::items(surreal, request.id)
                    .await
                    .map_err(|e| e.to_string())?;
                read.push((request, items, case.posture.clone()));
            }
        }
        let sets: Vec<ServedSet<'_>> = read
            .iter()
            .map(|(request, items, posture)| ServedSet {
                request,
                items,
                posture,
            })
            .collect();
        Ok(pairs(&sets, our_names, lens))
    }

    #[cfg(test)]
    mod tests {
        use super::{pairs, ServedSet};
        use crate::sections::discovery::{DiscoveryStatus, Sufficiency};
        use crate::sections::PageLens;
        use store::cases::{DiscoveryItem, DiscoveryRequest};
        use uuid::Uuid;

        fn request(status: &str, responding: &str) -> DiscoveryRequest {
            DiscoveryRequest {
                id: Uuid::now_v7(),
                case_id: Uuid::now_v7(),
                docket_entry_id: None,
                device: "interrogatories".into(),
                set_number: 1,
                propounding_party: "Defendant Prine".into(),
                responding_party: responding.into(),
                served_on: Some("2026-03-01".into()),
                responses_due_on: Some("2026-03-31".into()),
                status: status.into(),
                inserted_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }
        }

        fn item(n: i32, answer: Option<&str>, objection: Option<&str>) -> DiscoveryItem {
            DiscoveryItem {
                id: Uuid::now_v7(),
                discovery_request_id: Uuid::now_v7(),
                item_number: n,
                request_text: format!("Synthetic request {n}"),
                response_text: answer.map(str::to_string),
                objections: objection.map(str::to_string),
                inserted_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }
        }

        fn ours() -> Vec<String> {
            vec!["Dermot Cruller".to_string()]
        }

        #[test]
        fn each_state_is_derived_from_the_record() {
            let served = request("served", "Plaintiff Dermot Cruller");
            let items = [
                item(1, None, None),
                item(2, Some("Synthetic answer"), Some("Overbroad")),
                item(3, Some("Synthetic answer"), None),
                item(4, None, Some("Privileged")),
            ];
            let out = pairs(
                &[ServedSet {
                    request: &served,
                    items: &items,
                    posture: "plaintiff",
                }],
                &ours(),
                PageLens::Lawyer,
            );
            let got: Vec<(DiscoveryStatus, Sufficiency)> =
                out.iter().map(|p| (p.status, p.sufficiency)).collect();
            assert_eq!(
                got,
                [
                    (DiscoveryStatus::NeedsResponse, Sufficiency::NoResponse),
                    (
                        DiscoveryStatus::ReadyToSign,
                        Sufficiency::AnsweredSubjectToObjection
                    ),
                    (DiscoveryStatus::ReadyToSign, Sufficiency::Answered),
                    (DiscoveryStatus::NeedsResponse, Sufficiency::ObjectionOnly),
                ]
            );
            assert_eq!(out[0].kicker, "Interrogatories · Set 1 · No. 1");
            assert_eq!(out[1].objection_by, "Counsel for Plaintiff Dermot Cruller");
            assert_eq!(out[1].answer_by, "Plaintiff Dermot Cruller, under oath");
            assert!(out[0].commentary.contains("2026-03-31"), "{:?}", out[0]);
        }

        #[test]
        fn their_unanswered_set_awaits_them_and_their_answered_set_is_responded() {
            let theirs = request("served", "Defendant Prine");
            let answered = request("responded", "Defendant Prine");
            let pending = [item(1, None, None)];
            let done = [item(1, Some("Synthetic answer"), None)];
            let out = pairs(
                &[
                    ServedSet {
                        request: &theirs,
                        items: &pending,
                        posture: "plaintiff",
                    },
                    ServedSet {
                        request: &answered,
                        items: &done,
                        posture: "plaintiff",
                    },
                ],
                &ours(),
                PageLens::Lawyer,
            );
            assert_eq!(out[0].status, DiscoveryStatus::AwaitingResponse);
            assert_eq!(out[1].status, DiscoveryStatus::Responded);
        }

        #[test]
        fn commentary_is_filled_on_the_lawyer_lens_only() {
            let served = request("served", "Plaintiff Dermot Cruller");
            let items = [item(1, None, Some("Overbroad"))];
            for lens in [PageLens::Client, PageLens::Clerk] {
                let out = pairs(
                    &[ServedSet {
                        request: &served,
                        items: &items,
                        posture: "plaintiff",
                    }],
                    &ours(),
                    lens,
                );
                assert!(out[0].commentary.is_empty(), "{lens:?}: {:?}", out[0]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        discovery_filter, DiscoveryFilter, DiscoveryPair, DiscoveryStatus, DiscoveryView,
        Sufficiency,
    };
    use crate::sections::tests::render;
    use crate::sections::{PageLens, SectionView};

    fn pair(
        n: u32,
        status: DiscoveryStatus,
        sufficiency: Sufficiency,
        answer: Option<&str>,
        objection: Option<&str>,
        commentary: &str,
    ) -> DiscoveryPair {
        DiscoveryPair {
            key: format!("item-{n}"),
            kicker: format!("Interrogatories · Set 1 · No. {n}"),
            request: format!("Synthetic request {n}"),
            objection: objection.map(str::to_string),
            objection_by: "Counsel for Plaintiff".into(),
            answer: answer.map(str::to_string),
            answer_by: "Plaintiff, under oath".into(),
            answer_heading: "Answer".into(),
            status,
            sufficiency,
            commentary: commentary.into(),
        }
    }

    fn view(filter: DiscoveryFilter, lens: PageLens, commentary: &str) -> DiscoveryView {
        DiscoveryView {
            lens: Some(lens),
            filter,
            page_href: "/app/projects/m/discovery".into(),
            pairs: vec![
                pair(
                    1,
                    DiscoveryStatus::NeedsResponse,
                    Sufficiency::NoResponse,
                    None,
                    None,
                    commentary,
                ),
                pair(
                    2,
                    DiscoveryStatus::Responded,
                    Sufficiency::AnsweredSubjectToObjection,
                    Some("Synthetic sworn answer"),
                    Some("Synthetic objection: overbroad"),
                    commentary,
                ),
                pair(
                    3,
                    DiscoveryStatus::ReadyToSign,
                    Sufficiency::Answered,
                    Some("Synthetic drafted answer"),
                    None,
                    commentary,
                ),
            ],
        }
    }

    #[test]
    fn an_unanswered_item_shows_its_request_and_no_boxes() {
        let mut only = view(DiscoveryFilter::All, PageLens::Lawyer, "");
        only.pairs.truncate(1);
        let html = render(SectionView::DiscoveryPairs(only));
        assert!(html.contains("Interrogatories · Set 1 · No. 1"), "{html}");
        assert!(html.contains("Synthetic request 1"), "{html}");
        assert!(html.contains("Needs a response"), "{html}");
        assert!(html.contains("No response"), "{html}");
        assert!(!html.contains("matter-discovery__box"), "{html}");
    }

    #[test]
    fn an_objected_and_answered_item_attributes_each_box() {
        let html = render(SectionView::DiscoveryPairs(view(
            DiscoveryFilter::Objecting,
            PageLens::Lawyer,
            "",
        )));
        assert!(html.contains("Synthetic objection: overbroad"), "{html}");
        assert!(html.contains("Counsel for Plaintiff"), "{html}");
        assert!(html.contains("Synthetic sworn answer"), "{html}");
        assert!(html.contains("Plaintiff, under oath"), "{html}");
        assert!(html.contains("Answered subject to objection"), "{html}");
        assert!(!html.contains("Synthetic request 1"), "filtered: {html}");
    }

    #[test]
    fn a_draft_ready_to_sign_shows_under_its_filter() {
        let html = render(SectionView::DiscoveryPairs(view(
            DiscoveryFilter::ReadyToSign,
            PageLens::Lawyer,
            "Drafted. Nothing is served until a lawyer signs.",
        )));
        assert!(html.contains("data-status=\"ready-to-sign\""), "{html}");
        assert!(html.contains("Synthetic drafted answer"), "{html}");
        assert!(html.contains("Where it leaves us"), "{html}");
        assert!(!html.contains("Synthetic sworn answer"), "{html}");
    }

    #[test]
    fn the_filter_is_a_set_of_links_with_counts() {
        let html = render(SectionView::DiscoveryPairs(view(
            DiscoveryFilter::NeedsYou,
            PageLens::Lawyer,
            "",
        )));
        assert!(html.contains("data-filter=\"needs-you\""), "{html}");
        for href in [
            "href=\"/app/projects/m/discovery\"",
            "href=\"/app/projects/m/discovery?discovery=needs-you\"",
            "href=\"/app/projects/m/discovery?discovery=objecting\"",
            "href=\"/app/projects/m/discovery?discovery=ready-to-sign\"",
        ] {
            assert!(html.contains(href), "{href}: {html}");
        }
        assert!(html.contains("Needs you (1)"), "{html}");
        assert!(html.contains("All (3)"), "{html}");
        assert_eq!(
            discovery_filter("side=ours&discovery=objecting"),
            DiscoveryFilter::Objecting
        );
        assert_eq!(discovery_filter("discovery=nope"), DiscoveryFilter::All);
    }

    #[test]
    fn no_commentary_renders_without_commentary() {
        let html = render(SectionView::DiscoveryPairs(view(
            DiscoveryFilter::All,
            PageLens::Client,
            "",
        )));
        assert!(!html.contains("Where it leaves us"), "{html}");
        assert!(!html.contains("data-firm-only"), "{html}");
    }

    #[test]
    fn progress_and_status_rows_follow_the_pairs() {
        let v = view(DiscoveryFilter::All, PageLens::Lawyer, "");
        assert_eq!(v.progress().done, 2);
        assert_eq!(v.progress().total, 3);
        let rows = v.status_rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].status, "Ready to sign");
    }
}
