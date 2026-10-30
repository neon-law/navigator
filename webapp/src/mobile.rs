//! The Nautilus mobile shell: the DeleteYourData.com subscription experience
//! as a native Dioxus app.
//!
//! The web root ([`crate::App`]) is SSR-bound — server futures and
//! SSR-injected context do not exist on a native target — so the mobile arm
//! of `main.rs` launches this module's root instead. Navigation is a
//! two-state signal rather than `dioxus_router`: the crate has no router
//! precedent, and this shell's tests render pages through `dioxus-ssr` the
//! same way the `marketing_page` component tests do.
//!
//! Pages reuse the [`crate::marketing_page::PageSkin`] visual vocabulary —
//! the `fm-page--practice fm-page--nautilus` modifier classes — so the app
//! reads as the same product the `/nautilus` page previews. Content is local
//! and static by design (see the Nautilus Mobile App Spec): no server
//! functions, no store reads.

use dioxus::prelude::*;

/// The two pages the shell can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MobilePage {
    /// The subscription home: what Nautilus includes and what it costs.
    #[default]
    Home,
    /// Delete Your Data: leaving Nautilus with your records.
    Dyd,
}

/// The mobile root. The launch arm in `main.rs` renders this; the page
/// signal is the shell's router.
#[component]
pub fn MobileApp() -> Element {
    let mut page = use_signal(MobilePage::default);
    rsx! {
        div { class: "fm-page fm-page--mobile",
            match page() {
                MobilePage::Home => rsx! { NautilusHome { on_open_dyd: move |_| page.set(MobilePage::Dyd) } },
                MobilePage::Dyd => rsx! { DydPage { on_back_home: move |_| page.set(MobilePage::Home) } },
            }
        }
    }
}

/// The Nautilus subscription home.
#[component]
pub fn NautilusHome(on_open_dyd: EventHandler<MouseEvent>) -> Element {
    rsx! {
        main { class: "fm-page--practice fm-page--nautilus",
            h1 { "Nautilus" }
            p { class: "fm-page__lede",
                "Private, protected paperwork — one subscription for your estate plan and the documents your family keeps."
            }
            section { class: "nautilus-included",
                h2 { "Included" }
                ul {
                    li { "Your will, health care directive, and power of attorney" }
                    li { "State-specific documents kept current as the law changes" }
                    li { "Unlimited notarized document updates" }
                    li { "Encrypted storage you can reach from anywhere" }
                }
            }
            section { class: "nautilus-pricing",
                h2 { "Pricing" }
                p { "$29/month or $290/year — cancel anytime." }
            }
            button { class: "nautilus-dyd-link", "data-mobile-nav": "dyd", onclick: on_open_dyd,
                "Delete Your Data"
            }
        }
    }
}

/// The Delete Your Data page: leaving Nautilus with your records.
#[component]
pub fn DydPage(on_back_home: EventHandler<MouseEvent>) -> Element {
    rsx! {
        main { class: "fm-page--practice fm-page--nautilus",
            h1 { "Delete Your Data" }
            p { class: "fm-page__lede",
                "You own your documents. Leaving is designed to be clean, not punitive."
            }
            section { class: "nautilus-dyd-steps",
                h2 { "How it works" }
                ol {
                    li { "Download everything — signed wills, directives, and every version you filed." }
                    li { "Confirm the export with your receiving attorney." }
                    li { "We erase your records from the store and confirm the deletion in writing." }
                }
            }
            button { class: "nautilus-home-link", "data-mobile-nav": "home", onclick: on_back_home,
                "Back to Nautilus"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared render helper: same `dioxus-ssr` render the
    /// `marketing_page` component tests use, parameterized by the shell's
    /// page state.
    fn render(page: MobilePage) -> String {
        let element = match page {
            MobilePage::Home => rsx! { NautilusHome { on_open_dyd: move |_| {} } },
            MobilePage::Dyd => rsx! { DydPage { on_back_home: move |_| {} } },
        };
        dioxus_ssr::render_element(element)
    }

    #[test]
    fn nautilus_home_renders_the_subscription_experience() {
        let html = render(MobilePage::Home);
        assert!(html.contains("fm-page--nautilus"), "home must wear the Nautilus skin");
        assert!(html.contains("Included"));
        assert!(html.contains("$29/month"), "monthly price must be present");
        assert!(html.contains("$290/year"), "annual price must be present");
        assert!(
            html.contains(r#"data-mobile-nav="dyd""#),
            "home must expose the navigation hook to the DYD page"
        );
    }

    #[test]
    fn dyd_page_renders_delete_your_data() {
        let html = render(MobilePage::Dyd);
        assert!(html.contains("Delete Your Data"));
        assert!(html.contains("erase your records"));
        assert!(html.contains("fm-page--nautilus"), "the DYD page wears the same skin");
        assert!(
            html.contains(r#"data-mobile-nav="home""#),
            "the DYD page must expose the navigation hook back home"
        );
    }

    #[test]
    fn the_two_pages_render_distinctly() {
        assert_ne!(render(MobilePage::Home), render(MobilePage::Dyd));
    }

    #[test]
    fn the_shell_launches_on_home() {
        // The launch root's initial signal state is the default `MobilePage`;
        // SSR renders that first paint, so the app must open on the home page.
        let html = dioxus_ssr::render_element(rsx! { MobileApp {} });
        assert!(html.contains("fm-page--nautilus"));
        assert!(html.contains("$29/month"));
        assert!(!html.contains("Delete Your Data</h1>"), "must not open on the DYD page");
    }
}
