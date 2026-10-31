//! Load each house brand's English marketing catalog and map it onto page types.
//!
//! The words live in `locales/en/<brand-key>/*.yaml`, and the sentences more
//! than one page or brand publishes live once in `locales/en/shared.yaml`.
//! This module is the only Rust that reads either: pick the directory for the
//! request's `BrandKey`, resolve `{shared:<key>}` against the shared catalog,
//! interpolate the brand placeholders, deserialize, and fill the few runtime
//! fields a YAML file cannot know (the day-rate photo URL, the CLI release
//! archives). Editing published copy is a YAML change.

use views::brand::BrandKey;
use views::locales::{
    interpolate, BandCopy, CardCopy, CopyRun, HeroCtaCopy, HeroLine, HomeCopy, LitigationCopy,
    MarketingPageCopy, PackageInstallCopy, PageSkin, Paragraph, PracticeLinkCopy, PracticeMark,
    PricingCardCopy, ProjectNetworkNodeCopy, ProvenanceLedgerRowCopy, ProvenanceMark,
    ProvenancePillarCopy, ProvenanceSectionCopy, ProvenanceStepCopy, ServiceSectionCopy, StepCopy,
    SummonsCopy, TransactionalCopy, VirtueCopy,
};
use webapp::components::DayRateBadge;
use webapp::lead_capture::LeadCaptureCopy;
use webapp::marketing_page::{
    Band, Card, Download, HeroCta, PackageInstall, PageContent, ProjectNetworkNode, Run, Step,
};

/// The shared copy catalog: a sentence several pages or brands publish,
/// authored once and referenced as `{shared:<key>}`.
const SHARED_CATALOG_YAML: &str = include_str!("../locales/en/shared.yaml");

const NEON_HOME_YAML: &str = include_str!("../locales/en/neon/home.yaml");
const NEON_TEAM_YAML: &str = include_str!("../locales/en/neon/team.yaml");
const NEON_LITIGATION_YAML: &str = include_str!("../locales/en/neon/litigation.yaml");
const NEON_FRACTIONAL_GC_YAML: &str = include_str!("../locales/en/neon/fractional-gc.yaml");
const NEON_NOTATIONS_YAML: &str = include_str!("../locales/en/neon/notations.yaml");
const NEON_LEGAL_AID_YAML: &str = include_str!("../locales/en/neon/legal-aid.yaml");
const NEON_NAVIGATOR_YAML: &str = include_str!("../locales/en/neon/navigator.yaml");
const NEON_SERVICES_YAML: &str = include_str!("../locales/en/neon/services.yaml");
/// `/delete-your-debt` — Neon's own gateway to the DeleteYourDebt.com
/// practice. The first of a family of Neon-only practice gateways; see
/// [`views::locales::KNOWN_PAGES`].
const NEON_GATEWAY_DELETE_YOUR_DEBT_YAML: &str =
    include_str!("../locales/en/neon/gateway-delete-your-debt.yaml");
/// `/delete-your-data` — Neon's own gateway to the DeleteYourData.com
/// practice. Same family as [`NEON_GATEWAY_DELETE_YOUR_DEBT_YAML`].
const NEON_GATEWAY_DELETE_YOUR_DATA_YAML: &str =
    include_str!("../locales/en/neon/gateway-delete-your-data.yaml");
/// Nautilus subscription and optional Vesta Death Check.
const NEON_NAUTILUS_YAML: &str = include_str!("../locales/en/neon/nautilus.yaml");
/// `/immigration` — Neon's own gateway to the Abhaya Immigration practice.
/// Same family as [`NEON_GATEWAY_DELETE_YOUR_DEBT_YAML`].
const NEON_GATEWAY_IMMIGRATION_YAML: &str =
    include_str!("../locales/en/neon/gateway-immigration.yaml");
const NEON_IMMIGRATION_FAMILY_GREEN_CARDS_YAML: &str =
    include_str!("../locales/en/neon/immigration-family-green-cards.yaml");
const NEON_IMMIGRATION_CITIZENSHIP_YAML: &str =
    include_str!("../locales/en/neon/immigration-citizenship.yaml");
const NEON_IMMIGRATION_EMPLOYMENT_VISAS_YAML: &str =
    include_str!("../locales/en/neon/immigration-employment-visas.yaml");
const NEON_IMMIGRATION_CONSULAR_PROCESSING_YAML: &str =
    include_str!("../locales/en/neon/immigration-consular-processing.yaml");
const NEON_IMMIGRATION_USCIS_NOTICES_YAML: &str =
    include_str!("../locales/en/neon/immigration-uscis-notices.yaml");
const NEON_IMMIGRATION_HUMANITARIAN_RELIEF_YAML: &str =
    include_str!("../locales/en/neon/immigration-humanitarian-relief.yaml");
const NEON_IMMIGRATION_REMOVAL_DEFENSE_YAML: &str =
    include_str!("../locales/en/neon/immigration-removal-defense.yaml");
const NEON_IMMIGRATION_FREE_SUPPORT_YAML: &str =
    include_str!("../locales/en/neon/immigration-free-support.yaml");
/// `/estate-planning` — Neon's own gateway to the Vesta Estate Planning
/// practice. Same family as [`NEON_GATEWAY_DELETE_YOUR_DEBT_YAML`].
const NEON_GATEWAY_ESTATE_PLANNING_YAML: &str =
    include_str!("../locales/en/neon/gateway-estate-planning.yaml");
/// `/accidents` — Neon's own gateway to the Misericordia Injury Law
/// practice. Same family as [`NEON_GATEWAY_DELETE_YOUR_DEBT_YAML`].
const NEON_GATEWAY_ACCIDENTS_YAML: &str = include_str!("../locales/en/neon/gateway-accidents.yaml");
/// `/divorce` — Neon's own gateway to the Daybridge Divorce Law practice.
/// Same family as [`NEON_GATEWAY_DELETE_YOUR_DEBT_YAML`].
const NEON_GATEWAY_DIVORCE_YAML: &str = include_str!("../locales/en/neon/gateway-divorce.yaml");
/// The firm's individual services as records. Only Neon publishes one; the
/// other house brands render `/services` without an individual-services band.
const NEON_SERVICES_CATALOG_YAML: &str = include_str!("../locales/en/neon/services-catalog.yaml");
const DELETE_YOUR_DATA_HOME_YAML: &str = include_str!("../locales/en/delete-your-data/home.yaml");
const VESTA_HOME_YAML: &str = include_str!("../locales/en/vesta/home.yaml");
const LAWYER_SHOOK_HOME_YAML: &str = include_str!("../locales/en/lawyer-shook/home.yaml");
const VESTA_SERVICES_YAML: &str = include_str!("../locales/en/vesta/services.yaml");
const MISERICORDIA_HOME_YAML: &str = include_str!("../locales/en/misericordia/home.yaml");
const MISERICORDIA_SERVICES_YAML: &str = include_str!("../locales/en/misericordia/services.yaml");
const ABHAYA_HOME_YAML: &str = include_str!("../locales/en/abhaya/home.yaml");
const ABHAYA_SERVICES_YAML: &str = include_str!("../locales/en/abhaya/services.yaml");
const ABHAYA_SERVICES_CATALOG_YAML: &str =
    include_str!("../locales/en/abhaya/services-catalog.yaml");
const DELETE_YOUR_DEBT_HOME_YAML: &str = include_str!("../locales/en/delete-your-debt/home.yaml");
const DELETE_YOUR_DEBT_SERVICES_YAML: &str =
    include_str!("../locales/en/delete-your-debt/services.yaml");
const SUMMONS_HOME_YAML: &str = include_str!("../locales/en/summons/home.yaml");
const SUMMONS_SERVICES_YAML: &str = include_str!("../locales/en/summons/services.yaml");
const CYBER_INJURY_SERVICES_YAML: &str =
    include_str!("../locales/en/cyber-injury-law/services.yaml");
const CYBER_INJURY_HOME_YAML: &str = include_str!("../locales/en/cyber-injury-law/home.yaml");
const AEGIS_HOME_YAML: &str = include_str!("../locales/en/aegis/home.yaml");
const AEGIS_SERVICES_YAML: &str = include_str!("../locales/en/aegis/services.yaml");
const GUIEB_SHOOK_HOME_YAML: &str = include_str!("../locales/en/guieb-shook/home.yaml");
const GROUNDWORK_HOME_YAML: &str = include_str!("../locales/en/groundwork/home.yaml");
const GROUNDWORK_SERVICES_YAML: &str = include_str!("../locales/en/groundwork/services.yaml");
const DAYBRIDGE_HOME_YAML: &str = include_str!("../locales/en/daybridge/home.yaml");
const DAYBRIDGE_SERVICES_YAML: &str = include_str!("../locales/en/daybridge/services.yaml");
// Death & Divorce keeps its home title to the brand name alone.
const DEATH_AND_DIVORCE_HOME_YAML: &str = include_str!("../locales/en/death-and-divorce/home.yaml");
const DEATH_AND_DIVORCE_SERVICES_YAML: &str =
    include_str!("../locales/en/death-and-divorce/services.yaml");
const DELETE_YOUR_DATA_SERVICES_YAML: &str =
    include_str!("../locales/en/delete-your-data/services.yaml");
const LAWYER_SHOOK_SERVICES_YAML: &str = include_str!("../locales/en/lawyer-shook/services.yaml");

/// The shipped shared catalog, parsed and validated once.
///
/// Every page load resolves references against it, so it is parsed on first
/// use and kept. `navigator project gate` (`Y002`) is the gate that keeps the
/// parse infallible, and the `shared_catalog_is_valid` test below proves it
/// in the Rust suite.
#[must_use]
pub fn shared_catalog() -> &'static views::locales::shared::SharedCatalog {
    static CATALOG: std::sync::OnceLock<views::locales::shared::SharedCatalog> =
        std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        views::locales::shared::SharedCatalog::parse(SHARED_CATALOG_YAML).expect(
            "invariant: locales/en/shared.yaml is valid; navigator project gate Y002 is the gate",
        )
    })
}

/// The shipped YAML for `key`'s `page` stem, if that brand publishes it.
#[must_use]
pub fn catalog_yaml(key: BrandKey, page: &str) -> Option<&'static str> {
    match (key, page) {
        (BrandKey::Aegis, "explainer-first-steps") => Some(include_str!(
            "../locales/en/aegis/explainer-first-steps.yaml"
        )),
        (BrandKey::Aegis, "explainer-process") => {
            Some(include_str!("../locales/en/aegis/explainer-process.yaml"))
        }
        (BrandKey::Aegis, "explainer-documents") => {
            Some(include_str!("../locales/en/aegis/explainer-documents.yaml"))
        }
        (BrandKey::Groundwork, "explainer-first-steps") => Some(include_str!(
            "../locales/en/groundwork/explainer-first-steps.yaml"
        )),
        (BrandKey::Groundwork, "explainer-process") => Some(include_str!(
            "../locales/en/groundwork/explainer-process.yaml"
        )),
        (BrandKey::Groundwork, "explainer-documents") => Some(include_str!(
            "../locales/en/groundwork/explainer-documents.yaml"
        )),
        (BrandKey::Neon, "home") => Some(NEON_HOME_YAML),
        (BrandKey::Neon, "team") => Some(NEON_TEAM_YAML),
        (BrandKey::Vesta, "home") => Some(VESTA_HOME_YAML),
        (BrandKey::Vesta, "explainer-probate") => {
            Some(include_str!("../locales/en/vesta/explainer-probate.yaml"))
        }
        (BrandKey::LawyerShook, "home") => Some(LAWYER_SHOOK_HOME_YAML),
        (BrandKey::Vesta, "services") => Some(VESTA_SERVICES_YAML),
        (BrandKey::Misericordia, "home") => Some(MISERICORDIA_HOME_YAML),
        (BrandKey::Misericordia, "services") => Some(MISERICORDIA_SERVICES_YAML),
        (BrandKey::Abhaya, "home") => Some(ABHAYA_HOME_YAML),
        (BrandKey::Abhaya, "services") => Some(ABHAYA_SERVICES_YAML),
        (BrandKey::Abhaya, views::locales::services::SERVICES_CATALOG_STEM) => {
            Some(ABHAYA_SERVICES_CATALOG_YAML)
        }
        (BrandKey::DeleteYourDebt, "home") => Some(DELETE_YOUR_DEBT_HOME_YAML),
        (BrandKey::DeleteYourDebt, "services") => Some(DELETE_YOUR_DEBT_SERVICES_YAML),
        (BrandKey::Summons, "home") => Some(SUMMONS_HOME_YAML),
        (BrandKey::Summons, "services") => Some(SUMMONS_SERVICES_YAML),
        (BrandKey::CyberInjuryLaw, "services") => Some(CYBER_INJURY_SERVICES_YAML),
        (BrandKey::CyberInjuryLaw, "home") => Some(CYBER_INJURY_HOME_YAML),
        (BrandKey::Aegis, "home") => Some(AEGIS_HOME_YAML),
        (BrandKey::Aegis, "services") => Some(AEGIS_SERVICES_YAML),
        (BrandKey::Groundwork, "home") => Some(GROUNDWORK_HOME_YAML),
        (BrandKey::GuiebShook, "home") => Some(GUIEB_SHOOK_HOME_YAML),
        (BrandKey::GuiebShook, "services") => {
            Some(include_str!("../locales/en/guieb-shook/services.yaml"))
        }
        (BrandKey::Groundwork, "services") => Some(GROUNDWORK_SERVICES_YAML),
        (BrandKey::Daybridge, "home") => Some(DAYBRIDGE_HOME_YAML),
        (BrandKey::Daybridge, "services") => Some(DAYBRIDGE_SERVICES_YAML),
        (BrandKey::DeathAndDivorce, "home") => Some(DEATH_AND_DIVORCE_HOME_YAML),
        (BrandKey::DeathAndDivorce, "services") => Some(DEATH_AND_DIVORCE_SERVICES_YAML),
        (BrandKey::Neon, "litigation") => Some(NEON_LITIGATION_YAML),
        (BrandKey::Neon, "fractional-gc") => Some(NEON_FRACTIONAL_GC_YAML),
        (BrandKey::Neon, "notations") => Some(NEON_NOTATIONS_YAML),
        (BrandKey::Neon, "navigator") => Some(NEON_NAVIGATOR_YAML),
        (BrandKey::Neon, "legal-aid") => Some(NEON_LEGAL_AID_YAML),
        (BrandKey::Neon, "nautilus") => Some(NEON_NAUTILUS_YAML),
        (BrandKey::Neon, "services") => Some(NEON_SERVICES_YAML),
        (BrandKey::Neon, "gateway-delete-your-debt") => Some(NEON_GATEWAY_DELETE_YOUR_DEBT_YAML),
        (BrandKey::Neon, "gateway-delete-your-data") => Some(NEON_GATEWAY_DELETE_YOUR_DATA_YAML),
        (BrandKey::Neon, "gateway-immigration") => Some(NEON_GATEWAY_IMMIGRATION_YAML),
        (BrandKey::Neon, "immigration-family-green-cards") => {
            Some(NEON_IMMIGRATION_FAMILY_GREEN_CARDS_YAML)
        }
        (BrandKey::Neon, "immigration-citizenship") => Some(NEON_IMMIGRATION_CITIZENSHIP_YAML),
        (BrandKey::Neon, "immigration-employment-visas") => {
            Some(NEON_IMMIGRATION_EMPLOYMENT_VISAS_YAML)
        }
        (BrandKey::Neon, "immigration-consular-processing") => {
            Some(NEON_IMMIGRATION_CONSULAR_PROCESSING_YAML)
        }
        (BrandKey::Neon, "immigration-uscis-notices") => Some(NEON_IMMIGRATION_USCIS_NOTICES_YAML),
        (BrandKey::Neon, "immigration-humanitarian-relief") => {
            Some(NEON_IMMIGRATION_HUMANITARIAN_RELIEF_YAML)
        }
        (BrandKey::Neon, "immigration-removal-defense") => {
            Some(NEON_IMMIGRATION_REMOVAL_DEFENSE_YAML)
        }
        (BrandKey::Neon, "immigration-free-support") => Some(NEON_IMMIGRATION_FREE_SUPPORT_YAML),
        (BrandKey::Neon, "gateway-estate-planning") => Some(NEON_GATEWAY_ESTATE_PLANNING_YAML),
        (BrandKey::Neon, "gateway-accidents") => Some(NEON_GATEWAY_ACCIDENTS_YAML),
        (BrandKey::Neon, "gateway-divorce") => Some(NEON_GATEWAY_DIVORCE_YAML),
        (BrandKey::Neon, views::locales::services::SERVICES_CATALOG_STEM) => {
            Some(NEON_SERVICES_CATALOG_YAML)
        }
        (BrandKey::DeleteYourData, "home") => Some(DELETE_YOUR_DATA_HOME_YAML),
        (BrandKey::DeleteYourData, "services") => Some(DELETE_YOUR_DATA_SERVICES_YAML),
        (BrandKey::LawyerShook, "services") => Some(LAWYER_SHOOK_SERVICES_YAML),
        _ => None,
    }
}

/// The public team directory and Nick's profile, authored in Neon's
/// `team.yaml` catalog.
pub fn team(branding: &views::brand::Branding) -> views::locales::TeamCopy {
    let raw = catalog_yaml(branding.brand_key, "team")
        .expect("invariant: the Neon brand publishes its team catalog");
    let interpolated = interpolate(raw, branding.firm.site_name, branding.firm_email);
    serde_yaml::from_str(&interpolated)
        .expect("invariant: navigator project gate validates the team catalog")
}

/// Load one catalog file as `T`, after resolving shared copy and the mounted
/// brand.
///
/// Shared references resolve first, so a shared sentence may itself carry a
/// brand placeholder and still be filled for the brand that mounted it.
fn load<T: serde::de::DeserializeOwned>(yaml: &str, branding: &views::brand::Branding) -> T {
    let shared = views::locales::shared::resolve_references(
        yaml,
        shared_catalog(),
        branding.brand_key.as_str(),
    )
    .expect("invariant: every `{shared:…}` reference resolves; the catalog test is the gate");
    let raw = interpolate(&shared, branding.firm.site_name, branding.firm_email);
    serde_yaml::from_str(&raw).expect(
        "invariant: shipped locale YAML deserializes; navigator project gate Y002 is the gate",
    )
}

/// The brand's individual-services schedule, if it publishes one.
///
/// Resolved per brand rather than parsed once, because a catalog value may
/// carry the brand placeholders the way page copy does. Routers are built at
/// startup, so this runs a handful of times per process.
#[must_use]
pub fn services_catalog(
    branding: &views::brand::Branding,
) -> Option<views::locales::services::ServicesCatalog> {
    let yaml = catalog_yaml(
        branding.brand_key,
        views::locales::services::SERVICES_CATALOG_STEM,
    )?;
    let shared = views::locales::shared::resolve_references(
        yaml,
        shared_catalog(),
        branding.brand_key.as_str(),
    )
    .expect("invariant: every `{shared:…}` reference resolves; the catalog test is the gate");
    let raw = interpolate(&shared, branding.firm.site_name, branding.firm_email);
    Some(
        views::locales::services::ServicesCatalog::parse(&raw).expect(
            "invariant: the shipped services catalog is valid; navigator project gate Y002 is the gate",
        ),
    )
}

fn load_page<T: serde::de::DeserializeOwned>(branding: &views::brand::Branding, page: &str) -> T {
    let yaml = catalog_yaml(branding.brand_key, page).unwrap_or_else(|| {
        panic!(
            "invariant: {} publishes `{page}`; BrandKey::catalog_pages is the gate",
            branding.brand_key.as_str()
        )
    });
    load(yaml, branding)
}

/// Split a hero statement into words, marking the first `accent_words` of them.
fn hero_words(line: &HeroLine) -> Vec<webapp::litigation_page::HeroWord> {
    line.text
        .split_whitespace()
        .enumerate()
        .map(|(index, text)| webapp::litigation_page::HeroWord {
            text: text.to_string(),
            accent: index < line.accent_words,
        })
        .collect()
}

fn copy_run_to_home(run: CopyRun) -> webapp::home::CopyRun {
    webapp::home::CopyRun {
        text: run.text,
        emphasis: run.emphasis,
        href: run.href,
    }
}

fn copy_run_to_marketing(run: CopyRun) -> Run {
    Run {
        text: run.text,
        emphasis: run.emphasis,
        code: run.code,
        href: run.href,
    }
}

fn paragraphs_to_home(body: Vec<Paragraph>) -> Vec<Vec<webapp::home::CopyRun>> {
    body.into_iter()
        .map(|paragraph| paragraph.into_iter().map(copy_run_to_home).collect())
        .collect()
}

fn paragraphs_to_marketing(body: Vec<Paragraph>) -> Vec<webapp::marketing_page::Paragraph> {
    body.into_iter()
        .map(|paragraph| paragraph.into_iter().map(copy_run_to_marketing).collect())
        .collect()
}

fn provenance_mark(mark: ProvenanceMark) -> webapp::home::ProvenanceMark {
    match mark {
        ProvenanceMark::Request => webapp::home::ProvenanceMark::Request,
        ProvenanceMark::Attorney => webapp::home::ProvenanceMark::Attorney,
        ProvenanceMark::Chain => webapp::home::ProvenanceMark::Chain,
    }
}

fn provenance_to_home(copy: ProvenanceSectionCopy) -> webapp::home::ProvenanceSection {
    let ProvenanceSectionCopy {
        overline,
        heading,
        heading_accent,
        lead,
        steps,
        ledger_heading,
        ledger_caption,
        ledger,
        pillars,
        notes,
    } = copy;
    webapp::home::ProvenanceSection {
        overline,
        heading,
        heading_accent,
        lead,
        steps: steps
            .into_iter()
            .map(
                |ProvenanceStepCopy {
                     mark,
                     label,
                     detail,
                 }| webapp::home::ProvenanceStep {
                    mark: provenance_mark(mark),
                    label,
                    detail,
                },
            )
            .collect(),
        ledger_heading,
        ledger_caption,
        ledger: ledger
            .into_iter()
            .map(
                |ProvenanceLedgerRowCopy { label, status }| webapp::home::ProvenanceLedgerRow {
                    label,
                    status,
                },
            )
            .collect(),
        pillars: pillars
            .into_iter()
            .map(
                |ProvenancePillarCopy { heading, body }| webapp::home::ProvenancePillar {
                    heading,
                    body,
                },
            )
            .collect(),
        notes: paragraphs_to_home(notes),
    }
}

fn practice_mark(mark: PracticeMark) -> webapp::components::PracticeMark {
    match mark {
        PracticeMark::Scales => webapp::components::PracticeMark::Scales,
        PracticeMark::Handshake => webapp::components::PracticeMark::Handshake,
        PracticeMark::Gavel => webapp::components::PracticeMark::Gavel,
        PracticeMark::Technology => webapp::components::PracticeMark::Technology,
        PracticeMark::Helm => webapp::components::PracticeMark::Helm,
    }
}

fn page_skin(skin: PageSkin) -> webapp::marketing_page::PageSkin {
    match skin {
        PageSkin::Marketing => webapp::marketing_page::PageSkin::Marketing,
        PageSkin::Practice => webapp::marketing_page::PageSkin::Practice,
        PageSkin::Nautilus => webapp::marketing_page::PageSkin::Nautilus,
    }
}

/// The photo `views::assets::asset_url` resolves for a published day rate,
/// if this amount has one. `webapp` cannot resolve this itself — it also
/// compiles for the browser, where the server-only `views` crate is not
/// available — so this is the one place a whole-dollar amount becomes the
/// badge the page actually renders. An amount with no photo (any value other
/// than the four denominations the firm has published so far) renders no
/// badge at all, same as no amount.
fn resolve_day_rate(amount: Option<u16>) -> Option<DayRateBadge> {
    let amount = amount?;
    let key = match amount {
        50 => "img/fifty-dollar-bill/fifty-dollar-bill.jpg",
        10 => "img/ten-dollar-bill/ten-dollar-bill.jpg",
        5 => "img/five-dollar-bill/five-dollar-bill.jpg",
        1 => "img/one-dollar-bill/one-dollar-bill.jpg",
        _ => return None,
    };
    Some(DayRateBadge {
        amount,
        image_src: views::assets::asset_url(key),
    })
}

fn card(copy: CardCopy) -> Card {
    let logo_href = copy
        .brand
        .as_deref()
        .and_then(views::brand::BrandKey::parse)
        // A brand mark is a tracked file the app serves from `/public`; it is
        // never uploaded to the assets bucket, so `asset_url` would 404.
        .map(|key| {
            key.resolve_branding(&views::brand::DEFAULT_BRANDING)
                .firm
                .logo_href
                .to_string()
        });
    Card {
        logo_href,
        title: copy.title,
        chips: copy.chips,
        body: paragraphs_to_marketing(copy.body),
        href: copy.href,
        href_label: copy.href_label,
        cadence: copy.cadence,
        features: copy.features,
        day_rate: resolve_day_rate(copy.day_rate_bill),
    }
}

fn step(copy: StepCopy) -> Step {
    Step {
        title: copy.title,
        body: paragraphs_to_marketing(copy.body),
        image: copy.image.map(|image| webapp::marketing_page::StepImage {
            src: views::assets::asset_url(&image.asset_key),
            alt: image.alt,
        }),
    }
}

fn network_node(copy: ProjectNetworkNodeCopy) -> ProjectNetworkNode {
    ProjectNetworkNode {
        label: copy.label,
        detail: copy.detail,
    }
}

fn network_label(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

fn package_install(copy: PackageInstallCopy) -> PackageInstall {
    PackageInstall {
        heading: copy.heading,
        body: paragraphs_to_marketing(copy.body),
        commands: vec![webapp::cli_release::HOMEBREW_INSTALL_COMMAND.to_string()],
    }
}

fn fill_downloads(
    anchor: String,
    overline: String,
    heading: String,
    description: Option<String>,
    archive_label: String,
    package: Option<PackageInstallCopy>,
) -> Band {
    let version = webapp::cli_release::release_version();
    Band::Downloads {
        anchor,
        overline,
        heading,
        description,
        version: version.clone(),
        archive_href: webapp::cli_release::RELEASES_HREF.to_string(),
        archive_label,
        items: webapp::cli_release::PLATFORMS
            .iter()
            .map(|platform| Download {
                platform: platform.slug.to_string(),
                label: platform.label.to_string(),
                detail: platform.detail.to_string(),
                filename: webapp::cli_release::asset_filename(&version, platform),
                href: webapp::cli_release::asset_href(&version, platform),
                mark: platform.mark,
            })
            .collect(),
        package: package.map(package_install),
    }
}

/// One service, resolved for rendering: the catalog's record with its fee and
/// category label filled in.
fn service(
    catalog: &views::locales::services::ServicesCatalog,
    record: &views::locales::services::ServiceCopy,
) -> webapp::services_search::Service {
    webapp::services_search::Service {
        id: record.id.clone(),
        item: record.item.clone(),
        name: record.name.clone(),
        blurb: record.blurb.clone(),
        category: catalog.category_label(record.category).to_string(),
        audience: record.category.search_aliases().to_string(),
        includes: record.includes.clone(),
        keywords: record.keywords.clone(),
        fee: catalog.fee(record).to_string(),
        period: record.period.clone(),
        members_only: record.members_only,
        state_fee: record.state_fee,
        package: catalog.package_quote(record).map(|quote| {
            webapp::services_search::ServicePackageQuote {
                members: quote.members,
            }
        }),
        plan_price: record
            .plan_price
            .as_ref()
            .map(|price| webapp::services_search::PlanPrice {
                amount: price.amount.clone(),
                plan: price.plan.clone(),
            }),
        template: record.template.clone(),
    }
}

/// The individual-services band: page copy from the catalog file's sibling
/// `services.yaml`, and the services themselves from the catalog.
///
/// Panics on any other band, which the one call site's `copy @` pattern makes
/// unreachable.
fn services_band(copy: BandCopy, catalog: &views::locales::services::ServicesCatalog) -> Band {
    let BandCopy::Services {
        anchor,
        overline,
        heading,
        description,
        search_label,
        search_placeholder,
        submit_label,
        examples,
        fee_label,
        includes_label,
        package_badge,
        package_members_label,
        members_badge,
        state_fee_badge,
        empty,
        empty_help,
        clear_label,
    } = copy
    else {
        unreachable!("services_band is called with a services band")
    };
    Band::Services(Box::new(webapp::services_search::ServicesBand {
        anchor,
        overline,
        heading,
        description,
        search_label,
        search_placeholder,
        submit_label,
        examples: examples
            .into_iter()
            .map(|example| webapp::services_search::SearchExample {
                label: example.label,
                query: example.query,
            })
            .collect(),
        fee_label,
        includes_label,
        package_badge,
        package_members_label,
        members_badge,
        state_fee_badge,
        empty,
        empty_help,
        clear_label,
        start_label: catalog.start.label.clone(),
        start_microcopy: catalog.start.microcopy.clone(),
        services: catalog
            .services
            .iter()
            .map(|record| service(catalog, record))
            .collect(),
    }))
}

fn band(copy: BandCopy, catalog: Option<&views::locales::services::ServicesCatalog>) -> Band {
    match copy {
        copy @ BandCopy::Enrollment { .. } => enrollment_band(copy),
        BandCopy::Statement {
            heading,
            lead,
            body,
        } => Band::Statement {
            heading,
            lead,
            body: paragraphs_to_marketing(body),
        },
        BandCopy::Cards {
            anchor,
            overline,
            heading,
            description,
            items,
            pricing_style,
        } => Band::Cards {
            anchor,
            overline,
            heading,
            description,
            items: items.into_iter().map(card).collect(),
            pricing_style,
        },
        BandCopy::Video {
            anchor,
            overline,
            heading,
            description,
            label,
            asset_key,
        } => Band::Video {
            anchor,
            overline,
            heading,
            description,
            label,
            src: views::assets::asset_url(&asset_key),
        },
        BandCopy::Steps {
            anchor,
            overline,
            heading,
            description,
            items,
        } => Band::Steps {
            anchor,
            overline,
            heading,
            description,
            items: items.into_iter().map(step).collect(),
        },
        copy @ BandCopy::ProjectNetwork { .. } => project_network_band(copy),
        BandCopy::Downloads {
            anchor,
            overline,
            heading,
            description,
            archive_label,
            package,
        } => fill_downloads(
            anchor,
            overline,
            heading,
            description,
            archive_label,
            package,
        ),
        copy @ BandCopy::Services { .. } => services_band(
            copy,
            catalog.expect(
                "invariant: a page carrying a `services` band publishes a services catalog; \
                 `the_services_band_only_ships_where_a_catalog_does` is the gate",
            ),
        ),
        copy @ BandCopy::AppPreview { .. } => app_preview_band(copy),
        BandCopy::Cta {
            heading,
            body,
            email,
            action,
        } => Band::Cta {
            heading,
            body,
            email,
            action: action.map(|HeroCtaCopy { href, label }| HeroCta { href, label }),
        },
    }
}

fn enrollment_band(copy: BandCopy) -> Band {
    let BandCopy::Enrollment {
        heading,
        body,
        steps,
        next_label,
        back_label,
        submit_label,
        booking_label,
        booking_body,
    } = copy
    else {
        unreachable!("enrollment helper receives only enrollment copy")
    };
    Band::Enrollment {
        heading,
        body,
        steps,
        next_label,
        back_label,
        submit_label,
        booking_label,
        booking_body,
        booking_href: String::new(),
    }
}

fn app_preview_band(copy: BandCopy) -> Band {
    let BandCopy::AppPreview {
        anchor,
        overline,
        heading,
        description,
        app_name,
        app_subtitle,
        price,
        install_label,
        checks_heading,
        checks,
        app_store_label,
        play_store_label,
        availability,
    } = copy
    else {
        unreachable!("app preview helper receives only app preview copy")
    };
    Band::AppPreview {
        anchor,
        overline,
        heading,
        description,
        app_name,
        app_subtitle,
        price,
        install_label,
        checks_heading,
        checks: checks
            .into_iter()
            .map(|check| webapp::marketing_page::AppCheck {
                title: check.title,
                detail: check.detail,
            })
            .collect(),
        app_store_label,
        play_store_label,
        availability,
    }
}

fn project_network_band(copy: BandCopy) -> Band {
    let BandCopy::ProjectNetwork {
        anchor,
        overline,
        heading,
        description,
        center_eyebrow,
        center_heading,
        center_detail,
        left_lane_label,
        right_lane_label,
        left,
        right,
        mcp_tools,
        agentic_coding_tools,
        saas_tools,
    } = copy
    else {
        unreachable!("project network helper receives only project network copy")
    };

    Band::ProjectNetwork {
        anchor,
        overline,
        heading,
        description,
        center_eyebrow: network_label(center_eyebrow, "The Project center"),
        center_heading: network_label(center_heading, "Navigator"),
        center_detail,
        left_lane_label: network_label(
            left_lane_label,
            "Project resources to the left of Navigator",
        ),
        right_lane_label: network_label(
            right_lane_label,
            "Project resources to the right of Navigator",
        ),
        left: left.into_iter().map(network_node).collect(),
        right: right.into_iter().map(network_node).collect(),
        mcp_tools,
        agentic_coding_tools,
        saas_tools,
    }
}

fn marketing_page(
    copy: MarketingPageCopy,
    catalog: Option<&views::locales::services::ServicesCatalog>,
    branding: &views::brand::Branding,
) -> PageContent {
    PageContent {
        head_title: copy.head_title,
        meta_description: copy.meta_description,
        og_description: copy.og_description,
        title: copy.title,
        hero_mark: copy.hero_mark.map(practice_mark),
        tagline: copy.tagline,
        hero_lines: copy.hero_lines.iter().map(hero_words).collect(),
        hero_lead: copy.hero_lead,
        hero_lead_runs: copy
            .hero_lead_runs
            .into_iter()
            .map(copy_run_to_marketing)
            .collect(),
        hero_cta: copy
            .hero_cta
            .map(|HeroCtaCopy { href, label }| HeroCta { href, label }),
        skin: page_skin(copy.skin),
        bands: copy
            .bands
            .into_iter()
            .map(|copy| band(copy, catalog))
            .collect(),
        lead_capture: lead_capture(branding),
    }
}

/// Resolve the shared consent language for the mounted brand. The marketing
/// page constructors replace this default with the request's brand copy below;
/// the helper is kept at the same catalog boundary as the page readers.
pub fn lead_capture(branding: &views::brand::Branding) -> LeadCaptureCopy {
    let copy = |key: &str| {
        let raw = shared_catalog()
            .lookup(branding.brand_key.as_str(), key)
            .unwrap_or_else(|| panic!("invariant: shared lead key `{key}` is present"));
        interpolate(raw, branding.firm.site_name, branding.firm_email)
    };
    LeadCaptureCopy {
        consent_sentence: copy("lead.consent"),
        phone_helper: copy("lead.phone_helper"),
        sms_label: copy("lead.sms_label"),
    }
}

/// The lifetime estate offer, mapped from the catalog onto the page type.
///
/// `webapp::home::EstateContent` mirrors `views::locales::EstateCopy` because
/// `views` is server-gated and the page type is compiled for the wasm client
/// too, so this is the seam that carries one onto the other.
fn estate_content(
    copy: views::locales::EstateCopy,
    deployment_host: Option<&str>,
) -> webapp::home::EstateContent {
    webapp::home::EstateContent {
        eyebrow: copy.eyebrow,
        process_link: copy.process_link,
        plan_label: copy.plan_label,
        price: copy.price,
        plan_description: copy.plan_description,
        features: copy.features,
        fee_note: copy.fee_note,
        nevada_label: copy.nevada_label,
        nevada_heading: copy.nevada_heading,
        nevada_body: copy.nevada_body,
        nevada_points: copy.nevada_points,
        nevada_note: copy.nevada_note,
        nevada_details_label: copy.nevada_details_label,
        administration_label: copy.administration_label,
        administration_heading: copy.administration_heading,
        administration_body: copy.administration_body,
        administration_services: copy.administration_services,
        administration_fee_label: copy.administration_fee_label,
        administration_fee_note: copy.administration_fee_note,
        administration_contact_label: copy.administration_contact_label,
        administration_details_label: copy.administration_details_label,
        video_updates_label: copy.video_updates_label,
        video_updates_body: copy.video_updates_body,
        video_updates_blog_label: copy.video_updates_blog_label,
        testimonials_label: copy.testimonials_label,
        video_label: copy.video_label,
        // The catalog authors the object key; the page needs the URL it
        // resolves to in this deployment.
        video_src: views::assets::asset_url(&copy.video_src),
        photos: views::assets::VESTA_PHOTOS
            .iter()
            .map(|(key, alt)| [views::assets::asset_url(key), (*alt).to_owned()])
            .collect(),
        astrology_label: copy.astrology_label,
        astrology_heading: copy.astrology_heading,
        astrology_body: copy.astrology_body,
        cosmos_label: copy.cosmos_label,
        cosmos_note: copy.cosmos_note,
        aster_name: copy.aster_name,
        aster_image: views::assets::asset_url(views::assets::ASTER_BOARD_KEY),
        aster_description: copy.aster_description,
        cosmos_themes: copy.cosmos_themes,
        probate_label: copy.probate_label,
        probate_heading: copy.probate_heading,
        probate_body: copy.probate_body,
        probate_details_label: copy.probate_details_label,
        process_label: copy.process_label,
        process_heading: copy.process_heading,
        steps: copy.steps,
        record_label: copy.record_label,
        record_unit: copy.record_unit,
        record_status: copy.record_status,
        record_heading: copy.record_heading,
        record_body: copy.record_body,
        record_note: copy.record_note,
        closing_heading: copy.closing_heading,
        closing_body: copy.closing_body,
        sibling_practices: sibling_practices(copy.sibling_practices, deployment_host),
    }
}

/// Sibling-practice cards with each destination resolved against
/// `deployment_host` through [`sibling_practice_href`]. A card naming a brand
/// that is not live, or a slug the registry does not know, is dropped rather
/// than published with a dead link.
fn sibling_practices(
    copies: Vec<views::locales::SiblingPracticeCopy>,
    deployment_host: Option<&str>,
) -> Vec<webapp::home::SiblingPractice> {
    copies
        .into_iter()
        .filter_map(|copy| {
            let key = views::brand::BrandKey::parse(&copy.brand)?;
            let href = sibling_practice_href(key, deployment_host)?;
            Some(webapp::home::SiblingPractice {
                eyebrow: copy.eyebrow,
                heading: copy.heading,
                body: copy.body,
                cta_label: copy.cta_label,
                href,
                note: copy.note,
                sources: copy.sources,
            })
        })
        .collect()
}

fn privacy_content(
    copy: views::locales::PrivacyCopy,
    deployment_host: Option<&str>,
) -> webapp::home::PrivacyContent {
    webapp::home::PrivacyContent {
        eyebrow: copy.eyebrow,
        price: copy.price,
        price_term: copy.price_term,
        annual_price: copy.annual_price,
        annual_term: copy.annual_term,
        monthly_label: copy.monthly_label,
        annual_label: copy.annual_label,
        household_note: copy.household_note,
        annual_household_note: copy.annual_household_note,
        offer_note: copy.offer_note,
        gift_link: copy.gift_link,
        benefits_heading: copy.benefits_heading,
        record_label: copy.record_label,
        record_heading: copy.record_heading,
        record_body: copy.record_body,
        gift_heading: copy.gift_heading,
        gift_body: copy.gift_body,
        gift_cta: copy.gift_cta,
        gift_card_label: copy.gift_card_label,
        gift_card_term: copy.gift_card_term,
        closing_heading: copy.closing_heading,
        benefits: copy.benefits,
        sibling_practices: sibling_practices(copy.sibling_practices, deployment_host),
    }
}

fn cyber_injury_content(
    copy: views::locales::CyberInjuryCopy,
    deployment_host: Option<&str>,
) -> webapp::home::CyberInjuryContent {
    webapp::home::CyberInjuryContent {
        skip_label: copy.skip_label,
        utility_lines: copy.utility_lines,
        home_label: copy.home_label,
        header_wordmark: copy.header_wordmark,
        tagline: copy.tagline,
        navigation_label: copy.navigation_label,
        advantage_label: copy.advantage_label,
        testimonials_label: copy.testimonials_label,
        assessment_label: copy.assessment_label,
        booking_label: copy.booking_label,
        booking_qr_alt: copy.booking_qr_alt,
        booking_lines: copy.booking_lines,
        portrait_alt: copy.portrait_alt,
        portrait_name: copy.portrait_name,
        portrait_caption: copy.portrait_caption,
        hero_highlights: copy.hero_highlights,
        hero_summary: copy.hero_summary,
        ticker_lines: copy.ticker_lines,
        discovery_eyebrow: copy.discovery_eyebrow,
        assessment_eyebrow: copy.assessment_eyebrow,
        privacy_lines: copy.privacy_lines,
        closing_eyebrow: copy.closing_eyebrow,
        closing_label: copy.closing_label,
        connect_heading: copy.connect_heading,
        phone_label: copy.phone_label,
        phone_href: copy.phone_href,
        email: copy.email,
        qr_labels: copy.qr_labels,
        qr_descriptions: copy.qr_descriptions,
        qr_alts: copy.qr_alts,
        qr_save_label: copy.qr_save_label,
        footer_wordmark: copy.footer_wordmark,
        advertising_label: copy.advertising_label,
        ad_label: copy.ad_label,
        form_title: copy.form_title,
        form_duration: copy.form_duration,
        incident_label: copy.incident_label,
        incident_placeholder: copy.incident_placeholder,
        incident_options: copy.incident_options,
        timing_label: copy.timing_label,
        timing_placeholder: copy.timing_placeholder,
        timing_options: copy.timing_options,
        care_label: copy.care_label,
        care_options: copy.care_options,
        submit_label: copy.submit_label,
        validation_error: copy.validation_error,
        result_eyebrow: copy.result_eyebrow,
        deadline_heading: copy.deadline_heading,
        recent_heading: copy.recent_heading,
        consultation_label: copy.consultation_label,
        eyebrow: copy.eyebrow,
        hero_lines: copy.hero_lines,
        hero_note: copy.hero_note,
        discovery_heading: copy.discovery_heading,
        discovery_body: copy.discovery_body,
        discovery_cards: copy.discovery_cards,
        discovery_note: copy.discovery_note,
        assessment_heading: copy.assessment_heading,
        assessment_body: copy.assessment_body,
        assessment_note: copy.assessment_note,
        recent_result: copy.recent_result,
        deadline_result: copy.deadline_result,
        evidence_tips: copy.evidence_tips,
        record_tips: copy.record_tips,
        deadline_tip: copy.deadline_tip,
        result_note: copy.result_note,
        closing_lines: copy.closing_lines,
        footer_note: copy.footer_note,
        hero_src: views::assets::asset_url(views::assets::CYBER_INJURY_HERO_KEY),
        ad_src: views::assets::asset_url(views::assets::CYBER_INJURY_AD_KEY),
        sibling_practices: sibling_practices(copy.sibling_practices, deployment_host),
    }
}

fn daybridge_content(copy: views::locales::DaybridgeCopy) -> webapp::home::DaybridgeContent {
    webapp::home::DaybridgeContent {
        eyebrow: copy.eyebrow,
        fee_label: copy.fee_label,
        fee_price: copy.fee_price,
        fee_unit: copy.fee_unit,
        fee_body: copy.fee_body,
        fee_note: copy.fee_note,
        response_heading: copy.response_heading,
        response_body: copy.response_body,
        motions_heading: copy.motions_heading,
        motions_body: copy.motions_body,
        costs_heading: copy.costs_heading,
        costs_body: copy.costs_body,
        process_label: copy.process_label,
        process_heading: copy.process_heading,
        steps: copy.steps,
        closing_heading: copy.closing_heading,
        closing_body: copy.closing_body,
    }
}

fn death_and_divorce_content(
    copy: views::locales::DeathAndDivorceCopy,
) -> webapp::home::DeathAndDivorceContent {
    webapp::home::DeathAndDivorceContent {
        eyebrow: copy.eyebrow,
        statement_heading: copy.statement_heading,
        statement_body: copy.statement_body,
        practices: copy.practices,
        video_label: copy.video_label,
        video_body: copy.video_body,
        process_label: copy.process_label,
        process_heading: copy.process_heading,
        steps: copy.steps,
        closing_heading: copy.closing_heading,
        closing_body: copy.closing_body,
        hero_src: views::assets::asset_url(views::assets::DEATH_AND_DIVORCE_HERO_KEY),
    }
}

fn practice_link(copy: PracticeLinkCopy) -> webapp::home::PracticeLink {
    webapp::home::PracticeLink {
        mark: practice_mark(copy.mark),
        heading: copy.heading,
        body: copy.body,
        href: copy.href,
        logo_href: String::new(),
        font_family: String::new(),
        primary_color: String::new(),
    }
}

/// A sibling practice's public home, when it has one.
///
/// `None` while the practice is held out of launch, because
/// `portal::canonical_host` refuses its hosts and the link would land a
/// reader on a `404`. `views::brand::BrandKey::public_home_href_for` builds
/// the address for this deployment; the launch gate decides whether there is
/// one to hand out.
fn sibling_practice_href(
    key: views::brand::BrandKey,
    deployment_host: Option<&str>,
) -> Option<String> {
    key.is_live()
        .then(|| key.public_home_href_for(deployment_host))
}

/// The firm home page, resolved from this brand's `home.yaml`.
pub fn home(branding: &views::brand::Branding) -> webapp::home::HomeContent {
    home_for_host(branding, None)
}

fn signup_prompt(branding: &views::brand::Branding) -> webapp::home::SignupPromptCopy {
    let word = |key| {
        shared_catalog()
            .lookup(branding.brand_key.as_str(), key)
            .unwrap_or_default()
            .to_owned()
    };
    webapp::home::SignupPromptCopy {
        title: word("signup.title"),
        body: word("signup.body"),
        action: word("signup.action"),
        dismiss: word("signup.dismiss"),
        notice: word("signup.notice"),
    }
}

/// The firm home page, with cross-brand links matched to this deployment.
pub fn home_for_host(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> webapp::home::HomeContent {
    let copy: HomeCopy = load_page(branding, "home");
    webapp::home::HomeContent {
        head_title: copy.head_title,
        meta_description: copy.meta_description,
        og_description: copy.og_description,
        heading: copy.heading,
        lead: copy.lead,
        contact_href: match branding.brand_key {
            BrandKey::Misericordia => "tel:+15108002090".to_string(),
            BrandKey::CyberInjuryLaw => "/consultation".to_string(),
            _ => branding.consultation_url.to_string(),
        },
        contact_label: copy.contact_label,
        signup: Some(signup_prompt(branding)),
        photo: copy.photo.map(|photo| webapp::home::HomePhoto {
            src: views::assets::asset_url(&photo.src),
            alt: photo.alt,
            caption: photo.caption,
        }),
        service: copy.service.map(|ServiceSectionCopy { heading, body }| {
            webapp::home::ServiceSection {
                heading,
                body: paragraphs_to_home(body),
            }
        }),
        practices_heading: copy.practices_heading,
        practices: copy.practices.into_iter().map(practice_link).collect(),
        provenance: copy.provenance.map(provenance_to_home),
        company: copy.company.map(|copy| webapp::home::CompanyContent {
            principles: copy.principles,
            services_heading: copy.services_heading,
            services_note: copy.services_note,
            services_link: copy.services_link,
            services: copy.services,
            community_heading: copy.community_heading,
            community_body: copy.community_body,
            community_links: copy.community_links,
            booking_href: copy.booking_href,
            pricing_link: copy.pricing_link,
            retainer_note: copy.retainer_note,
            retainer_amount: copy.retainer_amount,
            simulator_heading: copy.simulator_heading,
            simulator_body: copy.simulator_body,
            simulator_days_label: copy.simulator_days_label,
            simulator_plan_label: copy.simulator_plan_label,
            simulator_contract_label: copy.simulator_contract_label,
            simulator_total_label: copy.simulator_total_label,
            simulator_note: copy.simulator_note,
            packages: copy.packages,
            pricing_heading: copy.pricing_heading,
            video_label: copy.video_label,
            video_src: views::assets::asset_url(views::assets::HOME_PRESENTATION_KEY),
            membership_label: copy.membership_label,
            membership_prefix: copy.membership_prefix,
            membership_price: copy.membership_price,
            membership_unit: copy.membership_unit,
            membership_body: copy.membership_body,
            membership_features: copy.membership_features,
            page_note: copy.page_note,
            drafting_heading: copy.drafting_heading,
            drafting_packages: copy.drafting_packages,
            closing_heading: copy.closing_heading,
            closing_body: copy.closing_body,
            litigation_heading: copy.litigation_heading,
            litigation_link: copy.litigation_link,
            litigation_price: copy.litigation_price,
            litigation_unit: copy.litigation_unit,
            litigation_body: copy.litigation_body,
            litigation_note: copy.litigation_note,
            navigator_heading: copy.navigator_heading,
            navigator_body: copy.navigator_body,
            navigator_link: copy.navigator_link,
        }),
        // Lawyer Shook's resolver supplies its firm notice over this catalog.
        bare: None,
        estate: copy
            .estate
            .map(|estate| estate_content(estate, deployment_host)),
        privacy: copy
            .privacy
            .map(|privacy| privacy_content(privacy, deployment_host)),
        daybridge: copy.daybridge.map(daybridge_content),
        death_and_divorce: copy.death_and_divorce.map(death_and_divorce_content),
        cyber_injury: copy
            .cyber_injury
            .map(|cyber| cyber_injury_content(cyber, deployment_host)),
        summons: copy.summons.map(summons_content),
    }
}

fn summons_content(copy: SummonsCopy) -> webapp::home::SummonsContent {
    webapp::home::SummonsContent {
        eyebrow: copy.eyebrow,
        nav: copy.nav,
        hero_note: copy.hero_note,
        fees_heading: copy.fees_heading,
        fees: copy.fees,
        fees_note: copy.fees_note,
        process_heading: copy.process_heading,
        steps: copy.steps,
        guides_heading: copy.guides_heading,
        guides_intro: copy.guides_intro,
        guides: copy.guides,
        cases_heading: copy.cases_heading,
        cases_note: copy.cases_note,
        cases: copy.cases,
        resources_heading: copy.resources_heading,
        resources: copy.resources,
        intake_heading: copy.intake_heading,
        intake_body: copy.intake_body,
        intake_checklist: copy.intake_checklist,
        intake_note: copy.intake_note,
        faq_heading: copy.faq_heading,
        faqs: copy.faqs,
        footer_note: copy.footer_note,
        reviewed: copy.reviewed,
    }
}

/// The `/disputes` page, resolved from this brand's `litigation.yaml`.
pub fn litigation(branding: &views::brand::Branding) -> webapp::litigation_page::LitigationContent {
    let copy: LitigationCopy = load_page(branding, "litigation");
    webapp::litigation_page::LitigationContent {
        head_title: copy.head_title,
        meta_description: copy.meta_description,
        og_description: copy.og_description,
        eyebrow: copy.eyebrow,
        heading: hero_words(&copy.heading),
        lead: copy.lead,
        cta_href: branding.consultation_url.to_string(),
        cta_label: copy.cta_label,
        body: paragraphs_to_home(copy.body),
    }
}

/// The `/business` page, resolved from this brand's `fractional-gc.yaml`.
pub fn fractional_gc(
    branding: &views::brand::Branding,
) -> webapp::transactional_page::TransactionalContent {
    let copy: TransactionalCopy = load_page(branding, "fractional-gc");
    webapp::transactional_page::TransactionalContent {
        head_title: copy.head_title,
        meta_description: copy.meta_description,
        og_description: copy.og_description,
        eyebrow: copy.eyebrow,
        heading: hero_words(&copy.heading),
        lead: copy.lead,
        cta_href: branding.consultation_url.to_string(),
        cta_label: copy.cta_label,
        virtues: copy
            .virtues
            .into_iter()
            .map(|VirtueCopy { word, body }| webapp::transactional_page::Virtue { word, body })
            .collect(),
        fee_heading: copy.fee_heading,
        fee_body: copy.fee_body,
        pricing: copy
            .pricing
            .into_iter()
            .map(
                |PricingCardCopy {
                     title,
                     price,
                     cadence,
                     blurb,
                     features,
                     day_rate_bill,
                 }| webapp::transactional_page::PricingOffer {
                    title,
                    price,
                    cadence,
                    blurb,
                    features,
                    day_rate: resolve_day_rate(day_rate_bill),
                },
            )
            .collect(),
        closing_heading: copy.closing_heading,
        closing_body: copy.closing_body,
        closing_email: copy.closing_email,
    }
}

/// `/navigator`, from this brand's `navigator.yaml`.
pub fn navigator(branding: &views::brand::Branding) -> PageContent {
    marketing_page(load_page(branding, "navigator"), None, branding)
}

/// The legal-aid audience shares the canonical Navigator price card and usage disclosure.
pub fn legal_aid(branding: &views::brand::Branding) -> PageContent {
    let mut content = marketing_page(load_page(branding, "legal-aid"), None, branding);
    let pricing = navigator(branding)
        .bands
        .into_iter()
        .find_map(|mut band| match &mut band {
            Band::Cards { anchor, items, .. } if anchor == "pricing" => {
                let card = items
                    .drain(..)
                    .find(|card| card.title == "Legal aid organizations")
                    .expect("invariant: navigator.yaml publishes the Legal aid organizations card");
                items.push(card);
                Some(band)
            }
            _ => None,
        })
        .expect("invariant: navigator.yaml publishes the pricing band");
    // The contact band stays last, after workshop, self-hosting, and hosting.
    let contact = content
        .bands
        .iter()
        .position(|band| matches!(band, Band::Cta { .. }))
        .unwrap_or(content.bands.len());
    content.bands.insert(contact, pricing);
    content
}

/// `/services`, from this brand's `services.yaml`.
pub fn legal_services(branding: &views::brand::Branding) -> PageContent {
    marketing_page(
        load_page(branding, "services"),
        services_catalog(branding).as_ref(),
        branding,
    )
}

/// A catalogued explainer; callers choose stems from `BrandKey::explainer_pages`.
pub fn explainer(branding: &views::brand::Branding, stem: &str) -> PageContent {
    let mut content = marketing_page(load_page(branding, stem), None, branding);
    if let Some(cta) = content.hero_cta.as_mut() {
        cta.href = branding.consultation_url.to_string();
    }
    content
}

/// `/delete-your-debt` — Neon's own gateway page naming DeleteYourDebt.com as
/// a way to work with `PandA` Law on debt relief.
///
/// The hero CTA's `href` is not authored in `gateway-delete-your-debt.yaml`
/// because a catalog file cannot know which deployment (production or
/// staging) is rendering it. It is filled here the same way
/// [`home_for_host`] fills a sibling practice's address: from
/// [`sibling_practice_href`], resolved against `deployment_host`. The YAML's
/// own `hero_cta.href` is a production fallback, kept only so the catalog
/// still satisfies the typed schema; every real render overwrites it.
pub fn delete_your_debt_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    let mut content = marketing_page(
        load_page(branding, "gateway-delete-your-debt"),
        None,
        branding,
    );
    if let Some(href) =
        sibling_practice_href(views::brand::BrandKey::DeleteYourDebt, deployment_host)
    {
        if let Some(cta) = content.hero_cta.as_mut() {
            cta.href = href;
        }
    }
    content
}

/// `/delete-your-data` — Neon's own gateway page naming DeleteYourData.com as
/// the destination for data-removal and privacy-protection work. See
/// [`delete_your_debt_gateway`], whose CTA-resolution shape this mirrors.
pub fn delete_your_data_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    let mut content = marketing_page(
        load_page(branding, "gateway-delete-your-data"),
        None,
        branding,
    );
    if let Some(href) =
        sibling_practice_href(views::brand::BrandKey::DeleteYourData, deployment_host)
    {
        if let Some(cta) = content.hero_cta.as_mut() {
            cta.href = href;
        }
    }
    content
}

/// Nautilus uses the same prices and billing periods as `DeleteYourData`.
pub fn nautilus(branding: &views::brand::Branding, _deployment_host: Option<&str>) -> PageContent {
    let mut content = marketing_page(load_page(branding, "nautilus"), None, branding);
    let privacy = home(&views::brand::DELETE_YOUR_DATA_BRANDING).privacy;
    for band in &mut content.bands {
        match band {
            Band::Cards {
                pricing_style: true,
                items,
                ..
            } => {
                if let Some(privacy) = privacy.as_ref() {
                    for (card, (price, term)) in items.iter_mut().zip([
                        (&privacy.price, &privacy.price_term),
                        (&privacy.annual_price, &privacy.annual_term),
                    ]) {
                        card.chips = vec![price.clone()];
                        card.cadence = (!term.is_empty()).then(|| term.clone());
                    }
                }
            }
            Band::AppPreview { price, .. } if price.is_empty() => {
                if let Some(privacy) = privacy.as_ref() {
                    price.clone_from(&privacy.price);
                }
            }
            Band::Enrollment { booking_href, .. } => {
                *booking_href = branding.consultation_url.to_string();
            }
            _ => {}
        }
    }
    content
}

/// `/immigration` — Neon's own gateway landing naming Abhaya Immigration as
/// the destination for immigration work, and routing to every practice page
/// beneath it. The landing is the same catalog machinery as the practice
/// pages (see [`immigration_practice_page`]): its hero button and closing
/// Cta band resolve to this deployment's Abhaya Immigration host.
pub fn immigration_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    immigration_practice_page(branding, deployment_host, "gateway-immigration")
}

/// Every `/immigration` catalog page — the seven practice pages, the
/// free-support hub, and the gateway landing itself — in the
/// statement/steps/cta vocabulary, closing on an Abhaya Immigration CTA.
///
/// What a deployment resolves is [`sibling_practice_href`]: the hero button
/// when the catalog carries one (the gateway landing does), and every Cta
/// band's action `href`.
fn immigration_practice_page(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
    stem: &str,
) -> PageContent {
    let mut content = marketing_page(load_page(branding, stem), None, branding);
    if let Some(href) = sibling_practice_href(views::brand::BrandKey::Abhaya, deployment_host) {
        if let Some(cta) = content.hero_cta.as_mut() {
            cta.href.clone_from(&href);
        }
        for band in &mut content.bands {
            if let Band::Cta {
                action: Some(action),
                ..
            } = band
            {
                action.href.clone_from(&href);
            }
        }
    }
    content
}

/// `/immigration/family-green-cards` — family petitions and marriage-based
/// green cards. See [`immigration_practice_page`].
pub fn immigration_family_green_cards(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    immigration_practice_page(branding, deployment_host, "immigration-family-green-cards")
}

/// `/immigration/citizenship` — naturalization from the N-400 to the oath.
/// See [`immigration_practice_page`].
pub fn immigration_citizenship(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    immigration_practice_page(branding, deployment_host, "immigration-citizenship")
}

/// `/immigration/employment-visas` — employment-based visas and employer
/// sponsorship. See [`immigration_practice_page`].
pub fn immigration_employment_visas(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    immigration_practice_page(branding, deployment_host, "immigration-employment-visas")
}

/// `/immigration/consular-processing` — the NVC, DS-260, and embassy
/// interview stage. See [`immigration_practice_page`].
pub fn immigration_consular_processing(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    immigration_practice_page(branding, deployment_host, "immigration-consular-processing")
}

/// `/immigration/uscis-notices` — RFE, NOID, and other USCIS notice
/// responses. See [`immigration_practice_page`].
pub fn immigration_uscis_notices(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    immigration_practice_page(branding, deployment_host, "immigration-uscis-notices")
}

/// `/immigration/humanitarian-relief` — asylum, T and U visas, and VAWA.
/// See [`immigration_practice_page`].
pub fn immigration_humanitarian_relief(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    immigration_practice_page(branding, deployment_host, "immigration-humanitarian-relief")
}

/// `/immigration/removal-defense` — immigration-court defense. See
/// [`immigration_practice_page`].
pub fn immigration_removal_defense(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    immigration_practice_page(branding, deployment_host, "immigration-removal-defense")
}

/// `/immigration/free-support` — official tools, checklists,
/// know-your-rights material, and nonprofit referrals. See
/// [`immigration_practice_page`].
pub fn immigration_free_support(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    immigration_practice_page(branding, deployment_host, "immigration-free-support")
}

/// `/estate-planning` — Neon's own gateway page naming Vesta Estate Planning
/// as the destination for estate-planning work. See
/// [`delete_your_debt_gateway`], whose CTA-resolution shape this mirrors.
pub fn estate_planning_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    let mut content = marketing_page(
        load_page(branding, "gateway-estate-planning"),
        None,
        branding,
    );
    if let Some(href) = sibling_practice_href(views::brand::BrandKey::Vesta, deployment_host) {
        if let Some(cta) = content.hero_cta.as_mut() {
            cta.href = href;
        }
    }
    content
}

/// `/accidents` — Neon's own gateway page naming Misericordia Injury Law as
/// the destination for injury claims. See [`delete_your_debt_gateway`],
/// whose CTA-resolution shape this mirrors.
pub fn accidents_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    let mut content = marketing_page(load_page(branding, "gateway-accidents"), None, branding);
    if let Some(href) = sibling_practice_href(views::brand::BrandKey::Misericordia, deployment_host)
    {
        if let Some(cta) = content.hero_cta.as_mut() {
            cta.href = href;
        }
    }
    content
}

/// `/divorce` — Neon's own gateway page naming Daybridge Divorce Law as the
/// destination for divorce representation. See [`delete_your_debt_gateway`],
/// whose CTA-resolution shape this mirrors.
pub fn divorce_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    let mut content = marketing_page(load_page(branding, "gateway-divorce"), None, branding);
    if let Some(href) = sibling_practice_href(views::brand::BrandKey::Daybridge, deployment_host) {
        if let Some(cta) = content.hero_cta.as_mut() {
            cta.href = href;
        }
    }
    content
}

/// The public notation format explanation, loaded from the English catalog.
pub(crate) fn notations_content() -> PageContent {
    let branding = &views::brand::DEFAULT_BRANDING;
    marketing_page(load_page(branding, "notations"), None, branding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use views::brand::BrandKey;
    use views::locales::parse_locale_file;
    use webapp::marketing_page::Band as RenderedBand;

    /// Every catalog page's `<title>` and `og:title` lead with the brand, then a
    /// pipe, then the page: `{site_name} | Home`.
    #[test]
    fn every_brand_home_carries_the_account_invitation() {
        for key in views::brand::BrandKey::ALL {
            let content = home(key.resolve_branding(&views::brand::DEFAULT_BRANDING));
            assert!(content.signup.is_some(), "{}", key.as_str());
        }
    }

    #[test]
    fn architectural_photos_cover_the_homepages_without_substantive_art() {
        for key in [
            BrandKey::Neon,
            BrandKey::LawyerShook,
            BrandKey::Misericordia,
            BrandKey::DeleteYourDebt,
            BrandKey::Daybridge,
            BrandKey::Summons,
        ] {
            let content = home(key.resolve_branding(&views::brand::DEFAULT_BRANDING));
            let photo = content.photo.expect("a catalogued photograph");
            assert!(photo.src.contains("img/architecture/"));
            assert!(!photo.alt.is_empty());
        }
    }

    #[test]
    fn every_catalog_head_title_reads_brand_pipe_page() {
        for &key in BrandKey::ALL {
            for &page in key.catalog_pages() {
                let Some(yaml) = catalog_yaml(key, page) else {
                    continue;
                };
                let value: serde_yaml::Value = serde_yaml::from_str(yaml).expect("catalog parses");
                let Some(title) = value.get("head_title").and_then(serde_yaml::Value::as_str)
                else {
                    continue;
                };
                let page_name = title.strip_prefix("{site_name} | ").unwrap_or_else(|| {
                    panic!(
                        "{}/{page}.yaml head_title {title:?} must read `{{site_name}} | Page`",
                        key.as_str()
                    )
                });
                assert!(
                    !page_name.trim().is_empty() && !page_name.contains("{site_name}"),
                    "{}/{page}.yaml head_title {title:?} names the page once, after the brand",
                    key.as_str()
                );
            }
        }
    }

    /// A page's YAML `og_description` reaches the rendered content, and a page
    /// without one leaves it unset so the preview falls back to
    /// `meta_description`.
    #[test]
    fn og_description_is_settable_in_yaml() {
        let yaml = "head_title: \"{site_name} | Disputes\"\nmeta_description: Search line.\nog_description: Share line for {site_name}.\ntitle: Disputes\n";
        let page: PageContent = marketing_page(
            load(yaml, &views::brand::DEFAULT_BRANDING),
            None,
            &views::brand::DEFAULT_BRANDING,
        );
        assert_eq!(
            page.og_description.as_deref(),
            Some(format!("Share line for {}.", views::brand::FIRM_BRAND.site_name).as_str())
        );
        assert_eq!(home(&views::brand::DEFAULT_BRANDING).og_description, None);
    }

    #[test]
    fn service_directory_includes_every_registered_brand_with_its_logo() {
        let page = legal_services(&views::brand::DEFAULT_BRANDING);
        let cards = page
            .bands
            .iter()
            .find_map(|band| match band {
                RenderedBand::Cards { anchor, items, .. } if anchor == "brands" => Some(items),
                _ => None,
            })
            .expect("brand directory");
        // Neon Law's own site needs no Neon Law card, and the Lawyer Shook
        // and Guieb & Shook holding pages are firm marks, not services, so
        // neither is listed.
        let brands: Vec<BrandKey> = BrandKey::ALL
            .iter()
            .copied()
            .filter(|key| {
                !matches!(
                    key,
                    BrandKey::Neon | BrandKey::LawyerShook | BrandKey::GuiebShook
                )
            })
            .collect();
        assert_eq!(cards.len(), brands.len());
        for key in brands {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            let expected = branding.firm.logo_href;
            assert!(
                cards
                    .iter()
                    .any(|card| card.logo_href.as_deref() == Some(expected)),
                "{} logo missing",
                key.as_str()
            );
        }
    }

    #[test]
    fn home_company_contract_publishes_setup_and_daily_prices() {
        let copy: serde_yaml::Value = serde_yaml::from_str(NEON_HOME_YAML).unwrap();
        let company = &copy["company"];
        assert_eq!(company["membership_prefix"].as_str(), Some("From"));
        assert_eq!(company["membership_price"].as_str(), Some("$100"));
        assert_eq!(company["membership_unit"].as_str(), Some("/ day"));
        assert!(!company["membership_body"]
            .as_str()
            .unwrap()
            .contains("$100/day"));
        assert!(company["membership_body"]
            .as_str()
            .unwrap()
            .contains("Additional flat fees may apply and are quoted in the engagement letter."));
        assert!(
            company["express_price"].is_null(),
            "expedited reviews are retired"
        );
        assert_eq!(company["drafting_packages"][0][0].as_str(), Some("/ once"));
        assert_eq!(company["drafting_packages"][0][1].as_str(), Some("$5,000"));
        assert_eq!(
            company["drafting_packages"].as_sequence().map(Vec::len),
            Some(1)
        );
        assert!(company["retainer_note"]
            .as_str()
            .unwrap()
            .contains("$10,000"));
    }

    #[test]
    fn fractional_gc_copy_qualifies_the_daily_starting_price_and_extra_fees() {
        let copy: serde_yaml::Value = serde_yaml::from_str(NEON_FRACTIONAL_GC_YAML).unwrap();
        let body = copy["fee_body"].as_str().unwrap();
        assert!(body.contains("starts at $50/day"));
        assert!(body
            .contains("Additional flat fees may apply and are quoted in the engagement letter."));
        assert!(body.contains("$10,000 minimum retainer"));
        assert!(body.contains("Notations begin at $100"));
    }

    /// The shipped shared catalog is the contract both repositories consume.
    /// If it stops parsing, the export the other repository pins stops being
    /// producible — so this fails here rather than in the exporter.
    #[test]
    fn shared_catalog_is_valid() {
        let catalog = shared_catalog();
        assert_eq!(
            catalog.catalog_version,
            views::locales::shared::SUPPORTED_CATALOG_VERSION
        );
        for key in views::locales::shared::REQUIRED_KEYS {
            assert!(
                catalog.lookup(BrandKey::Neon.as_str(), key).is_some(),
                "the shared catalog must publish `{key}`"
            );
        }
    }

    /// Every `{shared:…}` a shipped page references must name a key the
    /// catalog defines, for every brand that publishes that page. An
    /// unresolved reference would otherwise reach a reader as a brace.
    #[test]
    fn every_shared_reference_a_shipped_page_makes_resolves() {
        let catalog = shared_catalog();
        for key in BrandKey::ALL {
            for page in key.catalog_pages() {
                let yaml = catalog_yaml(*key, page).expect("shipped catalog");
                for referenced in views::locales::shared::referenced_keys(yaml) {
                    assert!(
                        catalog.lookup(key.as_str(), &referenced).is_some(),
                        "{} `{page}` references `{referenced}`, which the shared catalog does not define",
                        key.as_str()
                    );
                }
                views::locales::shared::resolve_references(yaml, catalog, key.as_str())
                    .unwrap_or_else(|err| panic!("{} `{page}`: {err}", key.as_str()));
            }
        }
    }

    /// The shared catalog is not decoration: the pages that reference it must
    /// actually be the ones that carry the duplicated sentences.
    #[test]
    fn shared_copy_is_consumed_by_pages_or_the_exported_home_contract() {
        let mut referenced: std::collections::BTreeSet<String> = BrandKey::ALL
            .iter()
            .flat_map(|key| {
                key.catalog_pages()
                    .iter()
                    .filter_map(|page| catalog_yaml(*key, page))
                    .flat_map(views::locales::shared::referenced_keys)
            })
            .collect();
        // Lead copy is consumed by the shared form component rather than a
        // page YAML document, so account for that typed catalog reader here.
        referenced.extend([
            "lead.consent".to_string(),
            "lead.phone_helper".to_string(),
            "lead.sms_label".to_string(),
        ]);
        // Footer and account copy is read directly by the shared SSR components.
        referenced.extend(
            [
                "footer.provider_prefix",
                "footer.provider_name",
                "footer.provider_href",
                "footer.ownership",
                "signup.title",
                "signup.body",
                "signup.action",
                "signup.dismiss",
                "signup.notice",
                "signup.password_label",
                "signup.existing",
                "signup.invalid",
                "signup.rejected",
                "signup.signin_required",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        // The versioned `home.*` keys stay required by the shared contract
        // although no page references them: Neon's company offer is authored
        // in its own home catalog.
        for key in shared_catalog()
            .keys()
            .into_iter()
            .filter(|key| !key.starts_with("home."))
        {
            assert!(
                referenced.contains(key),
                "`{key}` is authored in the shared catalog but no page reads it"
            );
        }
    }

    /// Every registry key ships every catalog page it declares, and each file
    /// deserializes as that page. A missing file fails here, not at first request.
    #[test]
    fn every_registry_key_has_every_required_catalog_file() {
        for key in BrandKey::ALL {
            for page in key.catalog_pages() {
                let yaml = catalog_yaml(*key, page).unwrap_or_else(|| {
                    panic!(
                        "{} is missing locales/en/{}/{page}.yaml",
                        key.as_str(),
                        key.as_str()
                    )
                });
                parse_locale_file(page, yaml)
                    .unwrap_or_else(|err| panic!("{} `{page}`: {err}", key.as_str()));
            }
        }
    }

    /// The band a brand declares in its `services.yaml` and the catalog file
    /// beside it must agree. A `kind: services` band with no catalog panics at
    /// router build; a catalog nothing renders is a file nobody reads.
    #[test]
    fn the_services_band_only_ships_where_a_catalog_does() {
        for key in BrandKey::ALL {
            if !key.catalog_pages().contains(&"services") {
                continue;
            }
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            let declares_band = catalog_yaml(*key, "services")
                .expect("a brand shipping `services` ships the file")
                .contains("kind: services");
            assert_eq!(
                declares_band,
                services_catalog(branding).is_some(),
                "{}: a `kind: services` band and a services catalog ship together",
                key.as_str()
            );
        }
    }

    /// `/services` renders the schedule from the catalog: every service, with
    /// its fee resolved and its `related` ids turned into names a reader can
    /// follow.
    #[test]
    fn abhaya_publishes_flat_fees_for_common_filings_through_the_services_band() {
        let branding = &views::brand::ABHAYA_BRANDING;
        let catalog = services_catalog(branding).expect("Abhaya ships a catalog");
        let content = legal_services(branding);
        let band = content
            .bands
            .iter()
            .find_map(RenderedBand::services)
            .expect("the Abhaya services page renders its services band");
        assert_eq!(band.services.len(), catalog.services.len());

        let fee = |id: &str| {
            band.services
                .iter()
                .find(|service| service.id == id)
                .unwrap_or_else(|| panic!("{id} renders"))
        };
        let o1 = fee("o1-new");
        assert_eq!(o1.fee, "$7,200");
        assert_eq!(o1.category, "Work visas");
        assert_eq!(fee("aos-adult").fee, "$3,150");
        assert_eq!(fee("eb2-niw").category, "Green cards");
        // Immigration fees never include what USCIS or the State Department
        // charges, so every card carries the government-fee badge.
        assert!(band.services.iter().all(|service| service.state_fee));
        assert!(fee("ead-renewal").matches("work permit"));
    }

    #[test]
    fn the_services_page_renders_the_whole_catalog() {
        let catalog =
            services_catalog(&views::brand::DEFAULT_BRANDING).expect("Neon ships a catalog");
        let content = legal_services(&views::brand::DEFAULT_BRANDING);
        let band = content
            .bands
            .iter()
            .find_map(RenderedBand::services)
            .expect("the page renders its services band");
        assert_eq!(band.services.len(), catalog.services.len());
        assert_eq!(band.services.len(), 18);

        // The fee is resolved: a flat-fee service reads the catalog's one
        // figure rather than shipping an empty price.
        let llc = band
            .services
            .iter()
            .find(|service| service.id == "llc-file")
            .expect("llc-file renders");
        assert_eq!(llc.fee, catalog.flat_fee);
        assert_eq!(llc.category, "Start a business");
        // The estate audience words ride along, searched and never printed.
        let will = band
            .services
            .iter()
            .find(|service| service.id == "will")
            .expect("will renders");
        assert_eq!(will.audience, "personal family legacy");
        assert!(
            will.matches("family"),
            "estate work is findable by `family`"
        );
        assert_eq!(will.fee, "$3,000");
        let family_plan = band
            .services
            .iter()
            .find(|service| service.id == "estate-package")
            .expect("estate-package renders");
        let quote = family_plan
            .package
            .as_ref()
            .expect("estate-package is a Notation package");
        assert_eq!(quote.members.len(), 3);
        assert_eq!(family_plan.fee, "$5,000");
        // The retired consumer plan's discount is gone with the plan.
        assert_eq!(family_plan.plan_price, None);
    }

    /// The search finds services by the words a reader would actually type,
    /// through the shipped catalog rather than a fixture.
    #[test]
    fn the_shipped_catalog_answers_a_readers_words() {
        let content = legal_services(&views::brand::DEFAULT_BRANDING);
        let band = content
            .bands
            .iter()
            .find_map(RenderedBand::services)
            .expect("the page renders its services band");
        for (needle, expected) in [
            ("help with my LLC", "llc-file"),
            ("trademark", "trademark"),
            ("eviction", "eviction"),
            ("I need a will", "will"),
            ("1504", "msa"),
        ] {
            let found = band.matching(needle);
            assert!(
                found.iter().any(|service| service.id == expected),
                "`{needle}` must find `{expected}`; found {:?}",
                found.iter().map(|s| &s.id).collect::<Vec<_>>()
            );
        }
        // Every example chip the band offers finds something. A chip that
        // lands on the empty state is a chip advertising a dead end.
        for example in &band.examples {
            assert!(
                !band.matching(&example.query).is_empty(),
                "the `{}` chip finds nothing",
                example.label
            );
        }
    }

    /// Brand placeholders become the mounted site name and inbox.
    #[test]
    fn home_catalog_names_the_mounted_brand() {
        let content = home(&views::brand::DEFAULT_BRANDING);
        assert!(content
            .head_title
            .contains(views::brand::FIRM_BRAND.site_name));
        assert_eq!(content.contact_href, views::brand::consultation_url());
        assert_eq!(content.heading, "Keep building.");
        assert!(content.company.is_some());
        assert!(content.practices.is_empty());
    }

    #[test]
    fn abhaya_photo_uses_the_asset_checked_before_deployment() {
        let branding = BrandKey::Abhaya.resolve_branding(&views::brand::DEFAULT_BRANDING);
        let photo = home(branding).photo.expect("Abhaya home photograph");
        assert_eq!(
            photo.src,
            views::assets::asset_url(views::assets::ABHAYA_COMMUNITY_PHOTO_KEY)
        );
        assert!(!photo.alt.is_empty());
        assert!(!photo.caption.is_empty());
    }

    #[test]
    fn firm_brand_consultation_actions_use_the_booking_calendar() {
        for key in [
            BrandKey::Neon,
            BrandKey::DeleteYourData,
            BrandKey::Vesta,
            BrandKey::Abhaya,
            BrandKey::Daybridge,
            BrandKey::DeathAndDivorce,
        ] {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            let content = home(branding);
            assert_eq!(content.contact_href, branding.consultation_url, "{key:?}");
            assert_eq!(
                content.contact_label,
                if key == BrandKey::Vesta {
                    "Book a meet & greet"
                } else if key == BrandKey::DeleteYourData {
                    "Get privacy protection"
                } else {
                    "Book Consultation"
                },
                "{key:?}"
            );
        }

        for key in [BrandKey::Neon, BrandKey::Abhaya] {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            let services = legal_services(branding);
            let closing = services
                .bands
                .iter()
                .find_map(|band| match band {
                    Band::Cta { action, .. } => action.as_ref(),
                    _ => None,
                })
                .expect("closing booking action");
            assert_eq!(closing.href, branding.consultation_url, "{key:?}");
            assert_eq!(closing.label, "Book Consultation", "{key:?}");
        }

        for key in [
            BrandKey::Neon,
            BrandKey::Vesta,
            BrandKey::Abhaya,
            BrandKey::Daybridge,
            BrandKey::DeathAndDivorce,
            BrandKey::LawyerShook,
        ] {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            let services = legal_services(branding);
            let cta = services.hero_cta.expect("services hero CTA");
            assert_eq!(cta.href, branding.consultation_url, "{key:?}");
            assert_eq!(
                cta.label,
                if key == BrandKey::Vesta {
                    "Book a meet & greet"
                } else if key == BrandKey::DeleteYourData {
                    "Get privacy protection"
                } else {
                    "Book Consultation"
                },
                "{key:?}"
            );
            assert!(
                !catalog_yaml(key, "services")
                    .expect("services catalog")
                    .contains("mailto:"),
                "{key:?} services contain an email draft link"
            );
        }
    }

    /// Misericordia's call-labeled home CTA calls the published phone number,
    /// which remains reachable from the rendered home content.
    #[test]
    fn misericordia_home_cta_calls_and_keeps_phone_reachable() {
        let branding =
            views::brand::BrandKey::Misericordia.resolve_branding(&views::brand::DEFAULT_BRANDING);
        let content = home(branding);

        assert_eq!(content.contact_label, "Call for a free consultation");
        assert_eq!(content.contact_href, "tel:+15108002090");
        assert!(content
            .service
            .as_ref()
            .expect("Misericordia renders its service copy")
            .body
            .iter()
            .flatten()
            .any(|run| run.text == "+1 510 800 2090"
                && run.href.as_deref() == Some("tel:+15108002090")));
    }

    /// The firm's own site markets to businesses alone, never individuals.
    ///
    /// Asserted on the rendered copy rather than the routing table, because a
    /// page can be unreachable while the words that sold it survive in the
    /// hero, the plan chooser, or a link.
    ///
    /// The audience check looks for "business", not "technology": the hero
    /// names the whole business-agreement catalog (employment, privacy,
    /// blockchain, tax, government contracts), of which the `services` list's
    /// opening `Technology transactions` is one part.
    #[test]
    fn the_home_page_no_longer_markets_to_individuals() {
        let content = home(&views::brand::DEFAULT_BRANDING);
        let mut text = vec![content.heading.clone(), content.lead.clone()];
        if let Some(service) = content.service.as_ref() {
            text.extend(service.body.iter().flatten().map(|run| run.text.clone()));
        }
        for practice in &content.practices {
            text.push(practice.heading.clone());
            text.push(practice.body.clone());
        }
        let text = text.join(" ");

        for gone in ["Personal plan", "Personal Plan", "your family", "/personal"] {
            assert!(
                !text.contains(gone),
                "{gone:?} still on the home page: {text}"
            );
        }
        assert!(
            text.to_lowercase().contains("business"),
            "and the audience is named: {text}"
        );
    }

    /// `/business` publishes its base package as a $50-a-day retainer.
    #[test]
    fn fractional_gc_publishes_its_fifty_dollar_day_rate() {
        let content = fractional_gc(&views::brand::DEFAULT_BRANDING);
        let offer = content.pricing.first().expect("the Business plan offer");
        assert_eq!(offer.price, "From $50");
        assert_eq!(offer.cadence.as_deref(), Some("/day"));
        for benefit in [
            "Contract-library access",
            "Name Neon Law as your counsel",
            "$10,000 minimum retainer to start",
            "60 days before daily credits run out",
            "unchanged template for signature at $5",
            "Notations for prepared or revised documents start at $100",
            "One agreed scope and price for each Notation",
        ] {
            assert!(
                offer
                    .features
                    .iter()
                    .any(|feature| feature.contains(benefit)),
                "the Business plan names {benefit:?}: {:?}",
                offer.features
            );
        }
        assert!(
            offer.blurb.contains("From $50/day"),
            "the Business plan qualifies its daily starting price"
        );
        assert_eq!(
            offer.day_rate.as_ref().map(|badge| badge.amount),
            Some(50),
            "the Business plan renders the matching $50 bill mark"
        );
        assert!(
            offer
                .day_rate
                .as_ref()
                .is_some_and(|badge| badge.image_src.ends_with("fifty-dollar-bill.jpg")),
            "the Business plan uses the published $50 bill asset"
        );
    }

    #[test]
    fn litigation_starts_with_a_free_consultation_without_a_plan() {
        let content = litigation(&views::brand::DEFAULT_BRANDING);
        let text = content
            .body
            .iter()
            .flatten()
            .map(|run| run.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(content.cta_label, "Request a free consultation");
        assert!(content.lead.contains("free consultation"));
        assert!(text.contains("do not need a plan"));
        assert!(text.contains("what it costs before you decide"));
        assert!(!text.contains("Navigator"));
        // Scoped to company disputes, and saying so — without it the page
        // keeps drawing individual matters through a different door.
        for named in [
            "intellectual property",
            "employment",
            "investor",
            "We do not take personal injury",
        ] {
            assert!(text.contains(named), "disputes names {named:?}: {text}");
        }
        assert!(
            text.contains("We do not promise a result"),
            "no outcome promise: {text}"
        );
    }

    #[test]
    fn cyber_injury_home_uses_the_pocket_moniker_and_injury_matters() {
        let branding = BrandKey::CyberInjuryLaw.resolve_branding(&views::brand::DEFAULT_BRANDING);
        let content = home(branding);
        assert_eq!(
            content.heading,
            "We use AI to put more money in your pocket"
        );
        let copy = content
            .cyber_injury
            .expect("Cyber Injury Law has campaign copy");
        assert_eq!(
            copy.hero_lines,
            [
                "WE USE AI TO".to_string(),
                "PUT MORE".to_string(),
                "MONEY IN".to_string(),
                "YOUR POCKET.".to_string()
            ]
        );
        assert_eq!(
            copy.hero_highlights,
            [
                String::new(),
                "MORE".to_string(),
                "MONEY IN".to_string(),
                "YOUR POCKET.".to_string()
            ]
        );
        assert!(copy.incident_options[0].contains("Car accident"));
        assert!(copy.incident_options[1].contains("Medical"));
        assert!(copy
            .discovery_cards
            .iter()
            .any(|card| card[0].contains("VEHICLE COLLISIONS")));
        assert!(copy
            .discovery_cards
            .iter()
            .any(|card| card[0].contains("DATA THEFT")));
    }

    /// The services catalog remains brand-safe even though the route is
    /// gated off by `BrandKey::publishes_firm_path`.
    #[test]
    fn lawyer_shook_services_catalog_is_brand_keyed_and_attributed() {
        let branding = &views::brand::LAWYER_SHOOK_BRANDING;
        let services_content = legal_services(branding);
        assert!(services_content.hero_lead.contains("Guieb & Shook PC"));
        assert!(!services_content.meta_description.contains("flat-fee"));
    }

    /// Every word one of these pages renders, flattened, so a claim placed in
    /// any field — a lead, a card body, a step, a chip — is visible to a
    /// guard here. Scoped to this test module rather than reused from
    /// `firm_copy::firm_copy_tests`, whose `band_text` guards the firm's own
    /// `/navigator` and `/services` pages, not a house brand's.
    fn dyd_page_text(content: &PageContent) -> String {
        fn paragraphs(body: &[Vec<Run>]) -> String {
            body.iter()
                .flat_map(|p| p.iter().map(|r| r.text.clone()))
                .collect::<Vec<_>>()
                .join(" ")
        }
        let bands = content
            .bands
            .iter()
            .map(|band| match band {
                Band::Statement {
                    heading,
                    lead,
                    body,
                } => {
                    format!("{heading} {lead} {}", paragraphs(body))
                }
                Band::Cards {
                    overline,
                    heading,
                    description,
                    items,
                    ..
                } => {
                    let cards = items
                        .iter()
                        .map(|c| {
                            format!("{} {} {}", c.title, c.chips.join(" "), paragraphs(&c.body))
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!(
                        "{overline} {heading} {} {cards}",
                        description.clone().unwrap_or_default()
                    )
                }
                Band::Steps {
                    overline,
                    heading,
                    description,
                    items,
                    ..
                } => {
                    let steps = items
                        .iter()
                        .map(|s| format!("{} {}", s.title, paragraphs(&s.body)))
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!(
                        "{overline} {heading} {} {steps}",
                        description.clone().unwrap_or_default()
                    )
                }
                Band::Cta { heading, body, .. } => {
                    format!("{heading} {}", body.clone().unwrap_or_default())
                }
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "{} {} {} {bands}",
            content.tagline, content.hero_lead, content.meta_description
        )
    }

    #[test]
    fn vesta_offer_publishes_starting_price_and_separates_administration() {
        let content = home(&views::brand::VESTA_BRANDING);
        let estate = content.estate.expect("estate offer");
        assert_eq!(estate.price, "From $1,111");
        assert!(estate.fee_note.starts_with("Fees depend on the scope"));
        assert!(estate
            .plan_description
            .starts_with("For a basic estate plan"));
        assert!(estate
            .fee_note
            .contains("trust administration, and later updates require a separate quote"));
        assert_eq!(content.heading, "Your life is part of a larger story.");
    }

    #[test]
    fn vesta_home_resolves_all_six_photos_through_the_asset_origin() {
        let estate = home(&views::brand::VESTA_BRANDING)
            .estate
            .expect("estate offer");
        assert_eq!(estate.photos.len(), 6);
        for ([url, alt], (key, expected_alt)) in
            estate.photos.iter().zip(views::assets::VESTA_PHOTOS)
        {
            assert_eq!(url, &views::assets::asset_url(key));
            assert_eq!(alt, expected_alt);
        }
    }

    #[test]
    fn privacy_offer_has_only_monthly_and_prepaid_annual_prices() {
        let branding = &views::brand::DELETE_YOUR_DATA_BRANDING;
        let content = home(branding);
        let privacy = content.privacy.expect("privacy offer");
        assert_eq!(privacy.price, "$29/mo");
        assert_eq!(privacy.price_term, "");
        assert!(
            privacy.household_note.is_empty(),
            "no household surcharge is advertised: {}",
            privacy.household_note
        );
        assert!(privacy
            .benefits
            .iter()
            .any(|item| item[1].contains("Choose whether to opt in")));
        assert!(content.practices.is_empty());
        assert!(content.service.is_none());
        let text = serde_yaml::to_string(&privacy).expect("privacy copy serializes");
        assert!(
            !text.contains("$50"),
            "the retired annual price survives: {text}"
        );
        assert_eq!(privacy.annual_price, "$290/year");
        assert_eq!(privacy.annual_term, "");
        assert!(privacy.annual_household_note.contains("Prepay"));
        for retired in ["$30", "$300", "$20", "$200", "additional member"] {
            assert!(!text.contains(retired), "retired offer: {retired}");
        }
        assert!(privacy.offer_note.contains("non-legal service"));
        assert!(!text.contains("we guarantee"));
        assert!(home(&views::brand::DEFAULT_BRANDING).privacy.is_none());
        assert!(home(&views::brand::VESTA_BRANDING).privacy.is_none());
        let services = dyd_page_text(&legal_services(branding));
        assert!(services.contains("$29/mo") && services.contains("$290/year"));
        assert!(!services.contains("$50"));
    }

    #[test]
    fn privacy_prices_are_consistent_across_public_referrals_and_summaries() {
        for (name, text) in [
            (
                "privacy home",
                include_str!("../locales/en/delete-your-data/home.yaml"),
            ),
            (
                "privacy services",
                include_str!("../locales/en/delete-your-data/services.yaml"),
            ),
            (
                "gateway",
                include_str!("../locales/en/neon/gateway-delete-your-data.yaml"),
            ),
            ("Nautilus", include_str!("../locales/en/neon/nautilus.yaml")),
            ("Vesta", include_str!("../locales/en/vesta/home.yaml")),
            (
                "Cyber Injury Law",
                include_str!("../locales/en/cyber-injury-law/home.yaml"),
            ),
            (
                "mission",
                views::brand::DELETE_YOUR_DATA_BRANDING.mission_description,
            ),
            (
                "service",
                views::brand::DELETE_YOUR_DATA_BRANDING.service_description,
            ),
        ] {
            assert!(text.contains("$29") && text.contains("$290"), "{name}");
            assert!(
                text.contains("prepaid") || text.contains("Prepay"),
                "{name}"
            );
            for retired in ["$30", "$300", "$20", "$200", "additional member"] {
                // Nautilus prices a lawyer review "From $20"; the retired
                // $20 offer was a subscription price, which carries a cadence.
                if name == "Nautilus" && retired == "$20" {
                    assert!(!text.contains("$20/"), "{name}: retired offer {retired}");
                    continue;
                }
                assert!(!text.contains(retired), "{name}: retired offer {retired}");
            }
        }
    }

    /// The privacy home points a reader to two sibling practices — the
    /// optional death check through Vesta Estate Planning and cyber-crime
    /// claims through Cyber Injury Law — with each destination resolved for
    /// the rendering deployment, the same way the Neon gateways resolve theirs.
    #[test]
    fn privacy_home_resolves_the_death_check_and_cyber_crime_referrals_per_deployment() {
        let branding = &views::brand::DELETE_YOUR_DATA_BRANDING;
        for (host, vesta, cyber) in [
            (
                Some("www.deleteyourdata.com"),
                "https://www.vestaestateplanning.com",
                "https://www.cyberinjurylaw.com",
            ),
            (
                Some("staging.deleteyourdata.com"),
                "https://staging.vestaestateplanning.com",
                "https://staging.cyberinjurylaw.com",
            ),
        ] {
            let privacy = home_for_host(branding, host)
                .privacy
                .expect("privacy offer");
            let hrefs: Vec<&str> = privacy
                .sibling_practices
                .iter()
                .map(|referral| referral.href.as_str())
                .collect();
            assert_eq!(hrefs, [vesta, cyber], "{host:?}");
            let death_check = &privacy.sibling_practices[0];
            assert_eq!(death_check.heading, "An optional death check.", "{host:?}");
            assert!(
                death_check.body.contains("Vesta Estate Planning"),
                "{host:?}"
            );
            assert!(death_check.body.contains("triggers"), "{host:?}");
            assert!(death_check.body.contains("Neon Law Nautilus"), "{host:?}");
            assert!(!death_check.body.contains("guarantee"), "{host:?}");
            // The life-insurance note carries a linked source for each claim.
            assert!(death_check.note.contains("life insurer"), "{host:?}");
            let citations: Vec<&str> = death_check
                .sources
                .iter()
                .map(|[citation, _]| citation.as_str())
                .collect();
            assert_eq!(
                citations,
                [
                    "Nev. Rev. Stat. § 688A.140",
                    "Nev. Rev. Stat. § 688D.090",
                    "Patronis v. United Ins. Co. of Am., No. 1D18-2114 (Fla. 1st DCA June 3, 2020)",
                ],
                "{host:?}"
            );
            assert!(
                death_check
                    .sources
                    .iter()
                    .all(|[_, url]| url.starts_with("https://")),
                "{host:?}: every citation carries its link"
            );
            let cyber_crimes = &privacy.sibling_practices[1];
            assert!(cyber_crimes.heading.contains("cyber crimes"), "{host:?}");
            assert!(cyber_crimes.body.contains("Cyber Injury Law"), "{host:?}");
            assert_eq!(
                cyber_crimes.cta_label, "Visit CyberInjuryLaw.com",
                "{host:?}"
            );
        }
    }

    /// Nautilus starts intake on the current Neon deployment.
    #[test]
    fn nautilus_page_prices_the_subscription_and_resolves_the_cta_per_deployment() {
        for (host, expected) in [
            ("www.neonlaw.com", "#enrollment"),
            ("staging.neonlaw.com", "#enrollment"),
        ] {
            let content = nautilus(&views::brand::DEFAULT_BRANDING, Some(host));
            assert_eq!(
                content.hero_cta.as_ref().expect("hero CTA").href,
                expected,
                "{host}"
            );
            let text = serde_yaml::to_string(&content).expect("page serializes");
            for required in [
                "$29/mo",
                "$290/year",
                "forming an LLC",
                "Data removal",
                "Privacy protection",
                "Credit monitoring",
                "Dark web monitoring",
                "Identity theft insurance",
                "Unclaimed property",
                "Estate planning",
                "From $20",
                "Lex Tecnica",
                "App Store",
                "Google Play",
                "legal fees are excluded",
                "must be prepaid",
            ] {
                assert!(text.contains(required), "{host}: missing {required}");
            }
            // One product name, plainly: no themed card names, and no sibling
            // brand drawing attention to a single check.
            for retired in [
                "Neon Law Nautilus",
                "The Reef",
                "The Cove",
                "Vesta",
                "DeleteYourData",
            ] {
                assert!(!text.contains(retired), "{host}: {retired} survives");
            }
            assert!(!text.contains("$50"), "{host}: the retired price survives");
        }
    }

    #[test]
    fn nautilus_prices_and_billing_periods_match_delete_your_data() {
        let privacy = home(&views::brand::DELETE_YOUR_DATA_BRANDING)
            .privacy
            .expect("privacy");
        let content = nautilus(&views::brand::DEFAULT_BRANDING, None);
        let cards = content
            .bands
            .iter()
            .find_map(|band| match band {
                Band::Cards {
                    items,
                    pricing_style: true,
                    ..
                } => Some(items),
                _ => None,
            })
            .expect("pricing cards");
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].chips, vec![privacy.price.clone()]);
        assert_eq!(
            cards[0].cadence.as_deref(),
            (!privacy.price_term.is_empty()).then_some(privacy.price_term.as_str())
        );
        assert_eq!(cards[1].chips, vec![privacy.annual_price]);
        assert_eq!(
            cards[1].cadence.as_deref(),
            (!privacy.annual_term.is_empty()).then_some(privacy.annual_term.as_str())
        );
        let listing_price = content
            .bands
            .iter()
            .find_map(|band| match band {
                Band::AppPreview { price, .. } => Some(price),
                _ => None,
            })
            .expect("app preview");
        assert_eq!(listing_price, &privacy.price);
        let text = serde_yaml::to_string(&content).expect("serialize");
        assert!(text.contains("does not") || text.contains("do not"));
        assert!(text.contains("non-legal service"));
        assert!(text.contains("attorney-client privilege"));
    }

    /// Vesta Estate Planning and Cyber Injury Law each link back to
    /// DeleteYourData.com, resolved for the rendering deployment, so the
    /// referral runs both ways.
    #[test]
    fn vesta_and_cyber_injury_link_back_to_delete_your_data_per_deployment() {
        for (host, expected) in [
            ("www.example.test", "https://www.deleteyourdata.com"),
            ("staging.example.test", "https://staging.deleteyourdata.com"),
        ] {
            let vesta = home_for_host(&views::brand::VESTA_BRANDING, Some(host))
                .estate
                .expect("estate offer")
                .sibling_practices;
            let cyber = home_for_host(&views::brand::CYBER_INJURY_BRANDING, Some(host))
                .cyber_injury
                .expect("cyber injury campaign")
                .sibling_practices;
            for (brand, cards) in [("vesta", &vesta), ("cyber-injury-law", &cyber)] {
                assert_eq!(cards.len(), 1, "{brand} on {host}");
                assert_eq!(cards[0].href, expected, "{brand} on {host}");
                assert_eq!(cards[0].cta_label, "Visit DeleteYourData.com", "{brand}");
                assert!(cards[0].body.contains("$29/mo"), "{brand}");
                assert!(cards[0].body.contains("$290/year"), "{brand}");
                assert!(!cards[0].body.contains("additional member"), "{brand}");
            }
            assert!(vesta[0].body.contains("death check"), "{host}");
        }
    }

    #[test]
    fn neon_home_omits_the_outside_practice_directory() {
        let company = home(&views::brand::DEFAULT_BRANDING)
            .company
            .expect("Neon home has the company section");
        let text = serde_yaml::to_string(&company).expect("company copy serializes");
        assert!(!text.contains("For life outside the company."));
        assert!(!text.contains("Neon Law also helps you find the right Guieb & Shook PC practice"));
        for retired in [
            "Debt-collection defense",
            "/delete-your-debt",
            "/immigration",
            "/delete-your-data",
            "/estate-planning",
            "/accidents",
            "/divorce",
        ] {
            assert!(
                !text.contains(retired),
                "the company home catalog must not publish {retired}"
            );
        }
    }

    /// The gateway's CTA resolves the CTA destination from the actual
    /// deployment host, the same way `home_for_host`'s sibling links do — not
    /// from a hardcoded string in the YAML.
    #[test]
    fn delete_your_debt_gateway_resolves_the_cta_per_deployment() {
        let production =
            delete_your_debt_gateway(&views::brand::DEFAULT_BRANDING, Some("www.neonlaw.com"));
        assert_eq!(
            production
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://www.deleteyourdebt.com"
        );

        let staging =
            delete_your_debt_gateway(&views::brand::DEFAULT_BRANDING, Some("staging.neonlaw.com"));
        assert_eq!(
            staging
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://staging.deleteyourdebt.com"
        );
    }

    /// The gateway's label names the destination, and it is Neon's own
    /// `MarketingPageCopy` (the practice skin, so its hero CTA actually
    /// renders — see `webapp::marketing_page`'s skin-gated hero).
    #[test]
    fn delete_your_debt_gateway_wears_the_practice_skin_and_names_the_destination() {
        let content = delete_your_debt_gateway(&views::brand::DEFAULT_BRANDING, None);
        assert!(matches!(
            content.skin,
            webapp::marketing_page::PageSkin::Practice
        ));
        assert_eq!(
            content.hero_cta.as_ref().expect("hero CTA").label,
            "Visit DeleteYourDebt.com"
        );
    }

    /// The `/delete-your-data` gateway's CTA resolves per deployment, the
    /// same way `/delete-your-debt`'s does.
    #[test]
    fn delete_your_data_gateway_resolves_the_cta_per_deployment() {
        let production =
            delete_your_data_gateway(&views::brand::DEFAULT_BRANDING, Some("www.neonlaw.com"));
        assert_eq!(
            production
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://www.deleteyourdata.com"
        );

        let staging =
            delete_your_data_gateway(&views::brand::DEFAULT_BRANDING, Some("staging.neonlaw.com"));
        assert_eq!(
            staging
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://staging.deleteyourdata.com"
        );
    }

    /// The gateway wears the practice skin, names the destination, and never
    /// claims every trace of a reader's personal information can be removed
    /// — the one overclaim ENG-898 specifically forbids.
    #[test]
    fn delete_your_data_gateway_never_claims_complete_removal() {
        let content = delete_your_data_gateway(&views::brand::DEFAULT_BRANDING, None);
        assert!(matches!(
            content.skin,
            webapp::marketing_page::PageSkin::Practice
        ));
        assert_eq!(
            content.hero_cta.as_ref().expect("hero CTA").label,
            "Visit DeleteYourData.com"
        );
        let text = format!("{} {}", content.hero_lead, content.title).to_lowercase();
        assert!(text.contains("deleteyourdata.com"), "{text}");
        for overclaim in [
            "remove all your",
            "erase everything",
            "every trace of your personal information can be erased",
            "guarantee",
        ] {
            assert!(!text.contains(overclaim), "overclaims removal: {text}");
        }
        assert!(
            text.contains("does not promise") || text.contains("cannot promise"),
            "and says so plainly: {text}"
        );
    }

    /// Every word of rendered band copy — statement and step prose plus
    /// the closing Cta — in reading order, for the copy guards.
    fn band_copy(content: &PageContent) -> String {
        let mut text = String::new();
        for band in &content.bands {
            match band {
                RenderedBand::Statement { body, .. } => {
                    for paragraph in body {
                        for run in paragraph {
                            text.push_str(&run.text);
                            text.push(' ');
                        }
                    }
                }
                RenderedBand::Steps { items, .. } => {
                    for step in items {
                        text.push_str(&step.title);
                        text.push(' ');
                        for paragraph in &step.body {
                            for run in paragraph {
                                text.push_str(&run.text);
                                text.push(' ');
                            }
                        }
                    }
                }
                RenderedBand::Cta { heading, body, .. } => {
                    text.push_str(heading);
                    if let Some(body) = body {
                        text.push(' ');
                        text.push_str(body);
                    }
                }
                _ => {}
            }
        }
        text
    }

    /// The `/immigration` gateway's CTA resolves per deployment, names
    /// Abhaya Immigration, never promises USCIS approval or timing, and
    /// never coins a fused "Neon Law Immigration" trade name.
    #[test]
    fn immigration_gateway_never_promises_uscis_outcomes_or_a_fused_trade_name() {
        let production =
            immigration_gateway(&views::brand::DEFAULT_BRANDING, Some("www.neonlaw.com"));
        assert_eq!(
            production
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://www.abhayaimmigration.com"
        );
        let staging =
            immigration_gateway(&views::brand::DEFAULT_BRANDING, Some("staging.neonlaw.com"));
        assert_eq!(
            staging
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://staging.abhayaimmigration.com"
        );

        // The landing closes on a Cta band, and it resolves to the same
        // deployment's destination as the hero button.
        for (host, expected) in [
            ("www.neonlaw.com", "https://www.abhayaimmigration.com"),
            (
                "staging.neonlaw.com",
                "https://staging.abhayaimmigration.com",
            ),
        ] {
            let page = immigration_gateway(&views::brand::DEFAULT_BRANDING, Some(host));
            let closing = page.bands.iter().find_map(|band| match band {
                RenderedBand::Cta { action, .. } => action.as_ref().map(|cta| cta.href.clone()),
                _ => None,
            });
            assert_eq!(
                closing.as_deref(),
                Some(expected),
                "the closing CTA must resolve to {expected} on {host}"
            );
        }

        let content = immigration_gateway(&views::brand::DEFAULT_BRANDING, None);
        assert!(matches!(
            content.skin,
            webapp::marketing_page::PageSkin::Practice
        ));
        assert_eq!(
            content.hero_cta.as_ref().expect("hero CTA").label,
            "Visit Abhaya Immigration"
        );
        // The landing is the map of the practice: every page beneath
        // `/immigration` is linked from its prose.
        let linked = content
            .bands
            .iter()
            .filter_map(|band| match band {
                RenderedBand::Statement { body, .. } => Some(body),
                _ => None,
            })
            .flatten()
            .flatten()
            .filter_map(|run| run.href.clone())
            .collect::<Vec<_>>();
        for path in [
            "/immigration/family-green-cards",
            "/immigration/citizenship",
            "/immigration/employment-visas",
            "/immigration/consular-processing",
            "/immigration/uscis-notices",
            "/immigration/humanitarian-relief",
            "/immigration/removal-defense",
            "/immigration/free-support",
        ] {
            assert!(
                linked.iter().any(|href| href == path),
                "the gateway must link {path}"
            );
        }
        let text = format!("{} {}", content.hero_lead, band_copy(&content)).to_lowercase();
        assert!(text.contains("abhaya immigration"), "{text}");
        // "neon law immigration" may appear only inside the explicit denial
        // sentence, keeping the firms distinct.
        for sentence in text.split('.') {
            if sentence.contains("neon law immigration") {
                assert!(
                    sentence.contains("not offered as"),
                    "fused trade name outside a denial: {sentence:?}"
                );
            }
        }
        for claim in [
            "guaranteed approval",
            "fast-track",
            "expedite your case",
            "approval is certain",
            "we win",
            "guarantee",
        ] {
            assert!(
                !text.contains(claim),
                "immigration gateway implies {claim:?}"
            );
        }
        assert!(
            text.contains("cannot promise"),
            "and says so plainly: {text}"
        );
    }

    /// Every practice stem the Neon brand publishes beneath `/immigration`.
    const IMMIGRATION_PRACTICE_STEMS: &[&str] = &[
        "immigration-family-green-cards",
        "immigration-citizenship",
        "immigration-employment-visas",
        "immigration-consular-processing",
        "immigration-uscis-notices",
        "immigration-humanitarian-relief",
        "immigration-removal-defense",
        "immigration-free-support",
    ];

    /// Each practice page resolves its closing CTA to Abhaya Immigration
    /// per deployment: production on www.neonlaw.com, staging on
    /// staging.neonlaw.com.
    #[test]
    fn immigration_practice_pages_resolve_the_abhaya_cta() {
        const PRODUCTION_HREF: &str = "https://www.abhayaimmigration.com";
        const STAGING_HREF: &str = "https://staging.abhayaimmigration.com";

        for stem in IMMIGRATION_PRACTICE_STEMS {
            for (deployment_host, expected) in [
                ("www.neonlaw.com", PRODUCTION_HREF),
                ("staging.neonlaw.com", STAGING_HREF),
            ] {
                let page = immigration_practice_page(
                    &views::brand::DEFAULT_BRANDING,
                    Some(deployment_host),
                    stem,
                );
                let cta_href = page.bands.iter().find_map(|band| match band {
                    RenderedBand::Cta { action, .. } => action.as_ref().map(|cta| cta.href.clone()),
                    _ => None,
                });
                assert_eq!(
                    cta_href.as_deref(),
                    Some(expected),
                    "{stem} CTA must resolve to {expected}"
                );
            }
        }
    }

    /// Every practice page names Abhaya Immigration, states plainly that it
    /// cannot promise outcomes, promises none anyway, and fuses the firms'
    /// names nowhere outside the denial sentence.
    #[test]
    fn immigration_practice_pages_promise_no_outcome_and_name_the_practice() {
        for stem in IMMIGRATION_PRACTICE_STEMS {
            let page = immigration_practice_page(
                &views::brand::DEFAULT_BRANDING,
                Some("www.neonlaw.com"),
                stem,
            );

            let text = band_copy(&page).to_lowercase();

            assert!(
                text.contains("abhaya immigration"),
                "{stem} never names the practice: {text}"
            );
            // "neon law immigration" may appear only inside the explicit
            // denial sentence, keeping the firms distinct.
            for sentence in text.split('.') {
                if sentence.contains("neon law immigration") {
                    assert!(
                        sentence.contains("not offered as"),
                        "{stem} fused trade name outside a denial: {sentence:?}"
                    );
                }
            }
            for claim in [
                "guaranteed approval",
                "fast-track",
                "expedite your case",
                "approval is certain",
                "we win",
                "guarantee",
            ] {
                assert!(!text.contains(claim), "{stem} implies {claim:?}");
            }
            assert!(
                text.contains("cannot promise"),
                "{stem} must say plainly that it cannot promise outcomes: {text}"
            );
        }
    }

    /// The `/estate-planning` gateway's CTA resolves per deployment, names
    /// Vesta Estate Planning, names tailored estate planning and Nevada trust
    /// assessment, and never promises a tax or creditor outcome.
    #[test]
    fn estate_planning_gateway_names_vesta_and_promises_no_outcome() {
        let production =
            estate_planning_gateway(&views::brand::DEFAULT_BRANDING, Some("www.neonlaw.com"));
        assert_eq!(
            production
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://www.vestaestateplanning.com"
        );
        let staging =
            estate_planning_gateway(&views::brand::DEFAULT_BRANDING, Some("staging.neonlaw.com"));
        assert_eq!(
            staging
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://staging.vestaestateplanning.com"
        );

        let content = estate_planning_gateway(&views::brand::DEFAULT_BRANDING, None);
        assert!(matches!(
            content.skin,
            webapp::marketing_page::PageSkin::Practice
        ));
        assert_eq!(
            content.hero_cta.as_ref().expect("hero CTA").label,
            "Visit Vesta Estate Planning"
        );
        let text = content.hero_lead.to_lowercase();
        assert!(text.contains("vesta estate planning"), "{text}");
        assert!(text.contains("nevada asset protection trust"), "{text}");
        assert!(text.contains("across the country"), "{text}");
        assert!(text.contains("applicable law permit"), "{text}");
        assert!(text.contains("estate or trust administration"), "{text}");
        assert!(text.contains("separate written terms"), "{text}");
        for promise in ["tax result", "avoid probate", "asset protection is"] {
            assert!(
                !text.contains(promise),
                "estate gateway promises {promise:?}"
            );
        }
        assert!(
            text.contains("no trust guarantees a particular tax or creditor result"),
            "and says so plainly: {text}"
        );
    }

    /// The `/accidents` gateway's CTA resolves per deployment, names
    /// Misericordia Injury Law, publishes no dollar figure, and never
    /// promises a recovery amount, speed, or an unqualified "no fee" result
    /// — "no fee" appears only alongside its "unless something is
    /// recovered" qualifier.
    #[test]
    fn accidents_gateway_never_promises_a_recovery_or_an_unqualified_no_fee() {
        let production =
            accidents_gateway(&views::brand::DEFAULT_BRANDING, Some("www.neonlaw.com"));
        assert_eq!(
            production
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://www.misericordialaw.com"
        );
        let staging =
            accidents_gateway(&views::brand::DEFAULT_BRANDING, Some("staging.neonlaw.com"));
        assert_eq!(
            staging
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://staging.misericordialaw.com"
        );

        let content = accidents_gateway(&views::brand::DEFAULT_BRANDING, None);
        assert!(matches!(
            content.skin,
            webapp::marketing_page::PageSkin::Practice
        ));
        assert_eq!(
            content.hero_cta.as_ref().expect("hero CTA").label,
            "Visit Misericordia Injury Law"
        );
        let text = content.hero_lead.to_lowercase();
        assert!(text.contains("misericordia injury law"), "{text}");
        assert!(!text.contains('$'), "publishes a dollar figure: {text}");
        for promise in [
            "we will recover",
            "guaranteed recovery",
            "quickly resolve",
            "fast settlement",
        ] {
            assert!(
                !text.contains(promise),
                "accidents gateway promises {promise:?}"
            );
        }
        // "no fee" (or "no attorney's fee") must carry its qualifier in the
        // same sentence — never an unqualified "no fee" claim.
        for sentence in text.split('.') {
            if sentence.contains("no fee") || sentence.contains("no attorney") {
                assert!(
                    sentence.contains("unless") || sentence.contains("recovered"),
                    "unqualified no-fee claim: {sentence:?}"
                );
            }
        }
        assert!(
            text.contains("we do not promise a recovery amount"),
            "and says so plainly: {text}"
        );
    }

    /// The `/divorce` gateway's CTA resolves per deployment, names Daybridge
    /// Divorce Law, preserves the one-time mutual-split fee, and never
    /// promises a result or timeline.
    #[test]
    fn divorce_gateway_preserves_pricing_and_promises_no_outcome() {
        let production = divorce_gateway(&views::brand::DEFAULT_BRANDING, Some("www.neonlaw.com"));
        assert_eq!(
            production
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://www.daybridgedivorce.com"
        );
        let staging = divorce_gateway(&views::brand::DEFAULT_BRANDING, Some("staging.neonlaw.com"));
        assert_eq!(
            staging
                .hero_cta
                .as_ref()
                .expect("gateway carries a hero CTA")
                .href,
            "https://staging.daybridgedivorce.com"
        );

        let content = divorce_gateway(&views::brand::DEFAULT_BRANDING, None);
        assert!(matches!(
            content.skin,
            webapp::marketing_page::PageSkin::Practice
        ));
        assert_eq!(
            content.hero_cta.as_ref().expect("hero CTA").label,
            "Visit Daybridge Divorce Law"
        );
        let text = content.hero_lead.to_lowercase();
        assert!(text.contains("daybridge divorce law"), "{text}");
        assert!(text.contains("one-time $5,000 fee"), "{text}");
        assert!(text.contains("mutual split"), "{text}");
        for stale_fee in ["$500 setup", "$10 a day", "$50 each", "$5,000 per day"] {
            assert!(!text.contains(stale_fee), "{text}");
        }
        assert!(!text.contains("we guarantee"), "{text}");
        assert!(
            text.contains("no result or timeline is promised"),
            "and says so plainly: {text}"
        );
    }

    // --- ENG-744…749: the practice brands, and the lines that bind them ---

    /// Every word a practice brand publishes, home and services together, so
    /// a constraint is checked against the whole site rather than one page.
    fn brand_text(key: views::brand::BrandKey) -> String {
        let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
        let home = home(branding);
        let mut text = vec![
            home.head_title.clone(),
            home.meta_description.clone(),
            home.heading.clone(),
            home.lead.clone(),
        ];
        if let Some(service) = home.service.as_ref() {
            text.extend(service.body.iter().flatten().map(|run| run.text.clone()));
        }
        if let Some(estate) = home.estate.as_ref() {
            text.push(serde_yaml::to_string(estate).expect("estate copy serializes"));
        }
        for practice in &home.practices {
            text.push(practice.heading.clone());
            text.push(practice.body.clone());
        }
        text.push(dyd_page_text(&legal_services(branding)));
        for (_, stem) in key.explainer_pages() {
            text.push(dyd_page_text(&explainer(branding, stem)));
        }
        text.push(branding.mission_description.to_string());
        text.push(branding.service_description.to_string());
        text.join(" ")
    }

    const PRACTICE_BRANDS: &[views::brand::BrandKey] = &[
        views::brand::BrandKey::Aegis,
        views::brand::BrandKey::Groundwork,
        views::brand::BrandKey::Vesta,
        views::brand::BrandKey::Misericordia,
        views::brand::BrandKey::Abhaya,
        views::brand::BrandKey::DeleteYourDebt,
        views::brand::BrandKey::Summons,
    ];

    /// Every practice brand loads every catalog it declares. A missing file
    /// is a test failure here rather than a panic on a visitor's first
    /// request.
    #[test]
    fn every_practice_brand_publishes_the_pages_it_declares() {
        for key in PRACTICE_BRANDS {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            for stem in key.catalog_pages() {
                let yaml = catalog_yaml(*key, stem).expect("declared catalog exists");
                views::locales::parse_locale_file(stem, yaml).expect("typed catalog");
            }
            assert!(
                !home(branding).heading.is_empty(),
                "{} home renders",
                key.as_str()
            );
            assert!(
                !dyd_page_text(&legal_services(branding)).is_empty(),
                "{} services renders",
                key.as_str()
            );
            for (_, stem) in key.explainer_pages() {
                let content = explainer(branding, stem);
                assert!(!dyd_page_text(&content).is_empty());
                assert_eq!(
                    content.hero_cta.expect("booking CTA").href,
                    branding.consultation_url
                );
            }
        }
    }

    /// No brand promises a result, anywhere. Attorney advertising rules make
    /// this the one claim none of these sites may make, whatever the
    /// practice area. Preserve an explicit denial that a result is guaranteed
    /// without treating that disclaimer as an affirmative promise.
    #[test]
    fn no_practice_brand_promises_an_outcome() {
        for key in PRACTICE_BRANDS {
            let text = brand_text(*key)
                .to_lowercase()
                .replace("does not guarantee that", "");
            for promise in [
                "we guarantee",
                "guaranteed result",
                "we will win",
                "you will win",
                "we always win",
                "guarantee that",
            ] {
                assert!(
                    !text.contains(promise),
                    "{} promises an outcome: {promise:?}",
                    key.as_str()
                );
            }
        }
    }

    /// **18 U.S.C. § 706.** The Misericordia of Florence carried a red cross,
    /// and that emblem later became the protected symbol of medical care. It
    /// is not available to this firm, in any form — so the brand's words
    /// never reach for it either, which is where it would creep back in once
    /// the palette is settled.
    #[test]
    fn misericordia_never_reaches_for_the_red_cross() {
        let text = brand_text(views::brand::BrandKey::Misericordia).to_lowercase();
        for emblem in ["red cross", "redcross", "cross symbol", "crimson cross"] {
            assert!(!text.contains(emblem), "Misericordia names {emblem:?}");
        }
    }

    /// No case results and no dollar figures on Misericordia. A headline
    /// number is not cured by the advertising disclaimer beneath it, so the
    /// fee page explains the contingency in words and publishes no amount.
    #[test]
    fn misericordia_publishes_no_recovery_figures() {
        let text = brand_text(views::brand::BrandKey::Misericordia);
        assert!(
            !text.contains('$'),
            "Misericordia publishes a dollar figure: {text}"
        );
        // Boasts, not the word "recover": describing *how the fee works* —
        // a percentage of what is recovered — is the honest disclosure this
        // practice owes. What is banned is a past result offered as a
        // prediction, which the advertising disclaimer does not cure.
        for boast in [
            "we have recovered",
            "we've recovered",
            "million",
            "verdict",
            "record settlement",
            "results speak",
            "won over",
        ] {
            assert!(
                !text.to_lowercase().contains(boast),
                "Misericordia publishes a case result: {boast:?}"
            );
        }
        // What it must say instead: the contingency, honestly, including the
        // losing case.
        assert!(text.contains("no fee unless we recover") || text.contains("owe us no attorney"));
    }

    /// Abhaya may never imply it can secure or speed a USCIS decision.
    #[test]
    fn abhaya_never_implies_it_controls_uscis() {
        let text = brand_text(views::brand::BrandKey::Abhaya).to_lowercase();
        for claim in [
            "we can get you a visa",
            "guaranteed approval",
            "fast-track",
            "expedite your case",
            "we can speed",
            "approval is certain",
        ] {
            assert!(!text.contains(claim), "Abhaya implies {claim:?}");
        }
        assert!(
            text.contains("cannot promise"),
            "and says so plainly: {text}"
        );
    }

    /// The debt site connects visitors to `PandA` without promising an outcome.
    #[test]
    fn delete_your_debt_connects_four_states_to_panda() {
        let branding = &views::brand::DELETE_YOUR_DEBT_BRANDING;
        let text = brand_text(views::brand::BrandKey::DeleteYourDebt);
        for required in [
            "PandA Law",
            "Nevada",
            "Washington",
            "New York",
            "California",
            "Brooklyn",
        ] {
            assert!(text.contains(required), "missing {required}");
        }
        for obsolete in [
            "We do not settle debts",
            "We are not a debt settlement",
            "We defend, we do not settle",
        ] {
            assert!(!text.contains(obsolete), "obsolete scope: {obsolete}");
        }
        assert_eq!(
            home(branding).contact_href,
            "https://www.pandalawfirm.com/contact/"
        );
        assert!(text.contains("does not guarantee"));
    }

    /// The Summons catalog must not read as the tribunal it appears before.
    /// Keep this invariant at the locale boundary: inspect the catalog keys
    /// and values directly, while route and rendered-page behavior belongs to
    /// the firm-page and server tests.
    #[test]
    fn the_summons_catalog_disclaims_affiliation_with_the_city() {
        let value = |page: &str, key: &str| {
            let yaml = catalog_yaml(BrandKey::Summons, page).expect("Summons catalog page");
            let document: serde_yaml::Value =
                serde_yaml::from_str(yaml).expect("Summons catalog parses");
            document[key]
                .as_str()
                .unwrap_or_else(|| panic!("Summons {page} catalog key {key:?} is text"))
                .to_string()
        };

        let home_lead = home(&views::brand::SUMMONS_BRANDING)
            .summons
            .expect("Summons content")
            .hero_note;
        assert!(
            home_lead.contains("not affiliated with the City of New York"),
            "home.summons.hero_note: {home_lead}"
        );
        let services_hero_lead = value("services", "hero_lead");
        assert!(
            services_hero_lead.contains("not affiliated with the City of New York"),
            "services.hero_lead: {services_hero_lead}"
        );
    }

    /// Every public brand says whose practice it is. That disclosure is what
    /// keeps a trade name from being misleading, so it is derived from the
    /// brand rather than listed.
    #[test]
    fn every_public_brand_names_the_firm_behind_it() {
        for key in views::brand::BrandKey::ALL {
            let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
            assert_eq!(
                branding.firm.legal_entity,
                "Guieb & Shook PC",
                "{} names the firm",
                key.as_str()
            );
            assert_ne!(
                branding.firm.site_name,
                branding.firm.legal_entity,
                "{} publishes a public brand name distinct from its legal entity",
                key.as_str()
            );
        }
    }

    /// Nothing in this family loads a third-party font or analytics host.
    #[test]
    fn no_practice_brand_copy_references_a_third_party_host() {
        for key in PRACTICE_BRANDS {
            let text = brand_text(*key).to_lowercase();
            for host in [
                "fonts.googleapis.com",
                "fonts.gstatic.com",
                "google-analytics",
                "googletagmanager",
                "facebook.net",
            ] {
                assert!(!text.contains(host), "{} references {host}", key.as_str());
            }
        }
    }
}
