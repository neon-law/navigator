//! The Section registry: one Dioxus component per
//! [Section](../../docs/glossary/section.md) a [Matter
//! Page](../../docs/glossary/matter-page.md) may carry.
//!
//! A page is declared in a Project manifest's `pages:` block and validated by
//! the gate; this module is the rendering half. Two exhaustive matches keep
//! the halves paired:
//!
//! - [`load`] (server only) maps every `rules::dashboard::Section` to a
//!   [`SectionView`] carrying that section's data for the viewer's lens, so
//!   adding a `Section` variant fails to compile until it has a view.
//! - [`SectionBody`] maps every [`SectionView`] variant to its component, so
//!   adding a view fails to compile until it renders.
//!
//! Every section has an empty state: a page declared before its data exists
//! says so in plain words rather than rendering a blank box. The route that
//! assembles a page lives in [`page`].

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

pub mod argument_exchange;
pub mod authorities;
pub mod checklist;
pub mod discovery;
pub mod engagement;
pub mod estate;
pub mod evidence;
pub mod graph;
pub mod notations;
pub mod page;
pub mod records;
pub mod timeline;
pub mod transactional;

pub use argument_exchange::{ArgumentExchange, ArgumentExchangeView, ArgumentTurn};
pub use authorities::{AuthorityCard, AuthorityQuote, AuthorityView};
pub use checklist::ChecklistStep;
pub use discovery::{DiscoveryFilter, DiscoveryPair, DiscoveryView};
pub use engagement::{EngagementLetterView, LetterDocument, PageDraft};
pub use evidence::{EvidenceClaim, EvidenceExhibit, EvidenceIndexSection, EvidenceIndexView};
pub use graph::{GraphEdge, GraphNode, GraphView};
pub use notations::{ChecklistView, DocumentState, NotationDocumentView, NotationListRow};

pub use records::{DeadlineCard, ReceiptRow, RosterGroup, RosterView};

pub use timeline::{
    CaptionView, DocumentLink, FactStripView, Side, SideFilter, TimelineItem, TimelineView,
};

#[cfg(feature = "server")]
mod derive;
#[cfg(feature = "server")]
pub mod instruments;
#[cfg(feature = "server")]
mod load;
#[cfg(feature = "server")]
pub use load::{load, LoadContext};

/// The face of a page a reader sees, resolved from their participation on the
/// matter. Mirrors `rules::dashboard::Lens` in a wasm-safe shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageLens {
    Client,
    Lawyer,
    Clerk,
}

impl PageLens {
    /// The key the lens is declared under in `lenses:`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Lawyer => "lawyer",
            Self::Clerk => "clerk",
        }
    }
}

/// One display-ready row of a list section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionRow {
    pub label: String,
    pub detail: String,
    /// A short state chip, e.g. `Overdue`; empty for none.
    pub status: String,
}

/// A receipt-backed Project cost with a link to its source Document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCostRow {
    pub amount: String,
    pub detail: String,
    pub status: String,
    pub receipt_href: String,
}

/// Sum of visible Project cost amounts in one currency and certainty state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCostTotal {
    pub amount_status: String,
    pub currency: String,
    pub amount: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCostsView {
    pub rows: Vec<ProjectCostRow>,
    pub totals: Vec<ProjectCostTotal>,
    pub unavailable: bool,
}

/// What a page is not, and what still needs a human.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteView {
    pub text: String,
}

/// As-of date, what was examined, and what was not.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceView {
    pub as_of: String,
    pub examined: String,
    pub not_examined: String,
}

/// How far through an ordered set the reader is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterView {
    pub done: u32,
    pub total: u32,
}

/// One section's data, typed by section. The variant names match
/// `rules::dashboard::Section` one for one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SectionView {
    BoundaryNote(NoteView),
    ProvenanceStatement(ProvenanceView),
    QueueRail(Vec<SectionRow>),
    ItemDetail(Vec<SectionRow>),
    ItemStatusSetter(Vec<SectionRow>),
    ProgressCounter(CounterView),
    Stepper(Vec<SectionRow>),
    AssertionPanel(Vec<SectionRow>),
    SourcePagePanel(Vec<SectionRow>),
    MetricStrip(Vec<SectionRow>),
    WorkstreamCards(Vec<SectionRow>),
    KeyDatesStrip(Vec<SectionRow>),
    DecisionQueue(Vec<SectionRow>),
    DeadlineBoard(Vec<DeadlineCard>),
    DocumentBody(PageDraft),
    CommentRail(Vec<SectionRow>),
    AuthorityTable(AuthorityView),
    EvidenceIndex(EvidenceIndexView),
    DiscoveryPairs(DiscoveryView),
    StageList(Vec<SectionRow>),
    ArgumentExchange(ArgumentExchangeView),
    PackageManifest(Vec<SectionRow>),
    DownloadList(Vec<SectionRow>),
    EngagementLetter(EngagementLetterView),
    Invoice(Vec<SectionRow>),
    ProjectCosts(ProjectCostsView),
    PrebillReview(Vec<SectionRow>),
    CaseCaption(CaptionView),
    FactStrip(FactStripView),
    DocketTimeline(TimelineView),
    PartyRoster(RosterView),
    EfilingReceipts(Vec<ReceiptRow>),
    NotationDocument(NotationDocumentView),
    NotationList(Vec<NotationListRow>),
    NotationChecklist(ChecklistView),
    RelationshipGraph(GraphView),
    InstrumentSet(Vec<estate::InstrumentCard>),
    SigningCeremony(estate::CeremonyView),
    DistributionPlan(estate::DistributionView),
    AdministrationSteps(estate::StagesView),
    FiduciaryRoster(Vec<estate::FiduciaryRow>),
    AssetSchedule(Vec<estate::AssetRow>),
    CapTable(Vec<transactional::HoldingRow>),
    ClosingChecklist(Vec<transactional::ChecklistItem>),
    SignatureTracker(Vec<transactional::SignatureRow>),
    TermStrip(Vec<transactional::TermCell>),
    OpenTerms(Vec<transactional::OpenTermRow>),
    TermComparison(Vec<transactional::ComparisonRow>),
    RequirementTracker(Vec<transactional::RequirementRow>),
    RedlineView(Vec<transactional::RevisionDocument>),
    ActivityLog(Vec<SectionRow>),
    PitchHeadline(Vec<SectionRow>),
    ClaimCards(Vec<SectionRow>),
    FeeSummary(Vec<SectionRow>),
    RuleClocks(Vec<SectionRow>),
    PathChooser(Vec<SectionRow>),
    EligibilityChecklist(Vec<SectionRow>),
}

impl SectionView {
    /// The section's authored name, e.g. `deadline_board`.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::BoundaryNote(_) => "boundary_note",
            Self::ProvenanceStatement(_) => "provenance_statement",
            Self::QueueRail(_) => "queue_rail",
            Self::ItemDetail(_) => "item_detail",
            Self::ItemStatusSetter(_) => "item_status_setter",
            Self::ProgressCounter(_) => "progress_counter",
            Self::Stepper(_) => "stepper",
            Self::AssertionPanel(_) => "assertion_panel",
            Self::SourcePagePanel(_) => "source_page_panel",
            Self::MetricStrip(_) => "metric_strip",
            Self::WorkstreamCards(_) => "workstream_cards",
            Self::KeyDatesStrip(_) => "key_dates_strip",
            Self::DecisionQueue(_) => "decision_queue",
            Self::DeadlineBoard(_) => "deadline_board",
            Self::DocumentBody(_) => "document_body",
            Self::CommentRail(_) => "comment_rail",
            Self::AuthorityTable(_) => "authority_table",
            Self::EvidenceIndex(_) => "evidence_index",
            Self::DiscoveryPairs(_) => "discovery_pairs",
            Self::StageList(_) => "stage_list",
            Self::ArgumentExchange(_) => "argument_exchange",
            Self::PackageManifest(_) => "package_manifest",
            Self::DownloadList(_) => "download_list",
            Self::EngagementLetter(_) => "engagement_letter",
            Self::Invoice(_) => "invoice",
            Self::ProjectCosts(_) => "project_costs",
            Self::PrebillReview(_) => "prebill_review",
            Self::CaseCaption(_) => "case_caption",
            Self::FactStrip(_) => "fact_strip",
            Self::DocketTimeline(_) => "docket_timeline",
            Self::PartyRoster(_) => "party_roster",
            Self::EfilingReceipts(_) => "efiling_receipts",
            Self::NotationDocument(_) => "notation_document",
            Self::NotationList(_) => "notation_list",
            Self::NotationChecklist(_) => "notation_checklist",
            Self::RelationshipGraph(_) => "relationship_graph",
            Self::InstrumentSet(_) => "instrument_set",
            Self::SigningCeremony(_) => "signing_ceremony",
            Self::DistributionPlan(_) => "distribution_plan",
            Self::AdministrationSteps(_) => "administration_steps",
            Self::FiduciaryRoster(_) => "fiduciary_roster",
            Self::AssetSchedule(_) => "asset_schedule",
            Self::CapTable(_) => "cap_table",
            Self::ClosingChecklist(_) => "closing_checklist",
            Self::SignatureTracker(_) => "signature_tracker",
            Self::TermStrip(_) => "term_strip",
            Self::OpenTerms(_) => "open_terms",
            Self::TermComparison(_) => "term_comparison",
            Self::RequirementTracker(_) => "requirement_tracker",
            Self::RedlineView(_) => "redline_view",
            Self::ActivityLog(_) => "activity_log",
            Self::PitchHeadline(_) => "pitch_headline",
            Self::ClaimCards(_) => "claim_cards",
            Self::FeeSummary(_) => "fee_summary",
            Self::RuleClocks(_) => "rule_clocks",
            Self::PathChooser(_) => "path_chooser",
            Self::EligibilityChecklist(_) => "eligibility_checklist",
        }
    }

    /// Every section with no data — each one's empty state.
    #[must_use]
    pub fn empty_set() -> Vec<SectionView> {
        vec![
            Self::BoundaryNote(NoteView::default()),
            Self::ProvenanceStatement(ProvenanceView::default()),
            Self::QueueRail(Vec::new()),
            Self::ItemDetail(Vec::new()),
            Self::ItemStatusSetter(Vec::new()),
            Self::ProgressCounter(CounterView::default()),
            Self::Stepper(Vec::new()),
            Self::AssertionPanel(Vec::new()),
            Self::SourcePagePanel(Vec::new()),
            Self::MetricStrip(Vec::new()),
            Self::WorkstreamCards(Vec::new()),
            Self::KeyDatesStrip(Vec::new()),
            Self::DecisionQueue(Vec::new()),
            Self::DeadlineBoard(Vec::new()),
            Self::DocumentBody(PageDraft::default()),
            Self::CommentRail(Vec::new()),
            Self::AuthorityTable(AuthorityView::default()),
            Self::EvidenceIndex(EvidenceIndexView::default()),
            Self::DiscoveryPairs(DiscoveryView::default()),
            Self::StageList(Vec::new()),
            Self::ArgumentExchange(ArgumentExchangeView::default()),
            Self::PackageManifest(Vec::new()),
            Self::DownloadList(Vec::new()),
            Self::EngagementLetter(EngagementLetterView::default()),
            Self::Invoice(Vec::new()),
            Self::ProjectCosts(ProjectCostsView::default()),
            Self::PrebillReview(Vec::new()),
            Self::CaseCaption(CaptionView::default()),
            Self::FactStrip(FactStripView::default()),
            Self::DocketTimeline(TimelineView::default()),
            Self::PartyRoster(RosterView::default()),
            Self::EfilingReceipts(Vec::new()),
            Self::NotationDocument(NotationDocumentView::default()),
            Self::NotationList(Vec::new()),
            Self::NotationChecklist(ChecklistView::default()),
            Self::RelationshipGraph(GraphView::default()),
            Self::InstrumentSet(Vec::new()),
            Self::SigningCeremony(estate::CeremonyView::default()),
            Self::DistributionPlan(estate::DistributionView::default()),
            Self::AdministrationSteps(estate::StagesView::default()),
            Self::FiduciaryRoster(Vec::new()),
            Self::AssetSchedule(Vec::new()),
            Self::CapTable(Vec::new()),
            Self::ClosingChecklist(Vec::new()),
            Self::SignatureTracker(Vec::new()),
            Self::TermStrip(Vec::new()),
            Self::OpenTerms(Vec::new()),
            Self::TermComparison(Vec::new()),
            Self::RequirementTracker(Vec::new()),
            Self::RedlineView(Vec::new()),
            Self::ActivityLog(Vec::new()),
            Self::PitchHeadline(Vec::new()),
            Self::ClaimCards(Vec::new()),
            Self::FeeSummary(Vec::new()),
            Self::RuleClocks(Vec::new()),
            Self::PathChooser(Vec::new()),
            Self::EligibilityChecklist(Vec::new()),
        ]
    }
}

/// The frame every list section shares: a card titled with the section's
/// heading, its rows, or its empty state.
#[component]
fn SectionFrame(
    name: &'static str,
    heading: &'static str,
    empty: &'static str,
    rows: Vec<SectionRow>,
) -> Element {
    let heading_id = format!("section-{name}");
    rsx! {
        section {
            class: "matter-section",
            "data-section": name,
            aria_labelledby: "{heading_id}",
            crate::components::Card {
                header: rsx! { h2 { id: "{heading_id}", "{heading}" } },
                if rows.is_empty() {
                    p { class: "matter-section__empty", "{empty}" }
                } else {
                    ul { class: "matter-section__rows",
                        for row in rows.iter() {
                            li { class: "matter-section__row",
                                strong { "{row.label}" }
                                if !row.detail.is_empty() {
                                    " · "
                                    span { "{row.detail}" }
                                }
                                if !row.status.is_empty() {
                                    " "
                                    span { class: "status-chip", "{row.status}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

macro_rules! list_section {
    ($component:ident, $name:literal, $heading:literal, $empty:literal) => {
        #[doc = concat!("The `", $name, "` section.")]
        #[component]
        pub fn $component(rows: Vec<SectionRow>) -> Element {
            rsx! { SectionFrame { name: $name, heading: $heading, empty: $empty, rows } }
        }
    };
}

list_section!(
    QueueRailSection,
    "queue_rail",
    "Review queue",
    "Nothing is queued for review."
);
list_section!(
    ItemDetailSection,
    "item_detail",
    "Item detail",
    "Select an item to see its detail."
);
list_section!(
    ItemStatusSetterSection,
    "item_status_setter",
    "Item status",
    "No item has a status yet."
);
list_section!(
    StepperSection,
    "stepper",
    "Steps",
    "There are no steps to move through yet."
);
list_section!(
    AssertionPanelSection,
    "assertion_panel",
    "Assertions under verification",
    "No drafted assertion is waiting for verification."
);
list_section!(
    SourcePagePanelSection,
    "source_page_panel",
    "Source pages",
    "No source page is linked yet."
);
list_section!(
    MetricStripSection,
    "metric_strip",
    "At a glance",
    "No figures are recorded for this matter yet."
);
list_section!(
    WorkstreamCardsSection,
    "workstream_cards",
    "Workstreams",
    "No workstreams are recorded yet."
);
list_section!(
    KeyDatesStripSection,
    "key_dates_strip",
    "Key dates",
    "No key dates are on the calendar."
);
list_section!(
    DecisionQueueSection,
    "decision_queue",
    "Decisions",
    "No decision is waiting on you."
);
list_section!(
    CommentRailSection,
    "comment_rail",
    "Comments",
    "No comments yet."
);
list_section!(
    StageListSection,
    "stage_list",
    "Stages",
    "No stages are planned yet."
);
list_section!(
    PackageManifestSection,
    "package_manifest",
    "What this package includes",
    "Nothing is in this package yet."
);
list_section!(
    DownloadListSection,
    "download_list",
    "Files",
    "No files are available here yet."
);
list_section!(InvoiceSection, "invoice", "Invoices", "No invoices yet.");

list_section!(
    ActivityLogSection,
    "activity_log",
    "Activity log",
    "Nothing has been logged on this matter yet."
);
list_section!(
    PitchHeadlineSection,
    "pitch_headline",
    "Pitch headline",
    "No pitch has been written on this matter yet."
);
list_section!(
    ClaimCardsSection,
    "claim_cards",
    "Claim cards",
    "No claims have been carded on this matter yet."
);
list_section!(
    FeeSummarySection,
    "fee_summary",
    "Fee summary",
    "No fees have been summarized on this matter yet."
);
list_section!(
    RuleClocksSection,
    "rule_clocks",
    "Rule clocks",
    "No clocks are running on this matter yet."
);
list_section!(
    PathChooserSection,
    "path_chooser",
    "Path chooser",
    "No path has been chosen on this matter yet."
);
list_section!(
    EligibilityChecklistSection,
    "eligibility_checklist",
    "Eligibility checklist",
    "No eligibility gates are marked on this matter yet."
);

#[component]
fn ProjectCostsSection(view: ProjectCostsView) -> Element {
    rsx! {
        section { class: "matter-section", "data-section": "project_costs",
            h2 { "Project costs" }
            if view.unavailable { p { class: "matter-section__empty", "Project costs are temporarily unavailable." } }
            else if view.rows.is_empty() { p { class: "matter-section__empty", "No receipt-backed costs are recorded." } }
            else {
                if !view.totals.is_empty() {
                    ul { class: "matter-section__totals", aria_label: "Project cost amount totals", for total in view.totals {
                        li { "{total.amount_status} amount total ({total.currency}): " strong { "{total.amount}" } }
                    } }
                }
                ul { for row in view.rows {
                    li {
                        strong { "{row.amount}" }
                        " — {row.detail} ({row.status}) · "
                        a { href: "{row.receipt_href}", "Receipt" }
                    }
                } }
            }
        }
    }
}
list_section!(
    PrebillReviewSection,
    "prebill_review",
    "Prebill review",
    "No time entries are waiting for review."
);

/// The `boundary_note` section: what this page is not.
#[component]
pub fn BoundaryNoteSection(view: NoteView) -> Element {
    rsx! {
        aside {
            class: "matter-section matter-section--boundary",
            "data-section": "boundary_note",
            aria_label: "What this page is not",
            if view.text.is_empty() {
                p { class: "matter-section__empty", "No boundary note has been written for this page." }
            } else {
                p { "{view.text}" }
            }
        }
    }
}

/// The `provenance_statement` section: as of when, and from what.
#[component]
pub fn ProvenanceStatementSection(view: ProvenanceView) -> Element {
    rsx! {
        footer {
            class: "matter-section matter-section--provenance",
            "data-section": "provenance_statement",
            if view.as_of.is_empty() {
                p { class: "matter-section__empty", "No provenance has been recorded for this page." }
            } else {
                p { "As of {view.as_of}." }
                if !view.examined.is_empty() {
                    p { "Examined: {view.examined}" }
                }
                if !view.not_examined.is_empty() {
                    p { "Not examined: {view.not_examined}" }
                }
            }
        }
    }
}

/// The `progress_counter` section.
#[component]
pub fn ProgressCounterSection(view: CounterView) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "progress_counter",
            aria_labelledby: "section-progress_counter",
            crate::components::Card {
                header: rsx! { h2 { id: "section-progress_counter", "Progress" } },
                if view.total == 0 {
                    p { class: "matter-section__empty", "Nothing to count yet." }
                } else {
                    p { "{view.done} of {view.total} done" }
                }
            }
        }
    }
}

/// One section, rendered by its component. Exhaustive over [`SectionView`].
#[component]
pub fn SectionBody(view: SectionView) -> Element {
    match view {
        SectionView::BoundaryNote(view) => rsx! { BoundaryNoteSection { view } },
        SectionView::ProvenanceStatement(view) => rsx! { ProvenanceStatementSection { view } },
        SectionView::ProgressCounter(view) => rsx! { ProgressCounterSection { view } },
        SectionView::QueueRail(rows) => rsx! { QueueRailSection { rows } },
        SectionView::ItemDetail(rows) => rsx! { ItemDetailSection { rows } },
        SectionView::ItemStatusSetter(rows) => rsx! { ItemStatusSetterSection { rows } },
        SectionView::Stepper(rows) => rsx! { StepperSection { rows } },
        SectionView::AssertionPanel(rows) => rsx! { AssertionPanelSection { rows } },
        SectionView::SourcePagePanel(rows) => rsx! { SourcePagePanelSection { rows } },
        SectionView::MetricStrip(rows) => rsx! { MetricStripSection { rows } },
        SectionView::WorkstreamCards(rows) => rsx! { WorkstreamCardsSection { rows } },
        SectionView::KeyDatesStrip(rows) => rsx! { KeyDatesStripSection { rows } },
        SectionView::DecisionQueue(rows) => rsx! { DecisionQueueSection { rows } },
        SectionView::DeadlineBoard(rows) => rsx! { records::DeadlineBoardSection { rows } },
        SectionView::DocumentBody(draft) => rsx! { engagement::DocumentBodySection { draft } },
        SectionView::CommentRail(rows) => rsx! { CommentRailSection { rows } },
        SectionView::AuthorityTable(view) => rsx! { authorities::AuthorityTableSection { view } },
        SectionView::EvidenceIndex(view) => rsx! { EvidenceIndexSection { view } },
        SectionView::DiscoveryPairs(view) => rsx! { discovery::DiscoveryPairsSection { view } },
        SectionView::StageList(rows) => rsx! { StageListSection { rows } },
        SectionView::ArgumentExchange(view) => {
            rsx! { argument_exchange::ArgumentExchangeSection { view } }
        }
        SectionView::PackageManifest(rows) => rsx! { PackageManifestSection { rows } },
        SectionView::DownloadList(rows) => rsx! { DownloadListSection { rows } },
        SectionView::EngagementLetter(view) => {
            rsx! { engagement::EngagementLetterSection { view } }
        }
        SectionView::Invoice(rows) => rsx! { InvoiceSection { rows } },
        SectionView::ProjectCosts(view) => rsx! { ProjectCostsSection { view } },
        SectionView::PrebillReview(rows) => rsx! { PrebillReviewSection { rows } },
        SectionView::CaseCaption(view) => rsx! { timeline::CaseCaptionSection { view } },
        SectionView::FactStrip(view) => rsx! { timeline::FactStripSection { view } },
        SectionView::DocketTimeline(view) => rsx! { timeline::DocketTimelineSection { view } },
        SectionView::PartyRoster(view) => rsx! { records::PartyRosterSection { view } },
        SectionView::EfilingReceipts(rows) => rsx! { records::EfilingReceiptsSection { rows } },
        SectionView::NotationDocument(view) => rsx! { notations::NotationDocumentSection { view } },
        SectionView::NotationList(rows) => rsx! { notations::NotationListSection { rows } },
        SectionView::NotationChecklist(view) => {
            rsx! { notations::NotationChecklistSection { view } }
        }
        SectionView::RelationshipGraph(view) => rsx! { graph::RelationshipGraphSection { view } },
        SectionView::InstrumentSet(rows) => rsx! { estate::InstrumentSetSection { rows } },
        SectionView::SigningCeremony(view) => rsx! { estate::SigningCeremonySection { view } },
        SectionView::DistributionPlan(view) => rsx! { estate::DistributionPlanSection { view } },
        SectionView::AdministrationSteps(view) => {
            rsx! { estate::AdministrationStepsSection { view } }
        }
        SectionView::FiduciaryRoster(rows) => rsx! { estate::FiduciaryRosterSection { rows } },
        SectionView::AssetSchedule(rows) => rsx! { estate::AssetScheduleSection { rows } },
        SectionView::CapTable(rows) => rsx! { transactional::CapTableSection { rows } },
        SectionView::ClosingChecklist(rows) => {
            rsx! { transactional::ClosingChecklistSection { rows } }
        }
        SectionView::SignatureTracker(rows) => {
            rsx! { transactional::SignatureTrackerSection { rows } }
        }
        SectionView::TermStrip(rows) => rsx! { transactional::TermStripSection { rows } },
        SectionView::OpenTerms(rows) => rsx! { transactional::OpenTermsSection { rows } },
        SectionView::TermComparison(rows) => rsx! { transactional::TermComparisonSection { rows } },
        SectionView::RequirementTracker(rows) => {
            rsx! { transactional::RequirementTrackerSection { rows } }
        }
        SectionView::RedlineView(rows) => rsx! { transactional::RedlineViewSection { rows } },
        SectionView::ActivityLog(rows) => rsx! { ActivityLogSection { rows } },
        SectionView::PitchHeadline(rows) => rsx! { PitchHeadlineSection { rows } },
        SectionView::ClaimCards(rows) => rsx! { ClaimCardsSection { rows } },
        SectionView::FeeSummary(rows) => rsx! { FeeSummarySection { rows } },
        SectionView::RuleClocks(rows) => rsx! { RuleClocksSection { rows } },
        SectionView::PathChooser(rows) => rsx! { PathChooserSection { rows } },
        SectionView::EligibilityChecklist(rows) => rsx! { EligibilityChecklistSection { rows } },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{SectionBody, SectionRow, SectionView};
    use dioxus::prelude::*;

    /// The vocabulary's size. Pinned here, not read from `rules`, because the
    /// wasm build of this crate never links `rules`; the server-side test
    /// below proves the two agree.
    fn rules_section_count() -> usize {
        57
    }

    pub(super) fn render(view: SectionView) -> String {
        #[component]
        fn Harness(view: SectionView) -> Element {
            rsx! { SectionBody { view } }
        }
        let mut dom = VirtualDom::new_with_props(Harness, HarnessProps { view });
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[test]
    fn every_section_renders_its_empty_state() {
        let views = SectionView::empty_set();
        assert_eq!(views.len(), rules_section_count(), "one view per section");
        let mut names: Vec<&str> = views.iter().map(SectionView::name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            rules_section_count(),
            "no section is listed twice"
        );
        for view in views {
            let name = view.name();
            let html = render(view);
            assert!(
                html.contains(&format!("data-section=\"{name}\"")),
                "{name}: {html}"
            );
            assert!(
                html.contains("matter-section__empty"),
                "{name} has no empty state: {html}"
            );
        }
    }

    #[test]
    fn a_list_section_with_rows_renders_them_instead_of_the_empty_state() {
        let html = render(SectionView::KeyDatesStrip(vec![SectionRow {
            label: "2026-11-02 17:00 -08:00".into(),
            detail: "Order setting briefing schedule".into(),
            status: "Upcoming".into(),
        }]));
        assert!(html.contains("Order setting briefing schedule"), "{html}");
        assert!(html.contains("status-chip"), "{html}");
        assert!(!html.contains("matter-section__empty"), "{html}");
    }

    #[test]
    fn project_costs_show_unavailable_instead_of_claiming_no_costs() {
        let html = render(SectionView::ProjectCosts(super::ProjectCostsView {
            rows: Vec::new(),
            totals: Vec::new(),
            unavailable: true,
        }));
        assert!(
            html.contains("Project costs are temporarily unavailable."),
            "{html}"
        );
        assert!(
            !html.contains("No receipt-backed costs are recorded."),
            "{html}"
        );
    }

    #[test]
    fn project_costs_render_estimates_and_confirmed_amounts_as_separate_totals() {
        let html = render(SectionView::ProjectCosts(super::ProjectCostsView {
            rows: vec![super::ProjectCostRow {
                amount: "USD 125.00".into(),
                detail: "2026-10-16 · Example Supplies — Synthetic filing copy charge".into(),
                status: "confirmed · unbilled".into(),
                receipt_href: "/app/projects/sample-transactional/documents/receipt".into(),
            }],
            totals: vec![
                super::ProjectCostTotal {
                    amount_status: "estimated".into(),
                    currency: "USD".into(),
                    amount: "25.00".into(),
                },
                super::ProjectCostTotal {
                    amount_status: "confirmed".into(),
                    currency: "USD".into(),
                    amount: "125.00".into(),
                },
            ],
            unavailable: false,
        }));
        assert!(html.contains("estimated amount total (USD):"), "{html}");
        assert!(html.contains("confirmed amount total (USD):"), "{html}");
        assert!(html.contains("Synthetic filing copy charge"), "{html}");
        assert!(
            html.contains("/app/projects/sample-transactional/documents/receipt"),
            "{html}"
        );
    }

    /// The server's exhaustive `load` maps every rules section to the view of
    /// the same name, so the registry and the vocabulary cannot drift.
    #[cfg(feature = "server")]
    #[test]
    fn every_rules_section_loads_to_the_view_of_its_name() {
        let mut cx = super::LoadContext::empty(super::PageLens::Lawyer);
        cx.notations.stages = vec![super::SectionRow {
            label: "Synthetic question?".into(),
            detail: "Party A".into(),
            status: "High".into(),
        }];
        assert_eq!(rules::dashboard::Section::ALL.len(), rules_section_count());
        for section in rules::dashboard::Section::ALL {
            assert_eq!(super::load(*section, &cx).name(), section.as_str());
        }
        assert!(matches!(
            super::load(rules::dashboard::Section::StageList, &cx),
            SectionView::StageList(rows) if rows == cx.notations.stages
        ));
        cx.notations.exchanges.unavailable = true;
        assert!(matches!(
            super::load(rules::dashboard::Section::ArgumentExchange, &cx),
            SectionView::ArgumentExchange(view) if view.unavailable
        ));
        for lens in [super::PageLens::Client, super::PageLens::Clerk] {
            cx.lens = lens;
            assert!(matches!(
                super::load(rules::dashboard::Section::ArgumentExchange, &cx),
                SectionView::ArgumentExchange(view) if view == super::ArgumentExchangeView::default()
            ));
        }
        cx.lens = super::PageLens::Client;
        assert!(matches!(
            super::load(rules::dashboard::Section::StageList, &cx),
            SectionView::StageList(rows) if rows.is_empty()
        ));
    }
}
