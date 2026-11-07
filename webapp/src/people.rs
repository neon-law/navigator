//! The admin console people directory as a Dioxus component (#355 Tranche 1 /
//! #641) — the one browser surface that lists and edits a Person.
//!
//! The first Dioxus page to consume server-side data. [`list_admin_people`] is
//! a Dioxus **server function**: its body runs in the `web` process (the
//! `#[server]` macro compiles the body only for the server target and generates
//! an HTTP stub for the wasm client). It parses the request's JSON:API
//! `filter[...]`, `sort`, and `page[...]` parameters
//! ([`store::listing::ListParams`]) and reads one keyset page of the shared
//! `store` directory. During SSR [`use_server_future`] resolves it so the
//! page's rows are in the server-rendered HTML — readable before hydration —
//! with the sort and page links as real anchors that work pre-hydration and
//! for crawlers.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// The people list's own query parameter beside the JSON:API list members
/// [`store::firms::PEOPLE_DIRECTORY`] advertises.
#[derive(Deserialize, Default)]
pub struct PeopleQuery {
    /// The `?error=` flash the admin surface's `POST /app/admin/people/{id}/delete`
    /// sets when the command blocks a delete (the bootstrap Owner, or a non-client
    /// record), redirecting back to the list. Surfaced above the table so the
    /// admin sees why the record survived.
    #[serde(default)]
    pub error: Option<String>,
}

/// One person row, in a shape that crosses the server→client boundary (plain,
/// `wasm`-safe fields — no `store`/`SeaORM` types leak into the client build).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct PersonRow {
    pub id: String,
    pub name: String,
    pub email: String,
    pub role: String,
    /// The row may be deleted — a client record that is not the bootstrap Owner
    /// (the command blocks deleting privileged roles and the bootstrap Owner). Only set on
    /// the admin surface, which shows the Delete action.
    #[serde(default)]
    pub can_delete: bool,
    /// The Firms this person holds a `person_firm_role` membership on
    /// (ENG-494), by name, in membership order. Empty for a Client, who
    /// reaches a matter through `person_project_role` rather than firm
    /// membership, and for anyone with no membership at all.
    #[serde(default)]
    pub firms: Vec<String>,
}

/// The signed-in viewer's system tier. `web` derives it from the request
/// session and injects it (see `portal::dioxus_app::people_router`) so the
/// server-rendered page can show the same role-appropriate lawyer nav chrome the
/// `PageLayout` carried. Plain and wasm-safe so it also crosses to the
/// client and hydration re-renders identical nav markup.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewerRole {
    Owner,
    Admin,
    Lawyer,
    Clerk,
    #[default]
    Client,
}

impl ViewerRole {
    /// The lawyer tiers that reach the `/app/lawyer` workspace.
    /// mirroring `store::persons::Role::is_lawyer_tier`.
    #[must_use]
    pub fn is_lawyer_tier(self) -> bool {
        matches!(self, Self::Owner | Self::Admin | Self::Lawyer)
    }

    #[must_use]
    pub fn is_admin_tier(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }

    #[must_use]
    pub fn is_owner(self) -> bool {
        matches!(self, Self::Owner)
    }

    /// Every tier that works for the firm — the four roles seeded from a firm
    /// domain, with `client` the one authenticated tier outside it.
    ///
    /// Wider than [`is_lawyer_tier`](Self::is_lawyer_tier) by exactly `Clerk`,
    /// and it must stay that way: a supervised non-lawyer belongs to the firm
    /// without holding legal authority. Use this only for surfaces where the
    /// question is "does this person work here" — operating the product — never
    /// for legal work, which is `is_lawyer_tier`'s question.
    #[must_use]
    pub fn is_firm_tier(self) -> bool {
        !matches!(self, Self::Client)
    }

    #[must_use]
    pub const fn authority_rank(self) -> u8 {
        match self {
            Self::Owner => 4,
            Self::Admin => 3,
            Self::Lawyer => 2,
            Self::Clerk => 1,
            Self::Client => 0,
        }
    }
}

/// The list's base path — the sort-link anchors and the "Add person" href hang
/// off it.
pub const LIST_PATH: &str = "/app/admin/people";
/// The "Add person" destination.
pub const NEW_HREF: &str = "/app/admin/people/new";
/// The detail path base for the per-row Edit / Delete routes.
pub const DETAIL_PATH: &str = "/app/admin/people";

/// The rendered people view: the rows, the active sort/filter state the
/// component needs to build the sort-link anchors, and the viewer's role for
/// the nav chrome.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct PeopleView {
    /// The resolved brand's tokens stylesheet href, so the page wears
    /// its own palette rather than the firm's on a non-default host.
    #[serde(default)]
    pub tokens_href: String,
    pub rows: Vec<PersonRow>,
    pub sort: String,
    pub filter_name: String,
    pub filter_email: String,
    pub role: ViewerRole,
    /// The `?error=` flash surfaced above the table — set when the delete route
    /// redirects back after the command blocked a delete (the bootstrap Owner,
    /// or a non-client record). `None` on a plain visit.
    #[serde(default)]
    pub error: Option<String>,
    /// The session CSRF token for the per-row Delete form.
    #[serde(default)]
    pub csrf_token: String,
    /// The deploy's firm name, for the document title. Resolved from the
    /// request-scoped branding rather than written into the copy, so a
    /// white-label deploy's tab reads its own name.
    #[serde(default)]
    pub firm_name: String,
    /// The next page's link; `None` on the last page.
    #[serde(default)]
    pub next_href: Option<String>,
    /// The previous page's link; `None` on the first page.
    #[serde(default)]
    pub prev_href: Option<String>,
}

/// Fetch the people directory for the **admin console** (`/app/admin/people`):
/// refuse non-admin, read the injected CSRF token, and compute each row's
/// delete eligibility (only client records that are not the bootstrap Owner),
/// so the admin surface renders the per-row action column.
#[server]
pub async fn list_admin_people() -> Result<PeopleView, ServerFnError> {
    let axum::extract::Query(query) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<PeopleQuery>, _>()
            .await?;
    let axum::extract::RawQuery(raw) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::RawQuery, _>().await?;
    // The blocked-delete flash the `POST /app/admin/people/{id}/delete` route sets
    // when the command refuses the delete; surfaced above the table.
    let error = query.error.filter(|message| !message.is_empty());
    // The route's pre-handler has already answered a refused request with 400.
    let params = store::listing::ListParams::parse(
        raw.as_deref().unwrap_or(""),
        &store::firms::PEOPLE_DIRECTORY,
    )
    .map_err(|e| ServerFnError::new(e.to_string()))?;
    let sort = params.sort_param();
    let filter_name = params.filter("name").to_string();
    let filter_email = params.filter("email").to_string();

    // Defense in depth: the admin console is admin-only. `require_admin` reads
    // the injected tier and commits a real 403 for a non-admin (the status the
    // `admin_gate` returned), before the query runs.
    let role = crate::admin_listing::require_admin().await?;
    let csrf_token = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::csrf::CsrfToken>,
        _,
    >()
    .await
    .map(|axum::Extension(token)| token.0)
    .unwrap_or_default();
    let crate::person_show::BootstrapOwnerEmail(bootstrap_owner_email) =
        dioxus_fullstack_core::FullstackContext::extract::<
            axum::Extension<crate::person_show::BootstrapOwnerEmail>,
            _,
        >()
        .await
        .map(|axum::Extension(email)| email)
        .unwrap_or_default();

    let surreal = consume_context::<store::surreal::SurrealDb>();
    let mut scope = store::firms::DirectoryScope::Everyone;
    if role == ViewerRole::Admin {
        let crate::portal_project_list::PersonId(person_id) =
            dioxus_fullstack_core::FullstackContext::extract::<
                axum::Extension<crate::portal_project_list::PersonId>,
                _,
            >()
            .await
            .map(|axum::Extension(id)| id)
            .unwrap_or_default();
        let Some(viewer_id) = person_id
            .as_deref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
        else {
            return Ok(PeopleView {
                tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
                firm_name: crate::app_chrome::firm_name_from_context().await,
                rows: Vec::new(),
                sort,
                filter_name,
                filter_email,
                role,
                error,
                csrf_token,
                next_href: None,
                prev_href: None,
            });
        };
        scope = store::firms::DirectoryScope::VisibleToAdmin(viewer_id);
    }

    // One keyset page per request — never the whole directory, and never a
    // membership lookup per row.
    let page = store::firms::people_directory(&surreal, &params, scope)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let link = |member: &str, cursor: &Option<String>| {
        cursor.as_ref().map(|cursor| {
            format!(
                "{}&{member}={}",
                list_href(&sort, &filter_name, &filter_email, LIST_PATH),
                encode(cursor)
            )
        })
    };
    let next_href = link("page[after]", &page.next);
    let prev_href = link("page[before]", &page.prev);

    let rows = page
        .items
        .into_iter()
        .map(|entry| person_row(entry, bootstrap_owner_email.as_deref()))
        .collect();

    Ok(PeopleView {
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
        firm_name: crate::app_chrome::firm_name_from_context().await,
        rows,
        sort,
        filter_name,
        filter_email,
        role,
        error,
        csrf_token,
        next_href,
        prev_href,
    })
}

/// One directory entry as a table row, with its delete eligibility: the
/// command blocks deleting privileged roles and the bootstrap Owner.
#[cfg(feature = "server")]
fn person_row(
    entry: store::firms::DirectoryEntry,
    bootstrap_owner_email: Option<&str>,
) -> PersonRow {
    let store::firms::DirectoryEntry {
        person: p,
        firm_names,
    } = entry;
    let is_client = p.role == store::persons::Role::Client;
    let is_bootstrap_owner =
        bootstrap_owner_email.is_some_and(|configured| configured.eq_ignore_ascii_case(&p.email));
    PersonRow {
        id: p.id.to_string(),
        name: p.name,
        email: p.email,
        role: p.role.as_str().to_string(),
        can_delete: is_client && !is_bootstrap_owner,
        firms: firm_names,
    }
}

/// The `?sort=` link for a sortable column header: toggles that column between
/// ascending and descending while preserving the active filters, so a header
/// click navigates (a real anchor, working pre-hydration).
fn sort_href(
    column: &str,
    active_sort: &str,
    filter_name: &str,
    filter_email: &str,
    base_path: &str,
) -> String {
    let next = if active_sort == column {
        format!("-{column}")
    } else {
        column.to_string()
    };
    list_href(&next, filter_name, filter_email, base_path)
}

/// The list's link for a sort and the active filters — the first page of
/// that order. Page links append their cursor to it.
fn list_href(sort: &str, filter_name: &str, filter_email: &str, base_path: &str) -> String {
    use std::fmt::Write as _;
    let mut query = format!("{base_path}?");
    if !filter_email.is_empty() {
        let _ = write!(query, "filter[email]={}&", encode(filter_email));
    }
    if !filter_name.is_empty() {
        let _ = write!(query, "filter[name]={}&", encode(filter_name));
    }
    let _ = write!(query, "sort={}", encode(sort));
    query
}

/// Map a `store::entity::Role` token (as produced by `Role::as_str()`:
/// `owner`, `admin`, `lawyer`, `clerk`, `client`) to the user-facing directory
/// label, mirroring the lawyer directory's `role_label`
/// (`views::pages::admin::people`) so the migrated page keeps the same visible
/// role text instead of exposing internal tokens. The lawyer UI is English by
/// policy, so the labels are the `views/locales/en/people.yml` values inline;
/// an unknown token falls back to the `Client` label, matching the page.
fn role_label(role: &str) -> &'static str {
    match role {
        "owner" => "Owner",
        "admin" => "Admin",
        "lawyer" => "Lawyer (lawyer)",
        "clerk" => "Clerk (non-lawyer)",
        _ => "Client",
    }
}

/// Minimal percent-encoding for query-parameter values — enough for names and
/// emails, dependency-free so it compiles for the wasm client too.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "%20".to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The admin console people directory (`/app/admin/people`) — the same sortable list
/// with a per-row action column (Edit / Delete). Resolves the
/// admin server function and renders through the shared [`render_people`].
#[component]
pub fn AdminPeople() -> Element {
    let resource = use_server_future(list_admin_people)?;
    render_people(&resource)
}

/// Render the resolved people directory: a sortable table (the sort headers are
/// real anchors carrying the `?sort=` toggle, working pre-hydration) with the
/// per-row action column — a native Edit link plus a Delete `POST` form.
fn render_people(resource: &Resource<Result<PeopleView, ServerFnError>>) -> Element {
    // Clone the view out of the read guard before rendering so the borrow does
    // not outlive it (the `rsx!` output escapes this scope).
    let view = match &*resource.read() {
        Some(Ok(view)) => Some(view.clone()),
        Some(Err(_)) => {
            return rsx! {
                main { id: "admin-people", p { "Failed to load people." } }
            }
        }
        None => None,
    };

    match view {
        Some(view) => {
            let name_href = sort_href(
                "name",
                &view.sort,
                &view.filter_name,
                &view.filter_email,
                LIST_PATH,
            );
            let email_href = sort_href(
                "email",
                &view.sort,
                &view.filter_name,
                &view.filter_email,
                LIST_PATH,
            );
            let is_empty = view.rows.is_empty();
            let error = view.error.clone();
            let title = format!("{} | Admin | People", view.firm_name);
            rsx! {
                document::Title { "{title}" }
                document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
                document::Stylesheet { href: "{view.tokens_href}" }
                // The role-appropriate lawyer nav chrome the page carried:
                // portal + sign-out for every signed-in viewer, the lawyer
                // One destination for every tier; the firm dashboard's
                // directory is how an admin reaches the admin surface.
                nav { class: "lawyer-nav",
                    a { class: "nav-link", href: "/app/projects", "Projects" }
                    a { class: "nav-link", href: "/auth/logout", "Sign out" }
                    crate::components::AppProfileMenu {
                        destinations: vec![
                            crate::components::AppNavLink::new("Projects", "/app/projects"),
                            crate::components::AppNavLink::new("Sign out", "/auth/logout"),
                        ],
                    }
                }
                main { id: "admin-people",
                    h1 { "People" }
                    p {
                        a { class: "nav-btn nav-btn--primary people-new", href: "{NEW_HREF}", "Add person" }
                    }
                    if let Some(error) = error.as_ref() {
                        p { class: "nav-form-error", role: "alert", "{error}" }
                    }
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead {
                                tr {
                                    th { a { class: "sort-name", href: "{name_href}", "Name" } }
                                    th { a { class: "sort-email", href: "{email_href}", "Email" } }
                                    th { "Role" }
                                    th { "Firms" }
                                    th { "" }
                                }
                            }
                            tbody {
                                if is_empty {
                                    tr {
                                        td { class: "people-empty", colspan: "5", "No people yet." }
                                    }
                                }
                                for row in view.rows.iter().cloned() {
                                    tr { class: "person-row", key: "{row.id}",
                                        td { class: "person-name", "{row.name}" }
                                        td { class: "person-email", "{row.email}" }
                                        td { class: "person-role", {role_label(&row.role)} }
                                        td { class: "person-firms",
                                            if row.firms.is_empty() {
                                                span { class: "nav-muted", "—" }
                                            } else {
                                                "{row.firms.join(\", \")}"
                                            }
                                        }
                                        td { class: "person-actions",
                                            {person_row_actions(&view.csrf_token, &row)}
                                        }
                                    }
                                }
                            }
                        }
                    }
                    {people_pager(&view)}
                }
            }
        }
        None => rsx! {
            main { id: "admin-people", p { "Loading…" } }
        },
    }
}

/// Previous / Next anchors for the page's neighbours; nothing on a list that
/// fits one page.
fn people_pager(view: &PeopleView) -> Element {
    if view.prev_href.is_none() && view.next_href.is_none() {
        return rsx! {};
    }
    rsx! {
        nav { class: "people-pages", "aria-label": "People pages",
            if let Some(href) = view.prev_href.as_ref() {
                a { class: "nav-link people-prev", rel: "prev", href: "{href}", "Previous" }
            }
            if let Some(href) = view.next_href.as_ref() {
                a { class: "nav-link people-next", rel: "next", href: "{href}", "Next" }
            }
        }
    }
}

/// The admin console's per-row action cell: the Edit link (always), then — for a
/// deletable client that is not the bootstrap Owner — a native Delete `POST`
/// form. Native forms so they work pre-hydration; the row used an
/// `hx-delete` button.
fn person_row_actions(csrf_token: &str, row: &PersonRow) -> Element {
    let edit_href = format!("{DETAIL_PATH}/{}/edit", row.id);
    let delete_action = format!("{DETAIL_PATH}/{}/delete", row.id);
    rsx! {
        span { class: "row-actions",
            a { class: "nav-link", href: "{edit_href}", "Edit" }
            if row.can_delete {
                form { class: "row-action", method: "post", action: "{delete_action}",
                    "aria-label": "Delete {row.name}",
                    input { r#type: "hidden", name: "_csrf", value: "{csrf_token}" }
                    button { class: "nav-btn nav-btn--danger", r#type: "submit", "Delete" }
                }
            }
        }
    }
}
