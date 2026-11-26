//! The authored Guieb & Shook pages and their public routes.

use std::sync::{Arc, LazyLock};

use axum::{response::Redirect, routing::get, Router};
use portal::{dioxus_app::guieb_shook_page_router, AppState};
use views::brand::{BrandKey, Branding};
use webapp::guieb_shook::{Page, Website};

#[derive(serde::Deserialize)]
struct HomeCatalog {
    website: Website,
}

static WEBSITE: LazyLock<Arc<Website>> = LazyLock::new(|| {
    let HomeCatalog { website: mut site } =
        serde_yaml::from_str(include_str!("../locales/en/guieb-shook/home.yaml"))
            .expect("invariant: the Guieb & Shook home catalog includes its website copy");
    site.home.about = portal::blog::render_body(&site.home.about);
    for page in &mut site.practice {
        page.body = portal::blog::render_body(&page.body);
    }
    for page in &mut site.attorneys {
        page.body = portal::blog::render_body(&page.body);
    }
    for page in &mut site.legal {
        page.body = portal::blog::render_body(&page.body);
    }
    Arc::new(site)
});

pub(crate) fn public_pages() -> impl Iterator<Item = (&'static str, Page)> {
    std::iter::once(("/our-story", Page::Story))
        .chain(
            WEBSITE
                .practice
                .iter()
                .enumerate()
                .map(|(n, p)| (p.path.as_str(), Page::Practice(n))),
        )
        .chain(
            WEBSITE
                .attorneys
                .iter()
                .enumerate()
                .map(|(n, p)| (p.path.as_str(), Page::Attorney(n))),
        )
        .chain(
            WEBSITE
                .legal
                .iter()
                .enumerate()
                .map(|(n, p)| (p.path.as_str(), Page::Legal(n))),
        )
}

pub(crate) fn new_routes(branding: &'static Branding, state: &AppState) -> Router {
    public_pages()
        .fold(Router::new(), |router, (path, page)| {
            // Earlier links carried `.html`; send them to the clean path.
            router
                .merge(page_router(path, page, branding, state))
                .route(
                    &format!("{path}.html"),
                    get(move || async move { Redirect::permanent(path) }),
                )
        })
        .route("/index.html", get(|| async { Redirect::permanent("/") }))
        .route(
            "/contact.html",
            get(|| async { Redirect::permanent("/contact") }),
        )
}

pub(crate) fn shared_routes(branding: &'static Branding, state: &AppState) -> Router {
    page_router("/", Page::Home, branding, state)
        .merge(page_router("/contact", Page::Contact, branding, state))
        .merge(page_router(
            "/contact/sent",
            Page::ContactSent,
            branding,
            state,
        ))
}

fn page_router(path: &str, page: Page, branding: &'static Branding, state: &AppState) -> Router {
    let brand = BrandKey::GuiebShook.resolve_branding(branding);
    let copy = crate::locales::lead_capture(brand);
    guieb_shook_page_router(
        path,
        WEBSITE.clone(),
        page,
        brand.firm_phone.to_string(),
        copy.consent_sentence,
        (page == Page::Contact).then(|| (state.sessions.clone(), portal::secure_cookies(state))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_mockup_pages_are_published_by_guieb_shook_only() {
        assert_eq!(public_pages().count(), 15);
        for (path, _) in public_pages() {
            assert!(BrandKey::GuiebShook.publishes_firm_path(path), "{path}");
            assert!(!BrandKey::Neon.publishes_firm_path(path), "{path}");
        }
        assert!(WEBSITE.home.about.contains("Navigator"));
    }
}
