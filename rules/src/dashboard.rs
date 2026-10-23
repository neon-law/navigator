//! The matter page **kind catalog** — the page types an attorney composes,
//! and the section skeleton each one is checked against.
//!
//! # Composition, not code
//!
//! A page kind is not a code path. `kind:` drives **validation and
//! scaffolding, never rendering** — the renderer only ever sees sections. So
//! a kind is a named skeleton plus the rules that check it, which is what
//! makes adding one cost a rules-crate entry rather than a component.
//!
//! Each entry in [`Kind::is_dashboard`]'s catalog recurred across
//! independently-authored matter hubs in the surveyed corpus. What
//! transfers from that survey is the **product knowledge** — which pages
//! lawyers build when nobody constrains them, and what each one contains.
//! None of the implementation transfers.
//!
//! # A declared page
//!
//! A page is one entry in the Project manifest's `pages:` block (see
//! [`crate::pages`]):
//!
//! ```yaml
//! pages:
//!   - slug: review
//!     kind: review_queue_workbench
//!     title: Document review
//!     lenses:
//!       lawyer: [queue_rail, item_detail, item_status_setter, boundary_note, provenance_statement]
//!       client: [boundary_note, provenance_statement]
//! ```
//!
//! `lenses:` is the per-lens composition: the client, lawyer, and clerk
//! section lists live in **one entry**, so the faces of a page cannot drift
//! apart across separate documents.
//!
//! # What every kind carries
//!
//! Two sections are in every skeleton regardless of kind, because the
//! survey found them in 13 of 13 surfaces in one batch and 8 of 9 in
//! another:
//!
//! - [`Section::BoundaryNote`] — what this page is *not*, and what still
//!   requires a human.
//! - [`Section::ProvenanceStatement`] — as-of date, what was examined,
//!   what was not.
//!
//! They are part of the skeleton rather than left to the author, and
//! [`D003RequiredSection`](crate::D003RequiredSection) holds **every**
//! declared lens to both of them. A client face without a boundary note is
//! the failure this rule exists to prevent.
//!
//! # Lenses and authorization
//!
//! A skeleton names the sections each kind may carry and, through
//! [`Skeleton::lawyer_only`], the ones that are the firm's working surface
//! for that kind ([`D005LensSection`](crate::D005LensSection)). It never
//! decides which *rows* a reader sees: each section's loader scopes its
//! data to the viewer, and the route resolves the lens from participation.
//!
//! Each [`Section`] maps 1:1 to a component in `webapp::sections`, whose
//! exhaustive `match` fails to compile until a new variant has one.

use std::fmt::Write as _;

use crate::kind::Kind;

/// The two families the surveyed corpus splits into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// An interactive surface a case team works in and iterates on.
    Dashboard,
    /// A linear, non-interactive handoff page. It is read, not worked in.
    Deliverable,
}

/// A face of a matter page. The three lists are authored in one entry, so a
/// page's faces cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    /// What a client participant sees.
    Client,
    /// What the firm sees.
    Lawyer,
    /// What a court clerk or comparable outside reader sees.
    Clerk,
}

impl Lens {
    /// Every lens, in declaration order.
    pub const ALL: &'static [Lens] = &[Lens::Client, Lens::Lawyer, Lens::Clerk];

    /// The key this lens is authored under in `lenses:`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Lens::Client => "client",
            Lens::Lawyer => "lawyer",
            Lens::Clerk => "clerk",
        }
    }

    /// Parse a `lenses:` key, or `None` when it names no known lens.
    #[must_use]
    pub fn parse(value: &str) -> Option<Lens> {
        Self::ALL.iter().copied().find(|l| l.as_str() == value)
    }
}

/// A section type a declared page may carry.
///
/// Closed on purpose: an author picks from a kind's catalog, and anything
/// outside it is a validation error rather than a silently-dropped
/// section. A shape the registry lacks is a new variant here plus its
/// component in `webapp::sections`, which then benefits every matter and
/// every tenant — never a hand-written page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    /// What this page is not, and what still requires a human. Universal.
    BoundaryNote,
    /// As-of date, what was examined, what was not. Universal.
    ProvenanceStatement,
    /// A filterable rail of the items under review.
    QueueRail,
    /// The detail pane for the selected item.
    ItemDetail,
    /// A per-item status and note the reviewer sets.
    ItemStatusSetter,
    /// How far through the queue the reviewer is.
    ProgressCounter,
    /// Prev/next movement through an ordered set.
    Stepper,
    /// The drafted assertion under verification.
    AssertionPanel,
    /// The source page region the assertion rests on (#893).
    SourcePagePanel,
    /// The headline numbers for a matter at a glance.
    MetricStrip,
    /// One card per workstream, each with a status chip.
    WorkstreamCards,
    /// The dates that matter, in order.
    KeyDatesStrip,
    /// Decisions only the principal can make.
    DecisionQueue,
    /// Dated rows with hard/soft classification, provenance confidence,
    /// and per-row calendar export (#539, #688).
    DeadlineBoard,
    /// A draft's block-structured Markdown body. Pagination and paper
    /// geometry are Typst's job (#889), never the browser's.
    DocumentBody,
    /// Phrase-anchored comments alongside a document body.
    CommentRail,
    /// Authorities with citation, holding, verbatim quote, and position
    /// for this matter (#890).
    AuthorityTable,
    /// Numbered request/response pairs with objection tracking.
    DiscoveryPairs,
    /// Ordered stages with argument points, authorities, and anticipated
    /// questions.
    StageList,
    /// What a package includes and, as importantly, what it excludes.
    PackageManifest,
    /// The files a reader takes away.
    DownloadList,
    /// The engagement agreement as a record.
    EngagementLetter,
    /// An issued invoice as a record.
    Invoice,
    /// Third-party Project costs and their receipt Documents.
    ProjectCosts,
    /// Prebill review with per-entry findings.
    PrebillReview,
    /// The case masthead: caption, court, case number, and judge.
    CaseCaption,
    /// The case in four figures: opened, on file, still ahead, and next.
    FactStrip,
    /// The case timeline: Docket Entries with their documents and the open
    /// Deadlines, in date order, filterable by side.
    DocketTimeline,
    /// The parties, counsel, and service list of a matter.
    PartyRoster,
    /// Court e-filing submissions: submitted, title, transaction, status.
    EfilingReceipts,
    /// One Notation's body as a collapsible Harvard outline, verified
    /// against the Draft digest it pinned.
    NotationDocument,
    /// Each Notation on the matter with its client-facing state phrase.
    NotationList,
    /// The steps a Notation's holds and blanks leave open.
    NotationChecklist,
    /// The matter's parties and the conflict-check graph's edges among
    /// them, drawn as a server-rendered diagram with a table beside it.
    RelationshipGraph,
    /// One card per estate instrument: its execution formalities, from its
    /// Notation's workflow, and where it stands.
    InstrumentSet,
    /// The signing ceremony: when, where, and each instrument's steps.
    SigningCeremony,
    /// The residuary shares the trust names.
    DistributionPlan,
    /// A Notation's workflow states, rendered as stages.
    AdministrationSteps,
    /// Who the estate plan names to each fiduciary role, in successor order.
    FiduciaryRoster,
    /// The estate's assets: how each passes and whether it is funded.
    AssetSchedule,
    /// Who holds which shares, with each holding's fully diluted share.
    CapTable,
    /// Conditions precedent and deliverables, from the matter's Notations.
    ClosingChecklist,
    /// Each signature request: sent, signed, or declined.
    SignatureTracker,
    /// The terms the Drafts carry, each agreed or open.
    TermStrip,
    /// The open terms and holds still to resolve, linked to each Draft.
    OpenTerms,
    /// The other side's proposal beside the client's position, by clause.
    TermComparison,
    /// Each filing a workflow requires, and whether it was made.
    RequirementTracker,
    /// The revisions of each living document.
    RedlineView,
}

/// The sections every kind carries, whatever else it carries.
pub const UNIVERSAL: &[Section] = &[Section::BoundaryNote, Section::ProvenanceStatement];

impl Section {
    /// Every section type, in declaration order.
    pub const ALL: &'static [Section] = &[
        Section::BoundaryNote,
        Section::ProvenanceStatement,
        Section::QueueRail,
        Section::ItemDetail,
        Section::ItemStatusSetter,
        Section::ProgressCounter,
        Section::Stepper,
        Section::AssertionPanel,
        Section::SourcePagePanel,
        Section::MetricStrip,
        Section::WorkstreamCards,
        Section::KeyDatesStrip,
        Section::DecisionQueue,
        Section::DeadlineBoard,
        Section::DocumentBody,
        Section::CommentRail,
        Section::AuthorityTable,
        Section::DiscoveryPairs,
        Section::StageList,
        Section::PackageManifest,
        Section::DownloadList,
        Section::EngagementLetter,
        Section::Invoice,
        Section::ProjectCosts,
        Section::PrebillReview,
        Section::CaseCaption,
        Section::FactStrip,
        Section::DocketTimeline,
        Section::PartyRoster,
        Section::EfilingReceipts,
        Section::NotationDocument,
        Section::NotationList,
        Section::NotationChecklist,
        Section::RelationshipGraph,
        Section::InstrumentSet,
        Section::SigningCeremony,
        Section::DistributionPlan,
        Section::AdministrationSteps,
        Section::FiduciaryRoster,
        Section::AssetSchedule,
        Section::CapTable,
        Section::ClosingChecklist,
        Section::SignatureTracker,
        Section::TermStrip,
        Section::OpenTerms,
        Section::TermComparison,
        Section::RequirementTracker,
        Section::RedlineView,
    ];

    /// The name this section is authored as.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Section::BoundaryNote => "boundary_note",
            Section::ProvenanceStatement => "provenance_statement",
            Section::QueueRail => "queue_rail",
            Section::ItemDetail => "item_detail",
            Section::ItemStatusSetter => "item_status_setter",
            Section::ProgressCounter => "progress_counter",
            Section::Stepper => "stepper",
            Section::AssertionPanel => "assertion_panel",
            Section::SourcePagePanel => "source_page_panel",
            Section::MetricStrip => "metric_strip",
            Section::WorkstreamCards => "workstream_cards",
            Section::KeyDatesStrip => "key_dates_strip",
            Section::DecisionQueue => "decision_queue",
            Section::DeadlineBoard => "deadline_board",
            Section::DocumentBody => "document_body",
            Section::CommentRail => "comment_rail",
            Section::AuthorityTable => "authority_table",
            Section::DiscoveryPairs => "discovery_pairs",
            Section::StageList => "stage_list",
            Section::PackageManifest => "package_manifest",
            Section::DownloadList => "download_list",
            Section::EngagementLetter => "engagement_letter",
            Section::Invoice => "invoice",
            Section::ProjectCosts => "project_costs",
            Section::PrebillReview => "prebill_review",
            Section::CaseCaption => "case_caption",
            Section::FactStrip => "fact_strip",
            Section::DocketTimeline => "docket_timeline",
            Section::PartyRoster => "party_roster",
            Section::EfilingReceipts => "efiling_receipts",
            Section::NotationDocument => "notation_document",
            Section::NotationList => "notation_list",
            Section::NotationChecklist => "notation_checklist",
            Section::RelationshipGraph => "relationship_graph",
            Section::InstrumentSet => "instrument_set",
            Section::SigningCeremony => "signing_ceremony",
            Section::DistributionPlan => "distribution_plan",
            Section::AdministrationSteps => "administration_steps",
            Section::FiduciaryRoster => "fiduciary_roster",
            Section::AssetSchedule => "asset_schedule",
            Section::CapTable => "cap_table",
            Section::ClosingChecklist => "closing_checklist",
            Section::SignatureTracker => "signature_tracker",
            Section::TermStrip => "term_strip",
            Section::OpenTerms => "open_terms",
            Section::TermComparison => "term_comparison",
            Section::RequirementTracker => "requirement_tracker",
            Section::RedlineView => "redline_view",
        }
    }

    /// A one-line summary — the surface an editor completion shows.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Section::BoundaryNote => "What this page is not, and what still requires a human",
            Section::ProvenanceStatement => {
                "As-of date, what was examined, and what was not examined"
            }
            Section::QueueRail => "A filterable rail of the items under review",
            Section::ItemDetail => "The detail pane for the selected item",
            Section::ItemStatusSetter => "A per-item status and note the reviewer sets",
            Section::ProgressCounter => "How far through the queue the reviewer is",
            Section::Stepper => "Prev/next movement through an ordered set",
            Section::AssertionPanel => "The drafted assertion under verification",
            Section::SourcePagePanel => "The source page region the assertion rests on",
            Section::MetricStrip => "The headline numbers for the matter at a glance",
            Section::WorkstreamCards => "One card per workstream, each with a status chip",
            Section::KeyDatesStrip => "The dates that matter, in order",
            Section::DecisionQueue => "Decisions only the principal can make",
            Section::DeadlineBoard => {
                "Dated rows with hard/soft classification and calendar export"
            }
            Section::DocumentBody => "A draft's block-structured Markdown body",
            Section::CommentRail => "Phrase-anchored comments alongside a document body",
            Section::AuthorityTable => "Authorities with citation, holding, quote, and position",
            Section::DiscoveryPairs => "Numbered request/response pairs with objection tracking",
            Section::StageList => "Ordered stages with argument points and anticipated questions",
            Section::PackageManifest => "What the package includes, and what it excludes",
            Section::DownloadList => "The files a reader takes away",
            Section::EngagementLetter => "The engagement agreement as a record",
            Section::Invoice => "An issued invoice as a record",
            Section::ProjectCosts => "Receipt-backed costs recorded for this Project",
            Section::PrebillReview => "Prebill review with per-entry findings",
            Section::CaseCaption => "The case caption, court, case number, and judge",
            Section::FactStrip => {
                "Case opened, documents on file, dates still ahead, and the next one"
            }
            Section::DocketTimeline => {
                "Docket entries with their documents and open deadlines, filterable by side"
            }
            Section::PartyRoster => "The parties, counsel, and service list",
            Section::EfilingReceipts => {
                "Court e-filing submissions with transaction number and status"
            }
            Section::NotationDocument => {
                "One Notation's body as a Harvard outline, verified against its Draft"
            }
            Section::NotationList => "Each Notation on the matter with where it stands",
            Section::NotationChecklist => "The steps a Notation's holds and blanks leave open",
            Section::RelationshipGraph => {
                "The matter's parties and the relationships among them, as a diagram and a table"
            }
            Section::InstrumentSet => {
                "One card per estate instrument with its formalities and status"
            }
            Section::SigningCeremony => "The signing ceremony's date, place, and steps",
            Section::DistributionPlan => "The residuary shares the trust names",
            Section::AdministrationSteps => "Workflow states rendered as stages",
            Section::FiduciaryRoster => "Each fiduciary role and its successors, in order",
            Section::AssetSchedule => "Each estate asset, how it passes, and its funding",
            Section::CapTable => "Holders, share classes, shares, and fully diluted ownership",
            Section::ClosingChecklist => "Conditions precedent and deliverables for closing",
            Section::SignatureTracker => "Each signature request: sent, signed, or declined",
            Section::TermStrip => "The terms the Drafts carry, each agreed or open",
            Section::OpenTerms => "Open terms and holds, each linked to its Draft",
            Section::TermComparison => "The proposal beside the client's position, by clause",
            Section::RequirementTracker => "Each required filing and whether it was made",
            Section::RedlineView => "The revisions of each living document",
        }
    }

    /// Parse an authored section name, or `None` when it names nothing in
    /// the vocabulary.
    #[must_use]
    pub fn parse(value: &str) -> Option<Section> {
        Self::ALL.iter().copied().find(|s| s.as_str() == value)
    }

    /// The section's name as a prose heading — what [`scaffold`] writes
    /// above the stub. Derived from [`Section::as_str`] so the two can
    /// never drift.
    #[must_use]
    pub fn heading(self) -> String {
        let spaced = self.as_str().replace('_', " ");
        let mut chars = spaced.chars();
        chars.next().map_or(spaced.clone(), |first| {
            first.to_uppercase().collect::<String>() + chars.as_str()
        })
    }
}

/// One kind's skeleton: what it must carry to be that kind, and what else
/// it may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Skeleton {
    /// Which family the kind belongs to.
    pub family: Family,
    /// The spine. Without these the page is not this kind, so each must
    /// appear in at least one declared lens.
    pub required: &'static [Section],
    /// Sections the kind may carry. Not required anywhere.
    pub optional: &'static [Section],
    /// Sections of this kind's catalog that only the lawyer lens may carry:
    /// the firm's working surface for the page.
    pub lawyer_only: &'static [Section],
}

impl Skeleton {
    /// Every section this kind may declare: its spine, its options, and
    /// the universal two. The allowlist
    /// [`D002OutOfCatalogSection`](crate::D002OutOfCatalogSection) checks
    /// against.
    #[must_use]
    pub fn catalog(&self) -> Vec<Section> {
        let mut out: Vec<Section> = UNIVERSAL.to_vec();
        out.extend_from_slice(self.required);
        out.extend_from_slice(self.optional);
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Whether this kind permits `section` on `lens`. A section outside the
    /// catalog is D002's report; this answers only the per-lens question
    /// [`D005LensSection`](crate::D005LensSection) checks.
    #[must_use]
    pub fn permits(&self, lens: Lens, section: Section) -> bool {
        lens == Lens::Lawyer || !self.lawyer_only.contains(&section)
    }
}

/// The skeleton for a dashboard kind, or `None` for any other [`Kind`].
///
/// Exhaustive over [`Kind`] on purpose: adding a dashboard kind fails to
/// compile until it declares a skeleton, so a kind can never reach an
/// author with no catalog behind it.
// One table, one arm per kind: its length is the catalog's, not logic's.
#[allow(clippy::too_many_lines)]
#[must_use]
pub fn skeleton(kind: Kind) -> Option<Skeleton> {
    use Section as S;
    let s = match kind {
        // The dominant layout in the corpus — six of nine surfaces in one
        // batch. Document review, discovery response, and citation
        // checking are all specialisations of it.
        Kind::ReviewQueueWorkbench => Skeleton {
            family: Family::Dashboard,
            required: &[S::QueueRail, S::ItemDetail, S::ItemStatusSetter],
            optional: &[S::ProgressCounter, S::Stepper],
            lawyer_only: &[S::ItemStatusSetter],
        },
        // Three hubs built this independently: drafted assertion on the
        // left, source page on the right, both marked (#891, #893).
        Kind::VerifierSplitView => Skeleton {
            family: Family::Dashboard,
            required: &[S::AssertionPanel, S::SourcePagePanel, S::ItemStatusSetter],
            optional: &[S::Stepper, S::ProgressCounter, S::AuthorityTable],
            lawyer_only: &[S::AssertionPanel, S::ItemStatusSetter],
        },
        Kind::MatterStatusConsole => Skeleton {
            family: Family::Dashboard,
            required: &[S::MetricStrip, S::WorkstreamCards, S::DecisionQueue],
            optional: &[
                S::KeyDatesStrip,
                S::NotationList,
                S::NotationChecklist,
                S::CapTable,
            ],
            lawyer_only: &[],
        },
        // The litigation board: the deadlines spine, with the case caption,
        // its fact strip, and the docket timeline beside it.
        Kind::DocketDeadlineBoard => Skeleton {
            family: Family::Dashboard,
            required: &[S::DeadlineBoard],
            optional: &[
                S::KeyDatesStrip,
                S::MetricStrip,
                S::CaseCaption,
                S::FactStrip,
                S::DocketTimeline,
                S::PartyRoster,
                S::EfilingReceipts,
                S::RelationshipGraph,
                S::AdministrationSteps,
            ],
            lawyer_only: &[S::RelationshipGraph],
        },
        Kind::DocumentWorkbench => Skeleton {
            family: Family::Dashboard,
            required: &[S::DocumentBody],
            optional: &[
                S::CommentRail,
                S::DownloadList,
                S::NotationDocument,
                S::NotationList,
                S::NotationChecklist,
            ],
            lawyer_only: &[],
        },
        Kind::AuthorityLibrary => Skeleton {
            family: Family::Dashboard,
            required: &[S::AuthorityTable],
            optional: &[S::SourcePagePanel],
            lawyer_only: &[S::AuthorityTable],
        },
        Kind::DiscoveryCockpit => Skeleton {
            family: Family::Dashboard,
            required: &[S::DiscoveryPairs],
            optional: &[S::ProgressCounter, S::ItemStatusSetter, S::DownloadList],
            lawyer_only: &[S::ItemStatusSetter],
        },
        Kind::HearingConsole => Skeleton {
            family: Family::Dashboard,
            required: &[S::StageList],
            optional: &[
                S::AuthorityTable,
                S::KeyDatesStrip,
                S::Stepper,
                S::CaseCaption,
                S::RelationshipGraph,
            ],
            lawyer_only: &[S::StageList, S::AuthorityTable, S::RelationshipGraph],
        },
        // The second family: linear, non-interactive, read rather than
        // worked in.
        Kind::DeliverablePackage => Skeleton {
            family: Family::Deliverable,
            required: &[S::PackageManifest],
            optional: &[S::DownloadList, S::NotationDocument],
            lawyer_only: &[],
        },
        // A Dashboard rather than a Deliverable because prebill review is
        // a surface the firm works in; the letter and invoice beside it
        // are the records that review is against.
        Kind::EngagementBillingRecords => Skeleton {
            family: Family::Dashboard,
            required: &[S::EngagementLetter, S::Invoice],
            optional: &[S::PrebillReview, S::DownloadList],
            lawyer_only: &[S::PrebillReview],
        },
        // A transaction's closing: what must happen, who has signed, which
        // terms are still open, and how each document changed. The
        // clause-by-clause comparison is the firm's review work product.
        Kind::ClosingRoom => Skeleton {
            family: Family::Dashboard,
            required: &[S::ClosingChecklist],
            optional: &[
                S::SignatureTracker,
                S::TermStrip,
                S::OpenTerms,
                S::TermComparison,
                S::RequirementTracker,
                S::RedlineView,
                S::CapTable,
                S::KeyDatesStrip,
                S::DownloadList,
            ],
            lawyer_only: &[S::TermComparison],
        },
        // The estate plan: the instruments and their ceremony, what the
        // trust distributes, and what happens after signing. Each loader
        // scopes its rows to the reader, so no section is firm-only.
        Kind::EstatePlanConsole => Skeleton {
            family: Family::Dashboard,
            required: &[S::InstrumentSet],
            optional: &[
                S::SigningCeremony,
                S::DistributionPlan,
                S::AdministrationSteps,
                S::FiduciaryRoster,
                S::AssetSchedule,
                S::KeyDatesStrip,
                S::DownloadList,
            ],
            lawyer_only: &[],
        },
        Kind::Letter
        | Kind::Filing
        | Kind::Will
        | Kind::Trust
        | Kind::Directive
        | Kind::Agreement
        | Kind::Pleading
        | Kind::Onboarding
        | Kind::Offboarding
        | Kind::Memo
        | Kind::Resume
        | Kind::TrialPrep
        | Kind::Event
        | Kind::Post
        | Kind::Workshop
        | Kind::Github
        | Kind::Transcript
        | Kind::InboundContract
        | Kind::CertificateOfNaturalization
        | Kind::Exhibit
        | Kind::Order
        | Kind::ClosedRepository
        | Kind::Invoice
        | Kind::Unclassified => return None,
    };
    Some(s)
}

/// Every section a page kind may declare, or an empty vector for any other
/// kind. The surface the scaffolder and the LSP's completion both read.
#[must_use]
pub fn catalog(kind: Kind) -> Vec<Section> {
    skeleton(kind).map(|s| s.catalog()).unwrap_or_default()
}

/// Scaffold one `pages:` entry for `kind`, at `slug`, titled `title`,
/// indented to sit directly under a manifest's `pages:` key.
///
/// The lawyer lens gets the kind's spine plus the universal two; the client
/// lens gets only the universal two. That default is deliberate — a client
/// face starts with nothing firm-side on it and the attorney adds what the
/// client should see, rather than starting from everything and
/// remembering to remove.
///
/// Returns `None` for a kind that is not a page kind.
#[must_use]
pub fn scaffold(kind: Kind, slug: &str, title: &str) -> Option<String> {
    let skeleton = skeleton(kind)?;
    let lawyer: Vec<&str> = skeleton
        .required
        .iter()
        .chain(UNIVERSAL.iter())
        .map(|s| s.as_str())
        .collect();
    let client: Vec<&str> = UNIVERSAL.iter().map(|s| s.as_str()).collect();

    let mut out = String::new();
    // Writing to a `String` is infallible, so the result is discarded
    // rather than propagated — the same idiom the rest of the workspace
    // uses for `write!` into an in-memory buffer.
    let _ = write!(
        &mut out,
        "  - slug: {slug}\n    kind: {}\n    title: {title}\n    lenses:\n      lawyer: [{}]\n      \
         client: [{}]\n",
        kind.as_str(),
        lawyer.join(", "),
        client.join(", "),
    );
    Some(out)
}

/// Every section name, for a diagnostic that has to spell the vocabulary
/// out.
#[must_use]
pub fn section_names() -> Vec<&'static str> {
    Section::ALL.iter().map(|s| s.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::{catalog, scaffold, skeleton, Family, Lens, Section, UNIVERSAL};
    use crate::kind::Kind;

    /// Every dashboard kind, taken from the enum rather than re-listed, so
    /// a new kind joins these tests automatically.
    fn dashboard_kinds() -> Vec<Kind> {
        Kind::ALL
            .iter()
            .copied()
            .filter(|k| k.is_dashboard())
            .collect()
    }

    #[test]
    fn every_dashboard_kind_has_a_skeleton_and_no_other_kind_does() {
        for kind in Kind::ALL {
            assert_eq!(
                skeleton(*kind).is_some(),
                kind.is_dashboard(),
                "{} skeleton/is_dashboard disagree",
                kind.as_str(),
            );
        }
        assert_eq!(dashboard_kinds().len(), 12, "the catalog is twelve kinds");
    }

    #[test]
    fn every_skeleton_has_a_spine() {
        // A kind with no required section is not a skeleton, it is a
        // blank file with a name.
        for kind in dashboard_kinds() {
            let s = skeleton(kind).unwrap();
            assert!(
                !s.required.is_empty(),
                "{} declares no required section",
                kind.as_str(),
            );
        }
    }

    #[test]
    fn the_universal_sections_are_in_every_catalog() {
        for kind in dashboard_kinds() {
            let catalog = catalog(kind);
            for section in UNIVERSAL {
                assert!(
                    catalog.contains(section),
                    "{} catalog is missing {}",
                    kind.as_str(),
                    section.as_str(),
                );
            }
        }
    }

    #[test]
    fn a_catalog_never_repeats_a_section() {
        // `required` and `optional` are hand-written; a section listed in
        // both would make the allowlist ambiguous.
        for kind in dashboard_kinds() {
            let s = skeleton(kind).unwrap();
            for section in s.required {
                assert!(
                    !s.optional.contains(section),
                    "{} lists {} as both required and optional",
                    kind.as_str(),
                    section.as_str(),
                );
                assert!(
                    !UNIVERSAL.contains(section),
                    "{} re-declares the universal section {}",
                    kind.as_str(),
                    section.as_str(),
                );
            }
        }
    }

    #[test]
    fn the_corpus_splits_into_two_families() {
        assert_eq!(
            skeleton(Kind::DeliverablePackage).unwrap().family,
            Family::Deliverable,
        );
        assert_eq!(
            skeleton(Kind::ReviewQueueWorkbench).unwrap().family,
            Family::Dashboard,
        );
    }

    #[test]
    fn section_round_trips_and_rejects_unknown_names() {
        for section in Section::ALL {
            assert_eq!(Section::parse(section.as_str()), Some(*section));
            assert!(!section.describe().is_empty());
        }
        assert_eq!(Section::parse("timeline_of_vibes"), None);
        assert_eq!(Section::parse(""), None);
    }

    #[test]
    fn section_names_are_unique() {
        let mut names: Vec<&str> = Section::ALL.iter().map(|s| s.as_str()).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "two sections share a name");
    }

    #[test]
    fn lens_round_trips_and_rejects_unknown_names() {
        for lens in Lens::ALL {
            assert_eq!(Lens::parse(lens.as_str()), Some(*lens));
        }
        assert_eq!(Lens::parse("partner"), None);
    }

    #[test]
    fn a_scaffold_declares_the_spine_on_lawyer_and_only_the_universal_on_client() {
        let out = scaffold(Kind::MatterStatusConsole, "status", "Status").unwrap();
        let manifest = format!("pages:\n{out}");
        let document: serde_yaml::Value = serde_yaml::from_str(&manifest).unwrap();
        let pages = crate::pages::entries(document.as_mapping().unwrap()).unwrap();
        let lawyer = pages[0].sections_for(Lens::Lawyer).unwrap();
        let client = pages[0].sections_for(Lens::Client).unwrap();

        assert!(lawyer.contains(&Section::MetricStrip));
        assert!(lawyer.contains(&Section::DecisionQueue));
        // The client face starts with nothing firm-side on it.
        assert!(!client.contains(&Section::DecisionQueue));
        assert_eq!(client, UNIVERSAL.to_vec());
    }

    #[test]
    fn the_litigation_board_carries_its_case_sections_on_any_lens() {
        let board = skeleton(Kind::DocketDeadlineBoard).unwrap();
        for section in [
            Section::CaseCaption,
            Section::FactStrip,
            Section::DocketTimeline,
            Section::PartyRoster,
            Section::EfilingReceipts,
        ] {
            assert!(board.catalog().contains(&section), "{}", section.as_str());
            // Each loader scopes its rows to the reader, so a client face may
            // carry the timeline; it shows only what the client may see.
            assert!(board.permits(Lens::Client, section), "{}", section.as_str());
        }
        assert_eq!(
            Section::parse("docket_timeline"),
            Some(Section::DocketTimeline)
        );
        assert!(skeleton(Kind::HearingConsole)
            .unwrap()
            .catalog()
            .contains(&Section::CaseCaption));
    }

    #[test]
    fn the_notation_sections_reach_the_client_lens_of_their_kinds() {
        let workbench = skeleton(Kind::DocumentWorkbench).unwrap();
        for section in [
            Section::NotationDocument,
            Section::NotationList,
            Section::NotationChecklist,
        ] {
            assert!(
                workbench.catalog().contains(&section),
                "{}",
                section.as_str()
            );
            // The loader scopes each Notation to the reader, so a client face
            // may carry all three.
            assert!(
                workbench.permits(Lens::Client, section),
                "{}",
                section.as_str()
            );
        }
        // The client lens of a status console is Notations first: the list,
        // the checklist, and the one decision waiting on the client.
        let console = skeleton(Kind::MatterStatusConsole).unwrap();
        for section in [
            Section::NotationList,
            Section::NotationChecklist,
            Section::DecisionQueue,
        ] {
            assert!(console.catalog().contains(&section), "{}", section.as_str());
            assert!(
                console.permits(Lens::Client, section),
                "{}",
                section.as_str()
            );
        }
        assert!(skeleton(Kind::DeliverablePackage)
            .unwrap()
            .catalog()
            .contains(&Section::NotationDocument));
        assert_eq!(
            Section::parse("notation_document"),
            Some(Section::NotationDocument)
        );
    }

    #[test]
    fn the_relationship_graph_is_a_lawyer_only_option_on_the_litigation_kinds() {
        assert_eq!(
            Section::parse("relationship_graph"),
            Some(Section::RelationshipGraph)
        );
        for kind in [Kind::DocketDeadlineBoard, Kind::HearingConsole] {
            let s = skeleton(kind).unwrap();
            assert!(s.catalog().contains(&Section::RelationshipGraph));
            assert!(s.permits(Lens::Lawyer, Section::RelationshipGraph));
            assert!(!s.permits(Lens::Client, Section::RelationshipGraph));
        }
    }

    #[test]
    fn the_estate_console_carries_its_instruments_on_any_lens() {
        let console = skeleton(Kind::EstatePlanConsole).unwrap();
        assert_eq!(console.required, [Section::InstrumentSet]);
        for section in [
            Section::InstrumentSet,
            Section::SigningCeremony,
            Section::DistributionPlan,
            Section::AdministrationSteps,
            Section::FiduciaryRoster,
            Section::AssetSchedule,
        ] {
            assert!(console.catalog().contains(&section), "{}", section.as_str());
            assert!(
                console.permits(Lens::Client, section),
                "{}",
                section.as_str()
            );
        }
        assert!(skeleton(Kind::MatterStatusConsole)
            .unwrap()
            .catalog()
            .contains(&Section::CapTable));
        // Probate reuses the stages on the litigation board.
        assert!(skeleton(Kind::DocketDeadlineBoard)
            .unwrap()
            .catalog()
            .contains(&Section::AdministrationSteps));
    }

    #[test]
    fn the_closing_room_keeps_the_clause_comparison_on_the_lawyer_lens() {
        let room = skeleton(Kind::ClosingRoom).unwrap();
        assert_eq!(room.required, [Section::ClosingChecklist]);
        assert!(!room.permits(Lens::Client, Section::TermComparison));
        for section in [
            Section::SignatureTracker,
            Section::TermStrip,
            Section::OpenTerms,
            Section::RequirementTracker,
            Section::RedlineView,
            Section::CapTable,
        ] {
            assert!(room.catalog().contains(&section), "{}", section.as_str());
            assert!(room.permits(Lens::Client, section), "{}", section.as_str());
        }
    }

    #[test]
    fn scaffold_returns_none_for_a_non_page_kind() {
        assert!(scaffold(Kind::Onboarding, "nope", "Nope").is_none());
        assert!(catalog(Kind::Onboarding).is_empty());
    }

    #[test]
    fn every_lawyer_only_section_is_in_its_catalog_and_not_universal() {
        for kind in dashboard_kinds() {
            let s = skeleton(kind).unwrap();
            for section in s.lawyer_only {
                assert!(
                    s.catalog().contains(section),
                    "{} marks {} lawyer-only outside its catalog",
                    kind.as_str(),
                    section.as_str(),
                );
                assert!(!UNIVERSAL.contains(section), "{}", kind.as_str());
                assert!(!s.permits(Lens::Client, *section));
                assert!(!s.permits(Lens::Clerk, *section));
                assert!(s.permits(Lens::Lawyer, *section));
            }
        }
    }
}
