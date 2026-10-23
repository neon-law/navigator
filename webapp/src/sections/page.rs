//! `/app/projects/{code}/{slug}` — one declared [Matter
//! Page](../../../docs/glossary/matter-page.md), through the reader's own
//! [Lens](../../../docs/glossary/lens.md).
//!
//! The lens is a fact about the person, never about the URL: the loader
//! re-checks participation exactly as the matter show page does
//! (`store::access::matter_viewer`) and derives the lens from it. A
//! non-participant, an unknown slug, and a page that declares no face for
//! this reader are the same committed `404`, so the response never confirms
//! that a matter or a page exists.
//!
//! The pages come from the Project's synced `matter_page` rows, never from
//! git. The tab strip ([`CaseNav`]) is generated from the same rows: the show
//! page is always the first tab, then each page that has a face for this
//! reader, in declared order.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{PageLens, SectionBody, SectionView};
use crate::people::ViewerRole;

/// One tab of the matter's page strip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageTab {
    pub label: String,
    pub href: String,
}

/// Everything one rendered page needs, display-ready. An empty `title`
/// means not found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MatterPageView {
    pub code: String,
    pub matter_name: String,
    pub slug: String,
    pub title: String,
    pub lens: PageLens,
    pub sections: Vec<SectionView>,
    pub tabs: Vec<PageTab>,
    pub role: ViewerRole,
    pub logo: Option<crate::components::AppLogo>,
    pub tokens_href: String,
}

impl MatterPageView {
    fn not_found(role: ViewerRole) -> Self {
        Self {
            code: String::new(),
            matter_name: String::new(),
            slug: String::new(),
            title: String::new(),
            lens: PageLens::Client,
            sections: Vec::new(),
            tabs: Vec::new(),
            role,
            logo: None,
            tokens_href: String::new(),
        }
    }
}

/// One link per page with a face for `lens`, in declared order. The single
/// answer to "which pages can this reader open", shared by the tab strip and
/// the Project portal banner so neither can link a page the route would 404.
#[cfg(feature = "server")]
#[must_use]
pub fn page_links(code: &str, pages: &[rules::pages::PageEntry], lens: PageLens) -> Vec<PageTab> {
    pages
        .iter()
        .filter(|page| page.lenses.contains_key(lens.as_str()))
        .map(|page| PageTab {
            label: page.title.clone(),
            href: format!("/app/projects/{code}/{}", page.slug),
        })
        .collect()
}

/// The page strip for a matter: the show page first, then each page with a
/// face for `lens`. A pure function of the stored entries, so the strip and
/// the route can never disagree about which pages a reader has.
#[cfg(feature = "server")]
#[must_use]
pub fn page_tabs(code: &str, pages: &[rules::pages::PageEntry], lens: PageLens) -> Vec<PageTab> {
    let mut tabs = vec![PageTab {
        label: rules::pages::SHOW_PAGE_TITLE.to_string(),
        href: format!("/app/projects/{code}"),
    }];
    tabs.extend(page_links(code, pages, lens));
    tabs
}

/// The lens a matter viewer reads pages through.
#[cfg(feature = "server")]
#[must_use]
pub fn lens_for_viewer(viewer: store::access::MatterViewer) -> PageLens {
    use store::access::MatterViewer;
    match viewer {
        MatterViewer::Client | MatterViewer::ClientDri => PageLens::Client,
        MatterViewer::Clerk => PageLens::Clerk,
        MatterViewer::Lawyer | MatterViewer::LawyerDri => PageLens::Lawyer,
    }
}

/// The lens `person_id` reads `project_id`'s pages through: the same
/// participation answer the show page dispatches on. `None` when the person
/// is not on the matter, which is a `404` for every tier.
///
/// # Errors
///
/// The store's message when the participation lookup fails.
#[cfg(feature = "server")]
pub async fn matter_page_lens(
    surreal: &store::surreal::SurrealDb,
    person_id: Option<uuid::Uuid>,
    role: store::persons::Role,
    project_id: uuid::Uuid,
) -> Result<Option<PageLens>, String> {
    Ok(
        store::access::matter_viewer(surreal, person_id, role, project_id)
            .await?
            .map(lens_for_viewer),
    )
}

/// The page strip for the matter show page: [`page_tabs`] over the
/// Project's stored pages, or nothing when it declares none. A read failure
/// drops the strip rather than the show page.
#[cfg(feature = "server")]
pub async fn stored_page_tabs(
    surreal: &store::surreal::SurrealDb,
    project_id: uuid::Uuid,
    code: &str,
    lens: PageLens,
) -> Vec<PageTab> {
    let Ok(stored) = store::matter_pages::list_for_project(surreal, project_id).await else {
        return Vec::new();
    };
    if stored.is_empty() {
        return Vec::new();
    }
    let entries: Vec<rules::pages::PageEntry> = stored.into_iter().map(|page| page.entry).collect();
    page_tabs(code, &entries, lens)
}

/// The sections `slug` shows on `lens`, in declared order, or `None` when the
/// slug names no stored page or the page declares no face for this lens.
#[cfg(feature = "server")]
#[must_use]
pub fn page_sections(
    pages: &[rules::pages::PageEntry],
    slug: &str,
    lens: PageLens,
) -> Option<(rules::pages::PageEntry, Vec<rules::dashboard::Section>)> {
    let rules_lens = match lens {
        PageLens::Client => rules::dashboard::Lens::Client,
        PageLens::Lawyer => rules::dashboard::Lens::Lawyer,
        PageLens::Clerk => rules::dashboard::Lens::Clerk,
    };
    let page = pages.iter().find(|page| page.slug == slug)?;
    let mut sections = page.sections_for(rules_lens)?;
    if !sections.contains(&rules::dashboard::Section::ProjectCosts) {
        if let Some(invoice) = sections
            .iter()
            .position(|section| *section == rules::dashboard::Section::Invoice)
        {
            sections.insert(invoice + 1, rules::dashboard::Section::ProjectCosts);
        } else {
            sections.push(rules::dashboard::Section::ProjectCosts);
        }
    }
    Some((page.clone(), sections))
}

#[cfg(feature = "server")]
fn commit_not_found() {
    dioxus_fullstack_core::FullstackContext::commit_http_status(
        axum::http::StatusCode::NOT_FOUND,
        None,
    );
}

/// The page's data for the caller, or a not-found view with a committed 404.
#[server]
pub async fn get_matter_page() -> Result<MatterPageView, ServerFnError> {
    let role = dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<ViewerRole>, _>()
        .await
        .map(|axum::Extension(role)| role)
        .unwrap_or_default();
    let person_id = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::portal_project_list::PersonId>,
        _,
    >()
    .await
    .ok()
    .and_then(|axum::Extension(pid)| pid.0)
    .and_then(|raw| raw.parse::<uuid::Uuid>().ok());
    let Ok(axum::extract::Path((code, slug))) = dioxus_fullstack_core::FullstackContext::extract::<
        axum::extract::Path<(String, String)>,
        _,
    >()
    .await
    else {
        commit_not_found();
        return Ok(MatterPageView::not_found(role));
    };
    let store_role = match role {
        ViewerRole::Owner => store::persons::Role::Owner,
        ViewerRole::Admin => store::persons::Role::Admin,
        ViewerRole::Lawyer => store::persons::Role::Lawyer,
        ViewerRole::Clerk => store::persons::Role::Clerk,
        ViewerRole::Client => store::persons::Role::Client,
    };
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let Some(project) = store::projects::find_by_code(&surreal, &code)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?
    else {
        commit_not_found();
        return Ok(MatterPageView::not_found(role));
    };
    // The same participation answer the show page dispatches on. No row is a
    // 404 for every tier here: a declared page is matter content, which the
    // unassigned-admin participation page never discloses.
    let Some(lens) = matter_page_lens(&surreal, person_id, store_role, project.id)
        .await
        .map_err(ServerFnError::new)?
    else {
        commit_not_found();
        return Ok(MatterPageView::not_found(role));
    };
    let stored = store::matter_pages::list_for_project(&surreal, project.id)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let synced_at = stored
        .first()
        .map(|page| page.synced_at.clone())
        .unwrap_or_default();
    let entries: Vec<rules::pages::PageEntry> = stored.into_iter().map(|page| page.entry).collect();
    let Some((page, sections)) = page_sections(&entries, &slug, lens) else {
        commit_not_found();
        return Ok(MatterPageView::not_found(role));
    };

    let query = dioxus_fullstack_core::FullstackContext::extract::<axum::extract::RawQuery, _>()
        .await
        .ok()
        .and_then(|axum::extract::RawQuery(query)| query)
        .unwrap_or_default();
    let cx = load_context(
        &surreal,
        &project,
        lens,
        PageAddress {
            synced_at,
            slug: slug.clone(),
            draft: page.draft.clone(),
            document_on_page: sections.contains(&rules::dashboard::Section::NotationDocument),
            side: side_filter(&query),
            discovery: super::discovery::discovery_filter(&query),
        },
    )
    .await?;
    Ok(MatterPageView {
        tabs: page_tabs(&project.code, &entries, lens),
        code: project.code,
        matter_name: project.name,
        slug,
        title: page.title,
        lens,
        sections: sections
            .into_iter()
            .map(|section| super::load(section, &cx))
            .collect(),
        role,
        logo: crate::app_chrome::app_logo_from_context().await,
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
    })
}

/// The `side` value of a page's query string, e.g. `side=ours`.
#[must_use]
pub fn side_filter(query: &str) -> super::SideFilter {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == "side")
        .map_or_else(super::SideFilter::default, |(_, value)| {
            super::SideFilter::parse(value)
        })
}

/// Where the reader is: the stored page and the filters they asked for.
#[cfg(feature = "server")]
struct PageAddress {
    synced_at: String,
    slug: String,
    side: super::SideFilter,
    /// The page's `draft:` path, which names the Notation it renders.
    draft: Option<String>,
    /// The page shows `notation_document` on this lens, so checklist steps
    /// link to their paragraphs.
    document_on_page: bool,
    discovery: super::DiscoveryFilter,
}

/// Who is on the matter: the client Entity's name and each participant as
/// `(name, participation)`, in participation order.
#[cfg(feature = "server")]
struct MatterPeople {
    client_entity: String,
    team: Vec<(String, String)>,
}

#[cfg(feature = "server")]
impl MatterPeople {
    /// The names that file for our side: the client Entity and every client
    /// or lawyer participant.
    fn our_names(&self) -> Vec<String> {
        std::iter::once(self.client_entity.clone())
            .chain(
                self.team
                    .iter()
                    .filter(|(_, participation)| {
                        matches!(participation.as_str(), "client" | "lawyer")
                    })
                    .map(|(name, _)| name.clone()),
            )
            .filter(|name| !name.is_empty())
            .collect()
    }
}

#[cfg(feature = "server")]
async fn matter_people(
    surreal: &store::surreal::SurrealDb,
    project: &store::projects::Project,
) -> Result<MatterPeople, String> {
    let client_entity = store::entities::find_by_id(surreal, project.entity_id)
        .await
        .map_err(|e| e.to_string())?
        .map(|entity| entity.name)
        .unwrap_or_default();
    let rows = store::projects::participations_for_project(surreal, project.id)
        .await
        .map_err(|e| e.to_string())?;
    let ids: Vec<uuid::Uuid> = rows.iter().map(|row| row.person_id).collect();
    let people = store::persons::find_by_ids(surreal, &ids)
        .await
        .map_err(|e| e.to_string())?;
    let team = rows
        .into_iter()
        .filter_map(|row| {
            let person = people.iter().find(|person| person.id == row.person_id)?;
            Some((person.name.clone(), row.participation))
        })
        .collect();
    Ok(MatterPeople {
        client_entity,
        team,
    })
}

/// Each Project Reference as `(name, reason)`, oldest first.
#[cfg(feature = "server")]
async fn references(
    surreal: &store::surreal::SurrealDb,
    project_id: uuid::Uuid,
) -> Result<Vec<(String, String)>, String> {
    use store::relationships::Endpoint;

    let rows = store::project_references::for_project(surreal, project_id)
        .await
        .map_err(|e| e.to_string())?;
    let ids = |kind: Endpoint| -> Vec<uuid::Uuid> {
        rows.iter()
            .filter(|row| row.party.kind == kind)
            .map(|row| row.party.id)
            .collect()
    };
    let people = store::persons::find_by_ids(surreal, &ids(Endpoint::Person))
        .await
        .map_err(|e| e.to_string())?;
    let entities = store::entities::find_by_ids(surreal, &ids(Endpoint::Entity))
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let name = match row.party.kind {
                Endpoint::Person => people
                    .iter()
                    .find(|person| person.id == row.party.id)
                    .map(|person| person.name.clone()),
                Endpoint::Entity => entities
                    .iter()
                    .find(|entity| entity.id == row.party.id)
                    .map(|entity| entity.name.clone()),
            }?;
            Some((name, row.reason.clone()))
        })
        .collect())
}

/// Each data source a section reads, fetched once and scoped to `lens`. A
/// clerk never reaches documents, deadlines, the docket, or billing, so the
/// clerk lens loads none of them.
#[cfg(feature = "server")]
async fn load_context(
    surreal: &store::surreal::SurrealDb,
    project: &store::projects::Project,
    lens: PageLens,
    address: PageAddress,
) -> Result<super::LoadContext, ServerFnError> {
    let mut cx = super::LoadContext::empty(lens);
    cx.timeline.page_href = format!("/app/projects/{}/{}", project.code, address.slug);
    cx.timeline.filter = address.side;
    cx.discovery.page_href.clone_from(&cx.timeline.page_href);
    cx.discovery.filter = address.discovery;
    cx.synced_at = address.synced_at;
    cx.slug = address.slug;
    if lens == PageLens::Clerk {
        return Ok(cx);
    }
    let storage = consume_context::<std::sync::Arc<dyn cloud::StorageService>>();
    cx.notations = super::notations::load_notations(
        surreal,
        &storage,
        &super::notations::NotationsSource {
            project_id: project.id,
            code: &project.code,
            draft: address.draft.as_deref(),
            lens,
            document_on_page: address.document_on_page,
        },
    )
    .await
    .map_err(ServerFnError::new)?;
    let assets = store::assets::for_project(surreal, project.id)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    cx.page_draft =
        super::engagement::load_page_draft(surreal, &storage, project.id, address.draft.as_deref())
            .await
            .map_err(ServerFnError::new)?;
    let runs = super::engagement::onboarding_runs(surreal, project.id)
        .await
        .map_err(ServerFnError::new)?;
    (cx.engagement_filed_any, cx.engagement_letters) =
        super::engagement::filed_letters(&project.code, &assets, &runs, lens);
    load_docket(surreal, project, &assets, &mut cx)
        .await
        .map_err(ServerFnError::new)?;
    load_records(surreal, project.id, &project.code, assets, &mut cx)
        .await
        .map_err(ServerFnError::new)?;
    cx.authorities.cards = super::authorities::derive::load(surreal, project.id, lens)
        .await
        .map_err(ServerFnError::new)?;
    // The relationship graph is the firm's conflicts record.
    if lens == PageLens::Lawyer {
        cx.graph = super::graph::derive::load(surreal, project)
            .await
            .map_err(ServerFnError::new)?;
    }
    super::instruments::load_sections(
        surreal,
        Some(&storage),
        project,
        super::instruments::OnPage {
            draft: address.draft.as_deref(),
            document_on_page: address.document_on_page,
        },
        &mut cx,
    )
    .await
    .map_err(ServerFnError::new)?;
    Ok(cx)
}

/// The caption, the timeline, its fact strip and key dates, the deadline
/// board, the party roster, and the discovery pairs.
#[cfg(feature = "server")]
async fn load_docket(
    surreal: &store::surreal::SurrealDb,
    project: &store::projects::Project,
    assets: &[store::assets::Asset],
    cx: &mut super::LoadContext,
) -> Result<(), String> {
    let client_lens = cx.lens == PageLens::Client;
    let now = chrono::Utc::now();
    let deadlines = store::project_deadlines::list(surreal, project.id)
        .await
        .map_err(|e| e.to_string())?;
    let cases = store::cases::for_project(surreal, project.id)
        .await
        .map_err(|e| e.to_string())?;
    let entries = store::cases::docket_for_project(surreal, project.id)
        .await
        .map_err(|e| e.to_string())?;
    let people = matter_people(surreal, project).await?;
    let matter = store::matter_captions::for_project(surreal, project.id)
        .await
        .map_err(|e| e.to_string())?
        .map(|caption| caption.matter);
    cx.caption = super::derive::caption(&cases, matter.as_ref(), &project.name);
    let docket = super::derive::docket(&super::derive::DocketInputs {
        cases: &cases,
        entries: &entries,
        deadlines: &deadlines,
        assets,
        our_names: &people.our_names(),
        code: &project.code,
        client_lens,
        now,
    });
    cx.deadlines = deadline_section_rows(&deadlines, now, cx.lens);
    // The service list and the references are the firm's conflicts record;
    // the client lens never loads them.
    let (services, references) = if client_lens {
        (Vec::new(), Vec::new())
    } else {
        (
            store::project_service::list(surreal, project.id)
                .await
                .map_err(|e| e.to_string())?,
            references(surreal, project.id).await?,
        )
    };
    cx.roster = super::derive::roster(&super::derive::RosterInputs {
        matter: matter.as_ref(),
        client_entity: &people.client_entity,
        team: &people.team,
        items: &docket.items,
        services: &services,
        references: &references,
        client_lens,
    });
    cx.discovery.pairs =
        super::discovery::derive::load(surreal, &cases, &people.our_names(), cx.lens).await?;
    cx.timeline.items = docket.items;
    cx.facts = docket.facts;
    cx.key_dates = docket.key_dates;
    Ok(())
}

/// Files, e-filing receipts, and invoices.
#[cfg(feature = "server")]
async fn load_records(
    surreal: &store::surreal::SurrealDb,
    project_id: uuid::Uuid,
    project_code: &str,
    assets: Vec<store::assets::Asset>,
    cx: &mut super::LoadContext,
) -> Result<(), String> {
    let client_lens = cx.lens == PageLens::Client;
    cx.receipts = super::derive::receipts(
        &store::efiling_transactions::list(surreal, project_id)
            .await
            .map_err(|e| e.to_string())?,
        &assets,
        client_lens,
    );
    cx.documents = assets
        .into_iter()
        // A client reads only what a lawyer marked client-visible, the same
        // rule the show page's document list applies.
        .filter(|asset| !client_lens || asset.visibility == store::documents::visibility::CLIENT)
        .map(|asset| super::SectionRow {
            label: asset
                .filename
                .clone()
                .or(asset.slug.clone())
                .unwrap_or_default(),
            detail: String::new(),
            status: String::new(),
        })
        .collect();
    match store::postgres::PostgresDb::from_env().await {
        Ok(pg) => {
            let costs = if client_lens {
                store::project_costs::list_client_visible(surreal, &pg, project_id).await
            } else {
                store::project_costs::list_for_project(&pg, project_id).await
            };
            match costs {
                Ok(costs) => {
                    let mut totals = BTreeMap::<(String, String), i64>::new();
                    for cost in &costs {
                        let total = totals
                            .entry((cost.amount_status.clone(), cost.currency.clone()))
                            .or_default();
                        *total = total.saturating_add(cost.amount_cents);
                    }
                    cx.project_costs.totals = totals
                        .into_iter()
                        .map(
                            |((amount_status, currency), cents)| super::ProjectCostTotal {
                                amount_status,
                                currency,
                                amount: format!("{}.{:02}", cents / 100, cents % 100),
                            },
                        )
                        .collect();
                    cx.project_costs.rows = costs
                        .into_iter()
                        .map(|cost| {
                            let whole = cost.amount_cents / 100;
                            let fractional = cost.amount_cents % 100;
                            super::ProjectCostRow {
                                amount: format!("{} {}.{:02}", cost.currency, whole, fractional),
                                detail: format!(
                                    "{} · {} — {}",
                                    cost.occurred_on, cost.payee, cost.description
                                ),
                                status: format!("{} · {}", cost.amount_status, cost.billing_status),
                                receipt_href: format!(
                                    "/app/projects/{project_code}/documents/{}",
                                    cost.receipt_document_id
                                ),
                            }
                        })
                        .collect();
                }
                Err(_) => cx.project_costs.unavailable = true,
            }
        }
        Err(_) => cx.project_costs.unavailable = true,
    }
    cx.invoices = store::xero_invoices::for_projects(surreal, &[project_id])
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|invoice| {
            let paid =
                invoice.amount_cents > 0 && invoice.amount_paid_cents >= invoice.amount_cents;
            super::SectionRow {
                label: crate::portal_project_detail::format_usd(invoice.amount_cents),
                detail: format!("Issued {}", invoice.issued_at.format("%Y-%m-%d")),
                status: if paid { "Paid" } else { "Due" }.to_string(),
            }
        })
        .collect();
    Ok(())
}

/// The deadline rows a section shows on `lens`: open deadlines, soonest
/// first, each marked hard or soft, with a client seeing only those a lawyer
/// gave a client lead. A clerk sees none. The route and `navigator project
/// preview` both build their rows here, so the two cannot disagree about
/// what a lens shows.
#[cfg(feature = "server")]
#[must_use]
pub fn deadline_section_rows(
    deadlines: &[store::project_deadlines::Deadline],
    now: chrono::DateTime<chrono::Utc>,
    lens: PageLens,
) -> Vec<super::DeadlineCard> {
    if lens == PageLens::Clerk {
        return Vec::new();
    }
    super::derive::deadline_board(deadlines, now, lens == PageLens::Client)
}

/// [`MatterPageBody`] rendered to an HTML fragment. `navigator project
/// preview` draws its draft banner around this, so the local preview shows
/// the page the route renders, from the same components.
#[cfg(feature = "server")]
#[must_use]
pub fn render_matter_page_body(view: MatterPageView) -> String {
    let mut dom = VirtualDom::new_with_props(MatterPageBody, MatterPageBodyProps { view });
    dom.rebuild_in_place();
    dioxus_ssr::render(&dom)
}

/// The matter's page strip. Renders nothing when the matter declares no page
/// this reader has, so a matter without `pages:` keeps its show page as is.
#[component]
pub fn CaseNav(tabs: Vec<PageTab>, current: String) -> Element {
    if tabs.len() < 2 {
        return rsx! {};
    }
    let tabs: Vec<crate::components::Tab> = tabs
        .into_iter()
        .map(|tab| {
            let selected = tab.href == current;
            crate::components::Tab::new(tab.label, tab.href, selected)
        })
        .collect();
    rsx! { crate::components::Tabs { aria_label: "Matter pages".to_string(), tabs } }
}

/// The page body, prop-driven so SSR tests render it without a server future.
#[component]
pub fn MatterPageBody(view: MatterPageView) -> Element {
    if view.title.is_empty() {
        return rsx! {
            main { id: "matter-page", class: "nav-theme",
                h1 { "Not found" }
                p { "No page is available at this address." }
            }
        };
    }
    let current = format!("/app/projects/{}/{}", view.code, view.slug);
    let back = format!("/app/projects/{}", view.code);
    rsx! {
        document::Title { "{view.matter_name} | {view.title}" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        if !view.tokens_href.is_empty() {
            document::Stylesheet { href: "{view.tokens_href}" }
        }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(view.role),
            logo: view.logo.clone(),
        }
        main {
            id: "matter-page",
            class: "nav-theme portal-detail",
            "data-lens": view.lens.as_str(),
            nav { class: "portal-detail__back",
                a { class: "nav-link", href: "{back}", "← {view.matter_name}" }
            }
            h1 { "{view.title}" }
            CaseNav { tabs: view.tabs.clone(), current }
            for section in view.sections.iter() {
                SectionBody { view: section.clone() }
            }
        }
    }
}

/// `/app/projects/{code}/{slug}`.
#[component]
pub fn MatterPage() -> Element {
    let resource = use_server_future(get_matter_page)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        _ => MatterPageView::not_found(ViewerRole::default()),
    };
    rsx! { MatterPageBody { view } }
}

#[cfg(test)]
mod tests {
    use super::{MatterPageBody, MatterPageView, PageTab};
    use crate::people::ViewerRole;
    use crate::sections::{NoteView, PageLens, SectionView};
    use dioxus::prelude::*;

    fn render(view: MatterPageView) -> String {
        let mut dom =
            VirtualDom::new_with_props(MatterPageBody, super::MatterPageBodyProps { view });
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    fn view() -> MatterPageView {
        MatterPageView {
            code: "sample-litigation".into(),
            matter_name: "Cruller v. Prine".into(),
            slug: "timeline".into(),
            title: "Case timeline".into(),
            lens: PageLens::Client,
            sections: vec![
                SectionView::KeyDatesStrip(Vec::new()),
                SectionView::BoundaryNote(NoteView {
                    text: "A summary.".into(),
                }),
            ],
            tabs: vec![
                PageTab {
                    label: "Overview".into(),
                    href: "/app/projects/sample-litigation".into(),
                },
                PageTab {
                    label: "Case timeline".into(),
                    href: "/app/projects/sample-litigation/timeline".into(),
                },
            ],
            role: ViewerRole::Client,
            logo: None,
            tokens_href: String::new(),
        }
    }

    #[test]
    fn a_page_renders_its_sections_in_declared_order_under_its_tabs() {
        let html = render(view());
        let dates = html.find("data-section=\"key_dates_strip\"").unwrap();
        let note = html.find("data-section=\"boundary_note\"").unwrap();
        assert!(dates < note, "declared order: {html}");
        assert!(html.contains("data-lens=\"client\""), "{html}");
        assert!(html.contains("aria-current=\"page\""), "{html}");
        assert!(
            html.contains("href=\"/app/projects/sample-litigation\""),
            "{html}"
        );
    }

    #[test]
    fn a_not_found_view_renders_no_matter_content() {
        let mut missing = view();
        missing.title = String::new();
        let html = render(missing);
        assert!(html.contains("Not found"), "{html}");
        assert!(!html.contains("Cruller"), "{html}");
    }

    #[cfg(feature = "server")]
    mod lens {
        use super::super::{page_sections, page_tabs};
        use crate::sections::PageLens;
        use rules::dashboard::Section;
        use rules::pages::PageEntry;
        use std::collections::BTreeMap;

        fn pages() -> Vec<PageEntry> {
            let mut timeline = BTreeMap::new();
            timeline.insert(
                "lawyer".to_string(),
                ["deadline_board", "boundary_note", "provenance_statement"]
                    .map(String::from)
                    .to_vec(),
            );
            timeline.insert(
                "client".to_string(),
                ["key_dates_strip", "boundary_note", "provenance_statement"]
                    .map(String::from)
                    .to_vec(),
            );
            let mut billing = BTreeMap::new();
            billing.insert(
                "lawyer".to_string(),
                [
                    "engagement_letter",
                    "invoice",
                    "prebill_review",
                    "boundary_note",
                    "provenance_statement",
                ]
                .map(String::from)
                .to_vec(),
            );
            vec![
                PageEntry {
                    slug: "timeline".into(),
                    kind: "docket_deadline_board".into(),
                    title: "Case timeline".into(),
                    draft: None,
                    lenses: timeline,
                },
                PageEntry {
                    slug: "billing".into(),
                    kind: "engagement_billing_records".into(),
                    title: "Billing".into(),
                    draft: None,
                    lenses: billing,
                },
            ]
        }

        #[test]
        fn a_client_sees_only_client_lens_sections() {
            let (_, sections) = page_sections(&pages(), "timeline", PageLens::Client).unwrap();
            assert_eq!(
                sections,
                [
                    Section::KeyDatesStrip,
                    Section::BoundaryNote,
                    Section::ProvenanceStatement,
                    Section::ProjectCosts,
                ]
            );
            assert!(!sections.contains(&Section::DeadlineBoard));
        }

        #[test]
        fn receipt_backed_costs_follow_invoices_for_both_lenses() {
            let mut pages = pages();
            pages
                .iter_mut()
                .find(|page| page.slug == "billing")
                .unwrap()
                .lenses
                .insert(
                    "client".to_string(),
                    ["invoice", "boundary_note", "provenance_statement"]
                        .map(String::from)
                        .to_vec(),
                );
            for lens in [PageLens::Client, PageLens::Lawyer] {
                let (_, sections) = page_sections(&pages, "billing", lens).unwrap();
                let invoice = sections
                    .iter()
                    .position(|section| *section == Section::Invoice)
                    .unwrap();
                assert_eq!(sections.get(invoice + 1), Some(&Section::ProjectCosts));
                assert_eq!(
                    sections
                        .iter()
                        .filter(|section| **section == Section::ProjectCosts)
                        .count(),
                    1
                );
            }
        }

        #[test]
        fn receipt_backed_costs_are_present_when_page_has_no_invoice_section() {
            let pages = pages();
            let (_, sections) = page_sections(&pages, "timeline", PageLens::Client).unwrap();
            assert_eq!(sections.last(), Some(&Section::ProjectCosts));
            assert_eq!(
                sections
                    .iter()
                    .filter(|section| **section == Section::ProjectCosts)
                    .count(),
                1
            );
        }

        #[test]
        fn a_page_without_this_lens_and_an_unknown_slug_are_not_found() {
            assert!(page_sections(&pages(), "billing", PageLens::Client).is_none());
            assert!(page_sections(&pages(), "nope", PageLens::Lawyer).is_none());
        }

        #[test]
        fn tabs_lead_with_the_show_page_and_list_only_this_lens() {
            let client: Vec<String> = page_tabs("m", &pages(), PageLens::Client)
                .into_iter()
                .map(|tab| tab.href)
                .collect();
            assert_eq!(client, ["/app/projects/m", "/app/projects/m/timeline"]);
            let lawyer = page_tabs("m", &pages(), PageLens::Lawyer);
            assert_eq!(lawyer.len(), 3);
            assert_eq!(lawyer[2].label, "Billing");
        }
    }
}
