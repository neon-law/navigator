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
    AuthorityView, CaptionView, DeadlineCard, DiscoveryView, EvidenceIndexView, FactStripView,
    GraphView, NoteView, PageLens, ProjectCostsView, ProvenanceView, ReceiptRow, RosterView,
    SectionRow, SectionView, TimelineView,
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
    /// The exhibits grouped by the claim each bears on (LAW-185); filled on
    /// the lawyer lens only — firm working state a client page never carries.
    pub evidence: EvidenceIndexView,
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
    /// Fill the caption and roster from a Project's declared Matter Caption.
    /// A local preview has no live case, service list, or participation rows.
    pub fn set_matter_caption(&mut self, matter: &word::matter::Matter) {
        self.caption = super::derive::caption(&[], Some(matter), "");
        self.roster = super::derive::roster(&super::derive::RosterInputs {
            matter: Some(matter),
            client_entity: "",
            team: &[],
            items: &[],
            services: &[],
            references: &[],
            client_lens: self.lens == PageLens::Client,
        });
    }

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
            evidence: EvidenceIndexView::default(),
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
        Section::EvidenceIndex => {
            SectionView::EvidenceIndex(lawyer_only(cx, |cx| cx.evidence.clone()))
        }
        Section::DiscoveryPairs => SectionView::DiscoveryPairs(cx.discovery.clone()),
        Section::ArgumentExchange => {
            SectionView::ArgumentExchange(lawyer_only(cx, |cx| cx.notations.exchanges.clone()))
        }
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
        Section::ActivityLog => SectionView::ActivityLog(Vec::new()),
        Section::PitchHeadline => SectionView::PitchHeadline(Vec::new()),
        Section::ClaimCards => SectionView::ClaimCards(Vec::new()),
        Section::FeeSummary => SectionView::FeeSummary(Vec::new()),
        Section::RuleClocks => SectionView::RuleClocks(Vec::new()),
        Section::PathChooser => SectionView::PathChooser(Vec::new()),
        Section::EligibilityChecklist => SectionView::EligibilityChecklist(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    /// The sections [`load`](super::load) still maps to an empty view, so
    /// their pages always show the empty state. Loading data into one means
    /// removing it from this list in the same change.
    const UNLOADED: [&str; 17] = [
        "ActivityLog",
        "AssertionPanel",
        "ClaimCards",
        "CommentRail",
        "EligibilityChecklist",
        "FeeSummary",
        "ItemDetail",
        "MetricStrip",
        "PackageManifest",
        "PathChooser",
        "PitchHeadline",
        "PrebillReview",
        "QueueRail",
        "RuleClocks",
        "SourcePagePanel",
        "Stepper",
        "WorkstreamCards",
    ];

    /// Every `Section::X => SectionView::X(Vec::new())` arm of `load`.
    fn unloaded_in_source() -> Vec<String> {
        let source = include_str!("load.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        let mut found: Vec<String> = production
            .lines()
            .filter_map(|line| {
                let (section, view) = line.trim().strip_prefix("Section::")?.split_once(" => ")?;
                let view = view.strip_prefix("SectionView::")?;
                (view == format!("{section}(Vec::new()),")).then(|| section.to_string())
            })
            .collect();
        found.sort_unstable();
        found
    }

    #[test]
    fn the_sections_that_load_nothing_are_exactly_the_listed_ones() {
        assert_eq!(
            unloaded_in_source(),
            UNLOADED,
            "a section now loads data (or a new one loads none): update UNLOADED"
        );
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::{load, LoadContext};
    use crate::sections::{
        EvidenceClaim, EvidenceExhibit, EvidenceIndexView, PageLens, SectionView,
    };
    use rules::dashboard::Section;

    fn populated() -> EvidenceIndexView {
        EvidenceIndexView {
            lens: Some(PageLens::Lawyer),
            claims: vec![EvidenceClaim {
                claim: "¶ 12".to_string(),
                exhibits: vec![EvidenceExhibit {
                    document_id: "01j-exhibit".to_string(),
                    href: "/app/projects/c/documents/01j-exhibit".to_string(),
                    label: "letter.pdf".to_string(),
                    dated_at: Some("2026-01-05".to_string()),
                    source: Some("demand letter".to_string()),
                    confidence: Some("exact".to_string()),
                }],
            }],
            unclaimed: Vec::new(),
            unrecorded: 0,
        }
    }

    #[test]
    fn the_lawyer_lens_reads_the_populated_index() {
        let mut cx = LoadContext::empty(PageLens::Lawyer);
        cx.evidence = populated();
        match load(Section::EvidenceIndex, &cx) {
            SectionView::EvidenceIndex(view) => {
                assert_eq!(view, populated());
            }
            other => panic!("wrong view: {other:?}"),
        }
    }

    #[test]
    fn a_client_lens_never_carries_the_index() {
        let mut cx = LoadContext::empty(PageLens::Client);
        cx.evidence = populated();
        match load(Section::EvidenceIndex, &cx) {
            SectionView::EvidenceIndex(view) => {
                assert_eq!(view.claims, Vec::new());
                assert_eq!(view.unclaimed, Vec::new());
            }
            other => panic!("wrong view: {other:?}"),
        }
    }
}
