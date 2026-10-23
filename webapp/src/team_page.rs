//! The firm's small public directory and individual team profiles.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{
    BackBreadcrumb, ExternalLink, PublicShell, SiteHeader, SiteNavLink, SocialMeta,
};
use crate::public_chrome::{PublicChrome, PublicFooter};

/// One entry on the `/team` index.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct TeamMemberSummary {
    pub name: String,
    pub href: String,
}

/// The short `/team` directory.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct TeamIndexContent {
    pub head_title: String,
    pub meta_description: String,
    /// The `og:description` override; `None` previews `meta_description`.
    #[serde(default)]
    pub og_description: Option<String>,
    pub page_title: String,
    pub members: Vec<TeamMemberSummary>,
}

/// One person's public profile.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct TeamProfileContent {
    pub head_title: String,
    pub meta_description: String,
    /// The `og:description` override; `None` previews `meta_description`.
    #[serde(default)]
    pub og_description: Option<String>,
    pub name: String,
    pub biography: String,
    pub linkedin_url: String,
}

/// Copy injected by the firm router for `/team`.
#[derive(Clone, Default)]
pub struct InjectedTeamIndex(pub TeamIndexContent);

/// Copy injected by the firm router for `/team/nick`.
#[derive(Clone, Default)]
pub struct InjectedTeamProfile(pub TeamProfileContent);

/// The `/team` page view.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct TeamIndexView {
    pub chrome: PublicChrome,
    pub content: TeamIndexContent,
}

/// Load the static team directory.
#[server]
pub async fn team_index_view() -> Result<TeamIndexView, ServerFnError> {
    Ok(TeamIndexView {
        chrome: crate::public_chrome::firm_public_chrome_from_context().await,
        content: consume_context::<InjectedTeamIndex>().0,
    })
}

/// The `/team` route entry.
#[component]
pub fn TeamIndexEntry() -> Element {
    let resource = use_server_future(team_index_view)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        _ => return rsx! {},
    };
    rsx! { TeamIndexPage { chrome: view.chrome, content: view.content } }
}

/// The compact team directory. Each person's details live on their profile.
#[component]
pub fn TeamIndexPage(chrome: PublicChrome, content: TeamIndexContent) -> Element {
    let header = team_header(&chrome);
    let footer = rsx! { PublicFooter { chrome: chrome.clone() } };
    rsx! {
        document::Title { "{content.head_title}" }
        document::Meta { name: "description", content: "{content.meta_description}" }
        SocialMeta {
            title: content.head_title.clone(),
            description: content.meta_description.clone(),
            og_description: content.og_description.clone(),
            site_name: chrome.brand_name.clone(),
            image: chrome.social_image.clone(),
        }
        PublicShell { header, footer,
            article { class: "team-index",
                h1 { "{content.page_title}" }
                ul { class: "team-index__list",
                    for member in content.members.iter() {
                        li { class: "team-index__item", key: "{member.href}",
                            a { class: "team-index__link", href: "{member.href}", "{member.name}" }
                        }
                    }
                }
            }
        }
    }
}

/// The individual `/team/nick` page view.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct TeamProfileView {
    pub chrome: PublicChrome,
    pub content: TeamProfileContent,
}

/// Load the static Nick profile.
#[server]
pub async fn team_profile_view() -> Result<TeamProfileView, ServerFnError> {
    Ok(TeamProfileView {
        chrome: crate::public_chrome::firm_public_chrome_from_context().await,
        content: consume_context::<InjectedTeamProfile>().0,
    })
}

/// The `/team/nick` route entry.
#[component]
pub fn TeamProfileEntry() -> Element {
    let resource = use_server_future(team_profile_view)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        _ => return rsx! {},
    };
    rsx! { TeamProfilePage { chrome: view.chrome, content: view.content } }
}

/// Nick's concise profile, with a single link to his `LinkedIn` profile.
#[component]
pub fn TeamProfilePage(chrome: PublicChrome, content: TeamProfileContent) -> Element {
    let header = team_header(&chrome);
    let footer = rsx! { PublicFooter { chrome: chrome.clone() } };
    rsx! {
        document::Title { "{content.head_title}" }
        document::Meta { name: "description", content: "{content.meta_description}" }
        SocialMeta {
            title: content.head_title.clone(),
            description: content.meta_description.clone(),
            og_description: content.og_description.clone(),
            site_name: chrome.brand_name.clone(),
            image: chrome.social_image.clone(),
        }
        PublicShell { header, footer,
            article { class: "team-profile",
                BackBreadcrumb { href: "/team".to_string(), label: "Team".to_string() }
                h1 { "{content.name}" }
                p { class: "team-profile__biography", "{content.biography}" }
                // A classless anchor inside a paragraph. The shared prose rule
                // underlines it; a bare anchor beside the biography is
                // distinguishable by colour alone in the dark scheme.
                p { class: "team-profile__link",
                    ExternalLink { href: content.linkedin_url.clone(), "LinkedIn profile" }
                }
            }
        }
    }
}

fn team_header(chrome: &PublicChrome) -> Element {
    rsx! {
        SiteHeader {
            brand_name: chrome.brand_name.clone(),
            home_href: chrome.home_href.clone(),
            logo_href: chrome.logo_href.clone(),
            destinations: chrome.destinations.iter()
                .map(|link| SiteNavLink::new(link.label.clone(), link.href.clone()))
                .collect(),
            utility: chrome.utility.iter()
                .map(|link| SiteNavLink::new(link.label.clone(), link.href.clone()))
                .collect(),
        }
    }
}
