//! The server half of the Section registry: each `rules::dashboard::Section`
//! mapped to its [`SectionView`] for one reader's lens.
//!
//! The page loader fetches each data source once, scoped to the lens, and
//! hands it here in a [`LoadContext`]; nothing in this module queries the
//! store, so the lens scoping lives in exactly one place — the loader. A
//! section whose data seam is not built yet maps to its empty view, which
//! renders the section's empty state.

use rules::dashboard::Section;

use super::notations::NotationsData;
use super::{
    AuthorityView, CaptionView, DeadlineCard, DiscoveryView, FactStripView, GraphView, NoteView,
    PageLens, ProjectCostsView, ProvenanceView, ReceiptRow, RosterView, SectionRow, SectionView,
    TimelineView,
};

/// What a page's sections are built from, already scoped to `lens`.
#[derive(Clone, Debug)]
pub struct LoadContext {
    pub lens: PageLens,
    /// Open deadlines this lens may see, soonest first.
    pub deadlines: Vec<DeadlineCard>,
    /// The next dated items still ahead that this lens may see, from the
    /// docket. When empty, `key_dates_strip` reads the deadlines still ahead
    /// instead, so a context built from deadlines alone still has key dates.
    pub key_dates: Vec<SectionRow>,
    /// Parties, counsel, and, on the lawyer lens, the conflicts record.
    pub roster: RosterView,
    /// E-filing submissions this lens may see.
    pub receipts: Vec<ReceiptRow>,
    /// Files this lens may see, newest first.
    pub documents: Vec<SectionRow>,
    /// Invoices this lens may see, newest first.
    pub invoices: Vec<SectionRow>,
    /// Receipt-backed Project costs visible to this lens.
    pub project_costs: ProjectCostsView,
    /// When `project sync` stored the page.
    pub synced_at: String,
    /// The page's slug, for the provenance line.
    pub slug: String,
    /// The case masthead.
    pub caption: CaptionView,
    /// The case timeline, already scoped to the lens.
    pub timeline: TimelineView,
    /// The case in four figures, from the same scoped timeline.
    pub facts: FactStripView,
    /// The Notation sections' data.
    pub notations: NotationsData,
    /// Each served discovery item, with commentary on the lawyer lens only.
    pub discovery: DiscoveryView,
    /// The Authorities this matter uses: every use with links on the lawyer
    /// lens, the client-visible uses as text on the client lens.
    pub authorities: AuthorityView,
    /// The matter's parties and the conflict-check graph's edges among
    /// them; filled on the lawyer lens only.
    pub graph: GraphView,
    /// The estate sections, from the matter's Notations.
    pub estate: super::estate::EstateViews,
    /// The transactional sections, from the matter's Notations and rows.
    pub closing: super::transactional::ClosingViews,
    /// The page's `draft:`, read once for `document_body` and for
    /// `engagement_letter`'s fallback.
    pub page_draft: super::PageDraft,
    /// Whether the matter has any filed engagement letter, whatever its
    /// visibility.
    pub engagement_filed_any: bool,
    /// The filed engagement letters this lens may open, newest first.
    pub engagement_letters: Vec<super::LetterDocument>,
}

impl LoadContext {
    /// A context with no data, for a lens.
    #[must_use]
    pub fn empty(lens: PageLens) -> Self {
        Self {
            lens,
            deadlines: Vec::new(),
            key_dates: Vec::new(),
            roster: RosterView::default(),
            receipts: Vec::new(),
            documents: Vec::new(),
            invoices: Vec::new(),
            project_costs: ProjectCostsView::default(),
            synced_at: String::new(),
            slug: String::new(),
            caption: CaptionView::default(),
            timeline: TimelineView {
                lens: Some(lens),
                ..TimelineView::default()
            },
            facts: FactStripView::default(),
            notations: NotationsData::default(),
            discovery: DiscoveryView {
                lens: Some(lens),
                ..DiscoveryView::default()
            },
            authorities: AuthorityView {
                lens: Some(lens),
                cards: Vec::new(),
            },
            graph: GraphView::default(),
            estate: super::estate::EstateViews::default(),
            closing: super::transactional::ClosingViews::default(),
            page_draft: super::PageDraft::default(),
            engagement_filed_any: false,
            engagement_letters: Vec::new(),
        }
    }
}

/// The key dates: the docket's when it has any, else the open deadlines
/// still ahead, soonest first.
fn key_dates(cx: &LoadContext) -> Vec<SectionRow> {
    if !cx.key_dates.is_empty() {
        return cx.key_dates.clone();
    }
    cx.deadlines
        .iter()
        .filter(|row| !row.overdue)
        .take(super::derive::KEY_DATES)
        .map(|row| SectionRow {
            label: row.due.clone(),
            detail: row.authority.clone(),
            status: row.state.clone(),
        })
        .collect()
}

/// What a page is not, for each lens. Every lens carries one (D003).
fn boundary_note(lens: PageLens) -> String {
    match lens {
        PageLens::Client => {
            "This page is a summary your legal team keeps current. It is not advice \
             on its own: anything that needs a decision still needs a conversation with your \
             lawyer."
                .to_string()
        }
        PageLens::Lawyer => "Firm working view. Nothing here is filed, sent, or signed until a \
             lawyer acts on it."
            .to_string(),
        PageLens::Clerk => "Supervised view. It shows only what a clerk may see; legal judgment \
             stays with the supervising lawyer."
            .to_string(),
    }
}

fn provenance(cx: &LoadContext) -> ProvenanceView {
    if cx.synced_at.is_empty() {
        return ProvenanceView::default();
    }
    ProvenanceView {
        as_of: cx.synced_at.clone(),
        examined: format!(
            "the records stored on this matter in Navigator, arranged by the `{}` page of the \
             matter's repository",
            cx.slug
        ),
        not_examined: "anything held outside Navigator, such as email threads and paper files"
            .to_string(),
    }
}

/// `build(cx)` on the lawyer lens, the empty value on any other: the
/// firm's working counters never reach a client or a clerk page.
fn lawyer_only<T: Default>(cx: &LoadContext, build: impl FnOnce(&LoadContext) -> T) -> T {
    if cx.lens == PageLens::Lawyer {
        build(cx)
    } else {
        T::default()
    }
}

/// The view for `section`, built from `cx`. Exhaustive over [`Section`]: a
/// new section does not compile until it has a view here.
#[must_use]
pub fn load(section: Section, cx: &LoadContext) -> SectionView {
    match section {
        Section::BoundaryNote => SectionView::BoundaryNote(NoteView {
            text: boundary_note(cx.lens),
        }),
        Section::ProvenanceStatement => SectionView::ProvenanceStatement(provenance(cx)),
        Section::DeadlineBoard => SectionView::DeadlineBoard(cx.deadlines.clone()),
        Section::KeyDatesStrip => SectionView::KeyDatesStrip(key_dates(cx)),
        Section::DownloadList => SectionView::DownloadList(cx.documents.clone()),
        Section::Invoice => SectionView::Invoice(cx.invoices.clone()),
        Section::ProjectCosts => SectionView::ProjectCosts(cx.project_costs.clone()),
        Section::ProgressCounter => {
            SectionView::ProgressCounter(lawyer_only(cx, |cx| cx.discovery.progress()))
        }
        Section::QueueRail => SectionView::QueueRail(Vec::new()),
        Section::ItemDetail => SectionView::ItemDetail(Vec::new()),
        Section::ItemStatusSetter => {
            SectionView::ItemStatusSetter(lawyer_only(cx, |cx| cx.discovery.status_rows()))
        }
        Section::Stepper => SectionView::Stepper(Vec::new()),
        Section::AssertionPanel => SectionView::AssertionPanel(Vec::new()),
        Section::SourcePagePanel => SectionView::SourcePagePanel(Vec::new()),
        Section::MetricStrip => SectionView::MetricStrip(Vec::new()),
        Section::WorkstreamCards => SectionView::WorkstreamCards(Vec::new()),
        Section::DecisionQueue => SectionView::DecisionQueue(cx.notations.decisions.clone()),
        Section::DocumentBody => SectionView::DocumentBody(cx.page_draft.clone()),
        Section::CommentRail => SectionView::CommentRail(Vec::new()),
        Section::AuthorityTable => SectionView::AuthorityTable(cx.authorities.clone()),
        Section::DiscoveryPairs => SectionView::DiscoveryPairs(cx.discovery.clone()),
        Section::StageList => {
            SectionView::StageList(lawyer_only(cx, |cx| cx.notations.stages.clone()))
        }
        Section::PackageManifest => SectionView::PackageManifest(Vec::new()),
        Section::EngagementLetter => {
            SectionView::EngagementLetter(super::EngagementLetterView::new(
                cx.engagement_filed_any,
                cx.engagement_letters.clone(),
                &cx.page_draft,
            ))
        }
        Section::PrebillReview => SectionView::PrebillReview(Vec::new()),
        Section::CaseCaption => SectionView::CaseCaption(cx.caption.clone()),
        Section::FactStrip => SectionView::FactStrip(cx.facts.clone()),
        Section::DocketTimeline => SectionView::DocketTimeline(cx.timeline.clone()),
        Section::PartyRoster => SectionView::PartyRoster(cx.roster.clone()),
        Section::EfilingReceipts => SectionView::EfilingReceipts(cx.receipts.clone()),
        Section::NotationDocument => SectionView::NotationDocument(cx.notations.document.clone()),
        Section::NotationList => SectionView::NotationList(cx.notations.list.clone()),
        Section::NotationChecklist => {
            SectionView::NotationChecklist(cx.notations.checklist.clone())
        }
        Section::RelationshipGraph => {
            SectionView::RelationshipGraph(lawyer_only(cx, |cx| cx.graph.clone()))
        }
        Section::InstrumentSet => SectionView::InstrumentSet(cx.estate.instruments.clone()),
        Section::SigningCeremony => SectionView::SigningCeremony(cx.estate.ceremony.clone()),
        Section::DistributionPlan => SectionView::DistributionPlan(cx.estate.distribution.clone()),
        Section::AdministrationSteps => SectionView::AdministrationSteps(cx.estate.stages.clone()),
        Section::FiduciaryRoster => SectionView::FiduciaryRoster(cx.estate.fiduciaries.clone()),
        Section::AssetSchedule => SectionView::AssetSchedule(cx.estate.assets.clone()),
        Section::CapTable => SectionView::CapTable(cx.closing.cap_table.clone()),
        Section::ClosingChecklist => SectionView::ClosingChecklist(cx.closing.checklist.clone()),
        Section::SignatureTracker => SectionView::SignatureTracker(cx.closing.signatures.clone()),
        Section::TermStrip => SectionView::TermStrip(cx.closing.terms.clone()),
        Section::OpenTerms => SectionView::OpenTerms(cx.closing.open_terms.clone()),
        Section::TermComparison => SectionView::TermComparison(cx.closing.comparison.clone()),
        Section::RequirementTracker => {
            SectionView::RequirementTracker(cx.closing.requirements.clone())
        }
        Section::RedlineView => SectionView::RedlineView(cx.closing.redlines.clone()),
    }
}
