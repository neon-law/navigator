//! `navigator project pages init` — propose a Project's `pages:` block.
//!
//! Run in a Project checkout, the verb reads three things and composes the
//! [Matter Pages](../../../docs/glossary/matter-page.md) they suggest:
//!
//! - the pinned Project Skills (`skills:`): a practice area names the pages
//!   its matters are worked in, such as a deadline board and a discovery
//!   cockpit for litigation;
//! - the Drafts under `drafts/` (or `templates/`): an engagement Draft
//!   suggests the engagement and billing records page, with the Draft as
//!   its `draft:`;
//! - with a stored login for the manifest's host, the live matter's data:
//!   each source `Y023` can count that holds rows suggests the sections that
//!   render it. Without a login this input is skipped silently.
//!
//! Every page is composed from its kind's skeleton in [`rules::dashboard`]:
//! the lawyer lens carries the chosen sections, the client lens only the
//! ones the skeleton permits a client to see, and both carry the universal
//! `boundary_note` and `provenance_statement`. The block is then held to
//! the same `Y021` shape check and D-rules a hand-written `pages:` block
//! meets, so a proposal can never be one the gate refuses.
//!
//! The default prints the block. `--write` splices it into
//! `navigator.yaml` in place of nothing, leaving every other line as it
//! was; replacing an existing `pages:` needs `--force`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;
use rules::dashboard::{skeleton, Lens, Section, UNIVERSAL};
use rules::kind::Kind;

use super::manifest::{self, Manifest};
use crate::remote::{self, MatchingSession};

/// `navigator project pages …`
#[derive(Subcommand)]
pub enum PagesCmd {
    /// Propose a `pages:` block from the pinned Project Skills, the Drafts
    /// under `drafts/`, and (with a stored login) the live matter's data.
    /// Prints the block; `--write` puts it in `navigator.yaml`.
    Init {
        /// Project repository root. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Insert the block into `navigator.yaml`, keeping every other line.
        #[arg(long)]
        write: bool,
        /// With `--write`, replace a `pages:` block the manifest already has.
        #[arg(long, requires = "write")]
        force: bool,
    },
}

/// Run one `navigator project pages` verb.
pub async fn run(cmd: PagesCmd) -> ExitCode {
    match cmd {
        PagesCmd::Init { dir, write, force } => match init(&dir, write, force).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("navigator project pages init: {message}");
                ExitCode::from(1)
            }
        },
    }
}

async fn init(dir: &Path, write: bool, force: bool) -> Result<(), String> {
    let path = dir.join(manifest::FILE);
    let contents = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "cannot read {}: {error}; run from a Project checkout root",
            manifest::FILE
        )
    })?;
    let parsed = manifest::parse(&contents)?;
    let practice_areas: Vec<String> = parsed
        .skills
        .iter()
        .map(|pin| pin.practice_area.clone())
        .collect();
    let drafts = scan_drafts(dir);
    let populated = live_sections(&parsed).await;
    let pages = propose(&practice_areas, &drafts, &populated);
    if pages.is_empty() {
        return Err(
            "nothing to propose: no pinned Project Skill, engagement Draft, or live data \
             suggests a page; declare one by hand (see `navigator glossary show \"Matter Page\"`)"
                .to_string(),
        );
    }
    let block = render(&pages);
    validate(&block)?;
    if !write {
        print!("{block}");
        if has_pages(&contents) {
            eprintln!(
                "{} already declares `pages:`; `--write --force` replaces it with the block above",
                manifest::FILE
            );
        }
        return Ok(());
    }
    let updated = apply(&contents, &block, force)?;
    std::fs::write(&path, updated)
        .map_err(|error| format!("cannot write {}: {error}", manifest::FILE))?;
    println!(
        "wrote {} page(s) to {}; run `navigator project gate` before committing",
        pages.len(),
        manifest::FILE
    );
    Ok(())
}

/// One page the proposal declares: its kind, where it lives, and the
/// sections beyond the universal two, in render order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedPage {
    pub slug: String,
    pub kind: Kind,
    pub title: String,
    pub draft: Option<String>,
    pub sections: Vec<Section>,
}

/// A Draft file found under `drafts/` (or `templates/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftFile {
    /// Repository-relative path, `/`-separated.
    pub path: String,
    /// The frontmatter `kind:`, when it declares one.
    pub kind: Option<String>,
}

impl DraftFile {
    /// Whether this Draft is a matter's engagement: an `onboarding` Draft,
    /// or one whose file name says engagement.
    fn is_engagement(&self) -> bool {
        let stem = self.path.rsplit('/').next().unwrap_or(&self.path);
        self.kind.as_deref() == Some(Kind::Onboarding.as_str())
            || stem.to_ascii_lowercase().contains("engagement")
    }
}

/// A page a practice area suggests.
struct Seed {
    kind: Kind,
    slug: &'static str,
    title: &'static str,
}

const LITIGATION: &[Seed] = &[
    Seed {
        kind: Kind::DocketDeadlineBoard,
        slug: "deadlines",
        title: "Deadlines",
    },
    Seed {
        kind: Kind::DiscoveryCockpit,
        slug: "discovery",
        title: "Discovery",
    },
];

const PROBATE: &[Seed] = &[
    Seed {
        kind: Kind::DocketDeadlineBoard,
        slug: "deadlines",
        title: "Deadlines",
    },
    Seed {
        kind: Kind::DeliverablePackage,
        slug: "estate-documents",
        title: "Estate documents",
    },
];

const ESTATES: &[Seed] = &[Seed {
    kind: Kind::EstatePlanConsole,
    slug: "estate-plan",
    title: "Estate plan",
}];

const TRANSACTIONAL: &[Seed] = &[Seed {
    kind: Kind::ClosingRoom,
    slug: "closing",
    title: "Closing",
}];

/// The pages a pinned practice area suggests, matched on the area's slug.
fn practice_seeds(practice_area: &str) -> &'static [Seed] {
    let area = practice_area.trim().to_ascii_lowercase().replace('-', "_");
    if area.contains("litigation") {
        LITIGATION
    } else if [
        "corporate",
        "transaction",
        "financing",
        "m_a",
        "mergers",
        "business",
        "contract",
    ]
    .iter()
    .any(|word| area.contains(word))
    {
        TRANSACTIONAL
    } else if area.contains("probate") {
        PROBATE
    } else if area.contains("estate") || area == "trusts" || area == "wills" {
        ESTATES
    } else {
        &[]
    }
}

/// The slug and title a page of `kind` takes when only live data suggests
/// it. `None` for a kind that is not a page kind.
fn default_seed(kind: Kind) -> Option<(&'static str, &'static str)> {
    skeleton(kind)?;
    Some(match kind {
        Kind::DocketDeadlineBoard => ("deadlines", "Deadlines"),
        Kind::DiscoveryCockpit => ("discovery", "Discovery"),
        Kind::EngagementBillingRecords => ("engagement", "Engagement and billing"),
        Kind::DeliverablePackage => ("deliverables", "Deliverables"),
        Kind::ReviewQueueWorkbench => ("review-queue", "Review queue"),
        Kind::VerifierSplitView => ("verification", "Verification"),
        Kind::MatterStatusConsole => ("status", "Matter status"),
        Kind::DocumentWorkbench => ("drafting", "Drafting"),
        Kind::AuthorityLibrary => ("authorities", "Authorities"),
        Kind::HearingConsole => ("hearing", "Hearing"),
        Kind::EstatePlanConsole => ("estate-plan", "Estate plan"),
        Kind::ClosingRoom => ("closing", "Closing"),
        _ => return None,
    })
}

/// Compose the proposed pages: the pinned practice areas' pages first, an
/// engagement page for the first engagement Draft, then whatever the live
/// matter's populated sources add. One page per kind; the first source to
/// suggest a kind names it.
#[must_use]
pub fn propose(
    practice_areas: &[String],
    drafts: &[DraftFile],
    populated: &[&str],
) -> Vec<ProposedPage> {
    let mut pages: Vec<ProposedPage> = Vec::new();
    for area in practice_areas {
        for seed in practice_seeds(area) {
            push_page(&mut pages, seed.kind, seed.slug, seed.title, None);
        }
    }
    if let Some(draft) = drafts.iter().find(|draft| draft.is_engagement()) {
        let kind = Kind::EngagementBillingRecords;
        if let Some(page) = pages.iter_mut().find(|page| page.kind == kind) {
            page.draft.get_or_insert_with(|| draft.path.clone());
        } else if let Some((slug, title)) = default_seed(kind) {
            push_page(&mut pages, kind, slug, title, Some(draft.path.clone()));
        }
    }
    let live: Vec<Section> = populated.iter().filter_map(|s| Section::parse(s)).collect();
    // A populated section that is some kind's spine suggests that page.
    for section in &live {
        if pages.iter().any(|page| catalogs(page.kind, *section)) {
            continue;
        }
        let spine = Kind::ALL.iter().copied().find(|kind| {
            skeleton(*kind).is_some_and(|s| s.required.contains(section))
                && !pages.iter().any(|page| page.kind == *kind)
        });
        if let Some((kind, (slug, title))) = spine.and_then(|k| default_seed(k).map(|d| (k, d))) {
            push_page(&mut pages, kind, slug, title, None);
        }
    }
    // Every populated section joins each page whose kind catalogs it.
    for section in &live {
        for page in &mut pages {
            if catalogs(page.kind, *section)
                && !page.sections.contains(section)
                && !UNIVERSAL.contains(section)
            {
                page.sections.push(*section);
            }
        }
    }
    pages
}

fn catalogs(kind: Kind, section: Section) -> bool {
    skeleton(kind).is_some_and(|s| s.catalog().contains(&section))
}

fn push_page(
    pages: &mut Vec<ProposedPage>,
    kind: Kind,
    slug: &str,
    title: &str,
    draft: Option<String>,
) {
    let Some(skeleton) = skeleton(kind) else {
        return;
    };
    if pages.iter().any(|page| page.kind == kind) {
        return;
    }
    let mut unique = slug.to_string();
    let mut n = 2;
    while pages.iter().any(|page| page.slug == unique) {
        unique = format!("{slug}-{n}");
        n += 1;
    }
    pages.push(ProposedPage {
        slug: unique,
        kind,
        title: title.to_string(),
        draft,
        sections: skeleton.required.to_vec(),
    });
}

/// The lens lists for `page`: the lawyer sees every chosen section, the
/// client only those the kind's skeleton permits on the client lens, and
/// both end with the universal two.
fn lens_sections(page: &ProposedPage, lens: Lens) -> Vec<&'static str> {
    let skeleton = skeleton(page.kind);
    page.sections
        .iter()
        .copied()
        .filter(|section| skeleton.is_some_and(|s| s.permits(lens, *section)))
        .chain(UNIVERSAL.iter().copied())
        .map(Section::as_str)
        .collect()
}

/// Render `pages` as a top-level `pages:` block, in the flow-list style
/// [`rules::dashboard::scaffold`] writes.
#[must_use]
pub fn render(pages: &[ProposedPage]) -> String {
    use std::fmt::Write as _;
    let mut out = format!("{}:\n", rules::pages::KEY);
    for page in pages {
        let _ = write!(
            out,
            "  - slug: {}\n    kind: {}\n    title: {}\n",
            page.slug,
            page.kind.as_str(),
            page.title
        );
        if let Some(draft) = &page.draft {
            let _ = writeln!(out, "    draft: {draft}");
        }
        let _ = writeln!(out, "    lenses:");
        for lens in [Lens::Lawyer, Lens::Client] {
            let _ = writeln!(
                out,
                "      {}: [{}]",
                lens.as_str(),
                lens_sections(page, lens).join(", ")
            );
        }
    }
    out
}

/// Hold a `pages:` block to `Y021` and the D-rules, exactly as the gate
/// does for a hand-written one.
///
/// # Errors
///
/// Each finding, one per line.
pub fn validate(block: &str) -> Result<(), String> {
    let mut problems: Vec<String> = rules::pages::shape_problems(block)
        .into_iter()
        .map(|(line, message)| format!("{}:{line}: {message}", manifest::PAGES_CODE))
        .collect();
    let file = rules::SourceFile {
        path: PathBuf::from(manifest::FILE),
        contents: block.to_string(),
    };
    problems.extend(
        rules::lint_pages(&file)
            .into_iter()
            .map(|v| format!("{}:{}: {}", v.code, v.line, v.message)),
    );
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the proposed block fails the page rules:\n{}",
            problems.join("\n")
        ))
    }
}

/// The 0-based line range of the top-level `pages:` block, trailing blank
/// lines excluded, or `None` when the manifest has none.
fn pages_range(lines: &[&str]) -> Option<(usize, usize)> {
    let key = format!("{}:", rules::pages::KEY);
    let start = lines
        .iter()
        .position(|line| line.trim_end() == key || line.starts_with(&format!("{key} ")))?;
    let mut end = start + 1;
    for (idx, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with([' ', '\t', '-']) {
            break;
        }
        end = idx + 1;
    }
    Some((start, end))
}

fn has_pages(contents: &str) -> bool {
    pages_range(&contents.lines().collect::<Vec<_>>()).is_some()
}

/// Put `block` into a manifest's text: in place of an existing top-level
/// `pages:` block when `force` is set, otherwise appended. Every other line
/// is kept as written.
///
/// # Errors
///
/// An existing `pages:` without `force`, or a result the manifest parser
/// or the page rules refuse.
pub fn apply(contents: &str, block: &str, force: bool) -> Result<String, String> {
    let lines: Vec<&str> = contents.lines().collect();
    let updated = match pages_range(&lines) {
        Some(_) if !force => {
            return Err(format!(
                "{} already declares `pages:`; pass --force to replace it",
                manifest::FILE
            ))
        }
        Some((start, end)) => {
            let mut out = String::new();
            for line in &lines[..start] {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(block);
            for line in &lines[end..] {
                out.push_str(line);
                out.push('\n');
            }
            out
        }
        None => {
            let mut out = contents.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(block);
            out
        }
    };
    manifest::parse(&updated)?;
    validate(&updated)?;
    Ok(updated)
}

/// The Drafts directly under `drafts/`, or `templates/` when the
/// repository uses the alias spelling, sorted by path.
fn scan_drafts(dir: &Path) -> Vec<DraftFile> {
    let mut out = Vec::new();
    for folder in ["drafts", "templates"] {
        let Ok(entries) = std::fs::read_dir(dir.join(folder)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") || !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let kind = std::fs::read_to_string(&path).ok().and_then(|text| {
                rules::frontmatter::extract(&text)
                    .and_then(|front| rules::frontmatter::field(front, "kind"))
            });
            out.push(DraftFile {
                path: format!("{folder}/{name}"),
                kind,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// With a stored, unexpired login for the manifest's host, the sections
/// whose live source holds rows; otherwise nothing, without a word.
async fn live_sections(parsed: &Manifest) -> Vec<&'static str> {
    let (Some(host), Some(code)) = (
        manifest::non_empty(parsed.host.as_deref()),
        parsed.project.as_deref(),
    ) else {
        return Vec::new();
    };
    let Ok(MatchingSession::Valid(base, token)) = remote::matching_stored_session(&host) else {
        return Vec::new();
    };
    let Ok(project_id) = super::surfaces::resolve_project_id(&base, &token, code).await else {
        return Vec::new();
    };
    super::gate::populated_sections(&base, &token, project_id).await
}

#[cfg(test)]
mod tests {
    use super::{apply, propose, render, scan_drafts, validate, DraftFile, ProposedPage};
    use crate::projects::manifest;
    use rules::dashboard::Section;
    use rules::kind::Kind;

    const NESTED: &str = concat!(
        "version: 26.10.6\nproject:\n  host: h.example.com\n  name: acme\n",
        "  lawyer_dri: lawyer@example.com\n  client_dri: client@example.com\n",
        "  private_slack_channel: not provisioned\n  private_notion_page: not provisioned\n",
    );

    fn kinds(pages: &[ProposedPage]) -> Vec<Kind> {
        pages.iter().map(|page| page.kind).collect()
    }

    fn areas(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    /// A full manifest carrying `block`, held to every manifest rule.
    fn assert_gate_clean(block: &str) {
        let yaml = format!(
            "{NESTED}skills:\n  - jurisdiction: NV\n    practice_area: litigation\n    version: \"1\"\n{block}"
        );
        let findings = manifest::lint_contents(std::path::Path::new(manifest::FILE), &yaml);
        assert!(findings.is_empty(), "{findings:?}\n{yaml}");
        let parsed = manifest::parse(&yaml).expect("the proposal parses");
        assert!(!parsed.pages.is_empty());
    }

    #[test]
    fn a_litigation_pin_proposes_a_deadline_board_and_a_discovery_cockpit() {
        let pages = propose(&areas(&["litigation"]), &[], &[]);
        assert_eq!(
            kinds(&pages),
            [Kind::DocketDeadlineBoard, Kind::DiscoveryCockpit]
        );
        let block = render(&pages);
        assert!(block.contains("slug: deadlines"), "{block}");
        assert!(block.contains("slug: discovery"), "{block}");
        validate(&block).expect("passes the page rules");
        assert_gate_clean(&block);
    }

    #[test]
    fn an_estate_pin_proposes_the_estate_plan_console() {
        let pages = propose(&areas(&["estates"]), &[], &[]);
        assert_eq!(kinds(&pages), [Kind::EstatePlanConsole]);
        assert_eq!(pages[0].slug, "estate-plan");
        assert_eq!(pages[0].sections, [Section::InstrumentSet]);
        let block = render(&pages);
        assert!(
            block.contains("client: [instrument_set, boundary_note, provenance_statement]"),
            "{block}"
        );
        let probate = propose(&areas(&["probate"]), &[], &[]);
        assert_eq!(
            kinds(&probate),
            [Kind::DocketDeadlineBoard, Kind::DeliverablePackage]
        );
        assert_gate_clean(&render(&pages));
        assert_gate_clean(&render(&probate));
    }

    #[test]
    fn a_corporate_pin_proposes_the_closing_room_without_the_firm_comparison_on_client() {
        let mut pages = propose(&areas(&["corporate"]), &[], &[]);
        assert_eq!(kinds(&pages), [Kind::ClosingRoom]);
        assert_eq!(pages[0].slug, "closing");
        pages[0].sections.push(Section::TermComparison);
        let block = render(&pages);
        assert!(
            block.contains(
                "lawyer: [closing_checklist, term_comparison, boundary_note, provenance_statement]"
            ),
            "{block}"
        );
        assert!(
            block.contains("client: [closing_checklist, boundary_note, provenance_statement]"),
            "{block}"
        );
        assert_gate_clean(&block);
    }

    #[test]
    fn an_engagement_draft_proposes_engagement_billing_records_with_its_draft() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("drafts")).unwrap();
        std::fs::write(
            dir.path().join("drafts/opening.md"),
            "---\ntitle: Opening\nkind: onboarding\n---\n\nBody.\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("drafts/notes.txt"), "not a draft").unwrap();
        let drafts = scan_drafts(dir.path());
        assert_eq!(
            drafts,
            [DraftFile {
                path: "drafts/opening.md".to_string(),
                kind: Some("onboarding".to_string()),
            }]
        );
        let pages = propose(&[], &drafts, &[]);
        assert_eq!(kinds(&pages), [Kind::EngagementBillingRecords]);
        assert_eq!(pages[0].draft.as_deref(), Some("drafts/opening.md"));
        let block = render(&pages);
        assert!(block.contains("draft: drafts/opening.md"), "{block}");
        // Prebill review is the firm's working surface; the client sees the
        // letter and invoice, and every lens carries the universal two.
        let named = DraftFile {
            path: "templates/engagement-letter.md".to_string(),
            kind: None,
        };
        let mut page = propose(&[], &[named], &[]).remove(0);
        page.sections.push(Section::PrebillReview);
        let block = render(&[page]);
        assert!(
            block.contains(
                "lawyer: [engagement_letter, invoice, prebill_review, boundary_note, provenance_statement]"
            ),
            "{block}"
        );
        assert!(
            block.contains(
                "client: [engagement_letter, invoice, boundary_note, provenance_statement]"
            ),
            "{block}"
        );
        assert_gate_clean(&block);
    }

    #[test]
    fn the_client_lens_drops_lawyer_only_sections() {
        let mut pages = propose(&areas(&["litigation"]), &[], &[]);
        pages[1].sections.push(Section::ItemStatusSetter);
        let block = render(&pages);
        assert!(
            block.contains(
                "lawyer: [discovery_pairs, item_status_setter, boundary_note, provenance_statement]"
            ),
            "{block}"
        );
        assert!(
            block.contains("client: [discovery_pairs, boundary_note, provenance_statement]"),
            "{block}"
        );
        assert_gate_clean(&block);
    }

    #[test]
    fn populated_live_sources_add_their_sections() {
        // Deadlines with rows suggest the board and its key-dates strip, and
        // Docket Entries its timeline;
        // Documents with rows add a download list where the kind takes one.
        // A name outside the section vocabulary is ignored.
        let populated = [
            "not_a_section",
            "docket_timeline",
            "deadline_board",
            "key_dates_strip",
            "download_list",
        ];
        let pages = propose(&areas(&["estates"]), &[], &populated);
        assert_eq!(
            kinds(&pages),
            [Kind::EstatePlanConsole, Kind::DocketDeadlineBoard]
        );
        assert_eq!(
            pages[1].sections,
            [
                Section::DeadlineBoard,
                Section::DocketTimeline,
                Section::KeyDatesStrip
            ]
        );
        assert!(pages[0].sections.contains(&Section::DownloadList));
        assert_gate_clean(&render(&pages));
        assert!(propose(&[], &[], &[]).is_empty());
    }

    #[test]
    fn write_appends_and_keeps_every_other_line() {
        let contents = format!(
            "{NESTED}dockets:\n  - \"A-26-000001-C\"\nallowed_hosts:\n  example.org: synthetic reason\n"
        );
        let block = render(&propose(&areas(&["litigation"]), &[], &[]));
        let updated = apply(&contents, &block, false).expect("appends");
        assert!(updated.starts_with(&contents), "{updated}");
        assert!(updated.ends_with(&block));
        let parsed = manifest::parse(&updated).unwrap();
        assert_eq!(parsed.project.as_deref(), Some("acme"));
        assert_eq!(parsed.dockets, ["A-26-000001-C"]);
        assert_eq!(parsed.pages.len(), 2);
    }

    #[test]
    fn an_existing_pages_block_is_refused_without_force_and_replaced_with_it() {
        let existing = "pages:\n  - slug: old\n    kind: authority_library\n    title: Old\n    \
lenses:\n      lawyer: [authority_table, boundary_note, provenance_statement]\n\n";
        let contents = format!("{NESTED}{existing}dockets:\n  - \"A-26-000001-C\"\n");
        let block = render(&propose(&areas(&["litigation"]), &[], &[]));
        let refusal = apply(&contents, &block, false).unwrap_err();
        assert!(refusal.contains("--force"), "{refusal}");
        let replaced = apply(&contents, &block, true).expect("replaces");
        assert!(!replaced.contains("slug: old"), "{replaced}");
        assert!(replaced.starts_with(NESTED));
        assert!(
            replaced.ends_with("\ndockets:\n  - \"A-26-000001-C\"\n"),
            "{replaced}"
        );
        let parsed = manifest::parse(&replaced).unwrap();
        assert_eq!(parsed.pages.len(), 2);
        assert_eq!(parsed.dockets.len(), 1);
    }
}
