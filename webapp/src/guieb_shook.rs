//! Public pages for the Guieb & Shook brand.

use std::sync::Arc;

use dioxus::prelude::*;
use serde::Deserialize;

use crate::lead_capture::{HCaptchaWidget, LeadCaptureContext};
use crate::public_chrome::{copy_from_request_or_context, ChromeNavLink, PublicUtility};

#[path = "guieb_shook_clips.rs"]
mod clips;

const ASSETS: &str = "/assets/img/guieb-shook";
const CSS: &str = "/public/css/guieb-shook-website.css";
const SCRIPT: &str = "/public/js/guieb-shook-website.js";
const CONTACT: &str = "/contact";
const STORY: &str = "/our-story.html";
const EMAIL: &str = "info@guiebshook.com";

#[derive(Deserialize, PartialEq)]
pub struct Website {
    pub home: HomeCopy,
    pub story: StoryCopy,
    pub practice: Vec<PracticeCopy>,
    pub attorneys: Vec<AttorneyCopy>,
    pub legal: Vec<LegalCopy>,
}

#[derive(Deserialize, PartialEq)]
pub struct HomeCopy {
    pub description: String,
    pub slides: Vec<SlideCopy>,
    pub about: String,
    pub practice_intro: String,
}

#[derive(Deserialize, PartialEq)]
pub struct SlideCopy {
    video: String,
    eyebrow: String,
    heading: [String; 2],
}

#[derive(Deserialize, PartialEq)]
pub struct StoryCopy {
    pub description: String,
    intro: String,
    boroughs: Vec<Borough>,
    names: Vec<Namesake>,
}

#[derive(Deserialize, PartialEq)]
struct Borough {
    id: String,
    name: String,
    photo: String,
    alt: String,
    caption: String,
    copy: String,
}

#[derive(Deserialize, PartialEq)]
struct Namesake {
    name: String,
    paragraphs: Vec<String>,
}

#[derive(Deserialize, PartialEq)]
pub struct PracticeCopy {
    pub path: String,
    title: String,
    description: String,
    summary: String,
    photo: String,
    alt: String,
    lead: String,
    pub body: String,
}

#[derive(Deserialize, PartialEq)]
pub struct AttorneyCopy {
    pub path: String,
    title: String,
    description: String,
    role: String,
    email: String,
    portrait: String,
    #[serde(default)]
    offset: bool,
    pub body: String,
    facts: Vec<Fact>,
}

#[derive(Deserialize, PartialEq)]
struct Fact {
    label: String,
    entries: Vec<FactEntry>,
}

#[derive(Deserialize, PartialEq)]
struct FactEntry {
    text: String,
    href: Option<String>,
}

#[derive(Deserialize, PartialEq)]
pub struct LegalCopy {
    pub path: String,
    title: String,
    description: String,
    lead: String,
    pub body: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Home,
    Story,
    Practice(usize),
    Attorney(usize),
    Legal(usize),
    Contact,
    ContactSent,
}

impl Website {
    fn heading(&self, page: Page) -> (&str, &str) {
        match page {
            Page::Home => ("", &self.home.description),
            Page::Story => ("Our Story", &self.story.description),
            Page::Practice(index) => {
                let copy = &self.practice[index];
                (&copy.title, &copy.description)
            }
            Page::Attorney(index) => {
                let copy = &self.attorneys[index];
                (&copy.title, &copy.description)
            }
            Page::Legal(index) => {
                let copy = &self.legal[index];
                (&copy.title, &copy.description)
            }
            Page::Contact | Page::ContactSent => {
                ("Contact Us", "Start a conversation with Guieb & Shook.")
            }
        }
    }
}

#[server]
async fn page_context() -> Result<(LeadCaptureContext, Vec<ChromeNavLink>), ServerFnError> {
    let lead = copy_from_request_or_context(LeadCaptureContext::default).await;
    let utility = copy_from_request_or_context(PublicUtility::default).await;
    Ok((lead, utility.0))
}

#[component]
pub fn WebsiteEntry(site: Arc<Website>, page: Page, phone: String, consent: String) -> Element {
    let context = use_server_future(page_context)?;
    let context = context.read();
    let Some(Ok((lead, utility))) = context.as_ref() else {
        return rsx! {};
    };
    rsx! {
        WebsitePage { site, page, phone, consent, lead: lead.clone(), utility: utility.clone() }
    }
}

#[component]
pub fn WebsitePage(
    site: Arc<Website>,
    page: Page,
    phone: String,
    consent: String,
    lead: LeadCaptureContext,
    utility: Vec<ChromeNavLink>,
) -> Element {
    let (title, description) = site.heading(page);
    let title = if title.is_empty() {
        "Guieb & Shook P.C.".to_string()
    } else {
        format!("{title} · Guieb & Shook P.C.")
    };
    let path = match page {
        Page::Home => "/",
        Page::Story => STORY,
        Page::Practice(index) => &site.practice[index].path,
        Page::Attorney(index) => &site.attorneys[index].path,
        Page::Legal(index) => &site.legal[index].path,
        Page::Contact | Page::ContactSent => CONTACT,
    };
    let origin = "https://www.guiebshook.com";
    rsx! {
        document::Title { "{title}" }
        document::Meta { name: "description", content: "{description}" }
        document::Meta { name: "viewport", content: "width=device-width, initial-scale=1" }
        document::Link { rel: "icon", href: "{ASSETS}/favicon.svg", r#type: "image/svg+xml" }
        document::Link { rel: "apple-touch-icon", href: "{ASSETS}/apple-touch-icon.png" }
        document::Link {
            rel: "preload", as: "font", href: "{ASSETS}/fonts/instrument-sans-latin.woff2",
            r#type: "font/woff2", crossorigin: "anonymous",
        }
        document::Stylesheet { href: CSS }
        crate::components::SocialMeta {
            title: title.clone(), description: description.to_string(), og_description: None,
            site_name: "Guieb & Shook".to_string(), image: format!("{origin}{ASSETS}/share.jpg"),
            url: format!("{origin}{path}"),
        }
        div { class: "guieb-shook-website", id: "guieb-shook-website",
            a { class: "skip-link", href: "#top", "Skip to content" }
            Header { site: site.clone(), home: page == Page::Home, utility }
            main { id: "top", class: if page == Page::Story { "story-page" },
                match page {
                    Page::Home => rsx! { Home { site: site.clone() } },
                    Page::Story => rsx! { Story { site: site.clone() } },
                    Page::Practice(index) => rsx! { Practice { site: site.clone(), index, phone: phone.clone() } },
                    Page::Attorney(index) => rsx! { Attorney { site: site.clone(), index } },
                    Page::Legal(index) => rsx! { Legal { site: site.clone(), index } },
                    Page::Contact => rsx! { Contact { phone: phone.clone(), consent, lead } },
                    Page::ContactSent => rsx! {
                        div { class: "wrap legal-head",
                            p { class: "eyebrow", "Contact us" }
                            h1 { "Thank you for reaching out." }
                            p { class: "contact-lead", "We’ll be in touch. If you need to reach us, please call " a { href: telephone(&phone), "{phone}" } "." }
                        }
                    },
                }
            }
            Footer { site }
        }
        script { src: SCRIPT, defer: true }
    }
}

#[component]
fn Header(site: Arc<Website>, home: bool, utility: Vec<ChromeNavLink>) -> Element {
    let about = vec![(STORY, "Our Story"), ("/our-story.html#name", "Our Name")];
    let practice = site
        .practice
        .iter()
        .map(|copy| (copy.path.as_str(), copy.title.as_str()))
        .collect();
    let attorneys = site
        .attorneys
        .iter()
        .map(|copy| (copy.path.as_str(), copy.title.as_str()))
        .collect();
    rsx! {
        header { class: if home { "site-header" } else { "site-header is-solid" },
            div { class: "wrap",
                a { class: "brand", href: "/", aria_label: "Guieb & Shook P.C. home",
                    img { class: "brand-light", src: "{ASSETS}/Guieb-Shook-wordmark-cream.svg", alt: "Guieb & Shook" }
                    img { class: "brand-dark", src: "{ASSETS}/Guieb-Shook-wordmark-navy.svg", alt: "", aria_hidden: "true" }
                }
                button { class: "nav-menu-toggle", r#type: "button", aria_expanded: "false",
                    aria_controls: "primary-nav", aria_label: "Open menu",
                    span {} span {} span {}
                }
                nav { class: "nav", id: "primary-nav", aria_label: "Primary",
                    {navigation_group("/#about", "About", about)}
                    {navigation_group("/#practice", "Practice Areas", practice)}
                    {navigation_group("/#attorneys", "Attorneys", attorneys)}
                    {navigation_group(CONTACT, "Contact", vec![(CONTACT, "Contact Us")])}
                }
                if let Some(link) = utility.first() {
                    a { class: "sign-in", href: "{link.href}", "{link.label}" }
                } else {
                    a { class: "sign-in", href: "/auth/login", "Sign in" }
                }
            }
        }
    }
}

fn navigation_group(href: &str, label: &str, pages: Vec<(&str, &str)>) -> Element {
    rsx! {
        div { class: "nav-item has-menu",
            a { href, "{label}" }
            button { class: "nav-sub-toggle", r#type: "button", aria_expanded: "false", aria_label: "Show {label} pages" }
            ul { class: "nav-menu",
                for (link, name) in pages {
                    li { a { href: link, "{name}" } }
                }
            }
        }
    }
}

const OFFICES: [(&str, [&str; 2]); 4] = [
    (
        "New York",
        ["12 E 49th St, 18th Floor", "New York, NY 10017"],
    ),
    ("California", ["1515 20th Street", "Bakersfield, CA 93301"]),
    (
        "Nevada",
        ["5150 Mae Anne Ave, Ste 405-9002", "Reno, NV 89523"],
    ),
    (
        "Washington",
        ["1111 Main St, Ste 201E", "Vancouver, WA 98660"],
    ),
];

#[component]
fn Footer(site: Arc<Website>) -> Element {
    rsx! {
        footer { class: "site-footer",
            p { class: "footer-announce", "Founded in New York · Rooted in California" }
            div { class: "wrap footer-grid",
                for (state, [line, city]) in OFFICES {
                    div { class: "office", h3 { "{state}" } p { "{line}" br {} "{city}" } }
                }
                div { class: "footer-col-rule", aria_hidden: "true" }
                nav { class: "office footer-legal-col", aria_label: "Legal",
                    h3 { "Legal" }
                    ul { for page in &site.legal { li { a { href: "{page.path}", "{page.title}" } } } }
                }
            }
            div { class: "wrap footer-legal",
                div { class: "footer-bottom",
                    p { class: "fine", "© 2026 Guieb & Shook P.C." }
                    p { class: "fine", "Attorney Advertising." br { class: "m-br" } " Prior results do not guarantee a similar outcome." }
                }
            }
        }
    }
}

fn video(class: &str, clip: &str) -> Element {
    rsx! {
        video { class,
            "data-src": "{ASSETS}/video/{clip}.mp4",
            poster: "{ASSETS}/video/{clip}.jpg",
            muted: true, r#loop: true, playsinline: true, preload: "none", aria_hidden: "true",
        }
    }
}

#[component]
fn Home(site: Arc<Website>) -> Element {
    rsx! {
        section { class: "hero", aria_roledescription: "carousel", aria_label: "Introduction",
            div { class: "hero-media", {video("hero-video is-shown", &site.home.slides[0].video)} }
            for (index, slide) in site.home.slides.iter().enumerate() {
                div { class: if index == 0 { "slide is-active" } else { "slide" },
                    "data-video": "{slide.video}", aria_hidden: if index > 0 { "true" },
                    div { class: "wrap slide-copy",
                        p { class: "eyebrow", "{slide.eyebrow}" }
                        if index == 0 {
                            h1 { span { "{slide.heading[0]}" } " " span { "{slide.heading[1]}" } }
                        } else {
                            p { class: "h1", span { "{slide.heading[0]}" } " " span { "{slide.heading[1]}" } }
                        }
                    }
                }
            }
            div { class: "wrap hero-controls",
                div { class: "dots", role: "group", aria_label: "Choose a slide",
                    for index in 0..site.home.slides.len() {
                        button { r#type: "button", class: "nav-slide-dot", class: if index == 0 { "is-active" },
                            aria_label: "Slide {index + 1}", aria_pressed: index == 0,
                        }
                    }
                }
                button { class: "nav-slideshow-pause", r#type: "button", aria_label: "Pause slideshow", span {} }
            }
        }
        About { body: site.home.about.clone() }
        section { id: "practice", class: "practice",
            div { class: "wrap",
                div { class: "center-head",
                    p { class: "eyebrow", "Our practice areas" }
                    h2 { class: "split", span { "Focused practice," } " " span { "personal counsel." } }
                    p { "{site.home.practice_intro}" }
                }
                ul { class: "practice-list",
                    for copy in &site.practice {
                        li { a { href: "{copy.path}", span { b { "{copy.title}" } small { "{copy.summary}" } } } }
                    }
                }
            }
        }
        section { id: "attorneys", class: "attorneys-section",
            div { class: "wrap",
                div { class: "center-head",
                    p { class: "eyebrow", "Our attorneys" }
                    h2 { class: "split", span { "We are your counsel" } " " span { "and legal engineers." } }
                }
                div { class: "attorneys",
                    for copy in &site.attorneys {
                        article { class: "attorney",
                            a { class: "card card-link", href: "{copy.path}", aria_label: "{copy.title}: view profile",
                                div { class: "portrait has-photo",
                                    img { src: "{ASSETS}/{portrait_variant(&copy.portrait, \"square\")}", alt: "{copy.title}", loading: "lazy", width: "1000", height: "1000" }
                                }
                                div { class: "plate", h3 { "{copy.title}" } span { class: "plate-more", aria_hidden: "true", "View profile" } }
                            }
                        }
                    }
                }
            }
        }
        section { id: "contact", class: "contact has-video",
            {video("contact-video", "contact-path")}
            div { class: "wrap",
                div { class: "center-head",
                    p { class: "eyebrow", "Contact us" }
                    h2 { class: "split", span { "Start a" } " " span { "conversation." } }
                    p { "A few lines about what you need is all it takes to start." br {} "We’ll get back to you quickly." }
                }
                div { class: "center", a { class: "btn", href: CONTACT, "Contact us" } }
            }
        }
        div { class: "closing-band", aria_hidden: "true",
            img { class: "bridge bridge-brooklyn", src: "/public/brand/guieb-shook/bridge-brooklyn.svg", alt: "" }
            img { src: "{ASSETS}/Guieb-Shook-badge-cream.svg", alt: "" }
            img { class: "bridge bridge-bay", src: "/public/brand/guieb-shook/bridge-bay.svg", alt: "" }
        }
    }
}

#[component]
fn About(body: String) -> Element {
    rsx! {
        section { id: "about", class: "as-track", aria_label: "About us",
            div { class: "as-stage",
                div { class: "as-inner",
                    svg { width: "0", height: "0", style: "position:absolute", "aria-hidden": "true", "focusable": "false",
                        defs {
                            clipPath { id: "about-hex-a", clip_path_units: "objectBoundingBox", path { d: clips::A } }
                            clipPath { id: "about-hex-b", clip_path_units: "objectBoundingBox", path { d: clips::B } }
                        }
                    }
                    div { class: "as-seam", id: "as-seam", aria_hidden: "true" }
                    div { class: "as-comb", id: "as-comb",
                        figure { class: "as-half as-law", id: "as-law", {video("", "about-constitution")} }
                        figure { class: "as-half as-tech", id: "as-tech", {video("", "about-code")} }
                    }
                    div { class: "as-copy", id: "as-copy", "data-type": "scroll", "data-caret": "on",
                        h2 { class: "split", span { "At the intersection" } " " span { "of law and technology." } }
                        div { class: "as-rule", aria_hidden: "true" }
                        for paragraph in body.split("</p>").filter_map(|part| part.trim().strip_prefix("<p>")) {
                            p { dangerous_inner_html: paragraph }
                        }
                        a { class: "btn btn-go", href: STORY, "Our story and our name" }
                    }
                }
            }
        }
    }
}

#[component]
fn Story(site: Arc<Website>) -> Element {
    rsx! {
        section { id: "story", class: "story",
            div { class: "banner",
                {video("banner-video", "story-midtown")}
                div { class: "wrap banner-copy",
                    p { class: "eyebrow", "Our story" }
                    h1 { span { "Built in New York," } " " span { "across three boroughs." } }
                }
            }
            div { class: "wrap",
                p { class: "story-intro", "{site.story.intro}" }
                div { class: "boroughs",
                    for place in &site.story.boroughs {
                        article { class: "borough", id: "{place.id}",
                            figure { class: "borough-photo",
                                img { src: "{ASSETS}/{place.photo}", alt: "{place.alt}", loading: "lazy" }
                                figcaption { "{place.caption}" }
                            }
                            h3 { "{place.name}" } p { "{place.copy}" }
                        }
                    }
                }
            }
        }
        section { id: "name", class: "namesake",
            {video("namesake-video", "name-masungi")}
            div { class: "wrap",
                div { class: "center-head", h2 { class: "eyebrow", "Our name" } }
                div { class: "names",
                    for (index, item) in site.story.names.iter().enumerate() {
                        if index > 0 { div { class: "names-amp", aria_hidden: "true", "&" } }
                        div { class: "name-col",
                            p { class: "name-mark", "{item.name}" }
                            for paragraph in &item.paragraphs { p { "{paragraph}" } }
                        }
                    }
                }
            }
        }
        section { class: "story-people", aria_label: "The people behind the names",
            div { class: "wrap",
                p { class: "eyebrow", "The people behind the names" }
                div { class: "people-grid", for attorney in &site.attorneys { {peer(attorney)} } }
            }
        }
    }
}

#[component]
fn Practice(site: Arc<Website>, index: usize, phone: String) -> Element {
    let copy = &site.practice[index];
    let links = site
        .practice
        .iter()
        .map(|item| (item.path.as_str(), item.title.as_str()))
        .collect();
    rsx! {
        div { class: "banner page-banner",
            img { src: "{ASSETS}/{copy.photo}", alt: "{copy.alt}" }
            div { class: "wrap banner-copy", p { class: "eyebrow", "Practice areas" } h1 { "{copy.title}" } }
        }
        div { class: "wrap practice-page",
            article { class: "practice-main",
                p { class: "practice-lead", "{copy.lead}" }
                div { class: "edge-rule", aria_hidden: "true" }
                div { dangerous_inner_html: "{copy.body}" }
            }
            aside { class: "practice-side",
                {sidebar("Practice areas", &copy.path, links)}
                div { class: "side-box side-contact",
                    p { class: "eyebrow", "Start a conversation" }
                    p { a { href: telephone(&phone), "{phone}" } br {} a { href: "mailto:{EMAIL}", "{EMAIL}" } }
                    a { class: "btn", href: CONTACT, "Contact us" }
                }
            }
        }
    }
}

#[component]
fn Legal(site: Arc<Website>, index: usize) -> Element {
    let copy = &site.legal[index];
    let links = site
        .legal
        .iter()
        .map(|item| (item.path.as_str(), item.title.as_str()))
        .collect();
    rsx! {
        div { class: "wrap legal-head", p { class: "eyebrow", "Legal" } h1 { "{copy.title}" } }
        div { class: "wrap practice-page legal-page",
            article { class: "practice-main legal-main",
                p { class: "practice-lead", "{copy.lead}" }
                div { class: "edge-rule", aria_hidden: "true" }
                div { dangerous_inner_html: "{copy.body}" }
            }
            aside { class: "practice-side", {sidebar("Legal", &copy.path, links)} }
        }
    }
}

fn sidebar(label: &str, current: &str, links: Vec<(&str, &str)>) -> Element {
    rsx! {
        nav { class: "side-box", aria_label: "{label}",
            p { class: "eyebrow", "{label}" }
            ul { class: "side-list",
                for (link, title) in links {
                    li { a { href: link, class: if link == current { "is-current" },
                        aria_current: if link == current { "page" }, "{title}" } }
                }
            }
        }
    }
}

#[component]
fn Attorney(site: Arc<Website>, index: usize) -> Element {
    let copy = &site.attorneys[index];
    rsx! {
        section { class: "bio-hero",
            div { class: "wrap bio-hero-grid",
                div { class: "bio-id",
                    h1 { class: if copy.offset { "bio-title-offset" }, "{copy.title}" }
                    p { class: "bio-role", "{copy.role}" }
                    dl { class: "bio-contact", div { dt { "Email" } dd { a { href: "mailto:{copy.email}", "{copy.email}" } } } }
                }
                figure { class: "bio-portrait",
                    div { class: "portrait has-photo",
                        img { src: "{ASSETS}/{copy.portrait}", alt: "{copy.title}", width: "1200", height: "1500" }
                    }
                }
            }
        }
        div { class: "wrap bio-body",
            article { class: "bio-main", dangerous_inner_html: "{copy.body}" }
            aside { class: "bio-facts", aria_label: "At a glance",
                p { class: "eyebrow", "At a glance" }
                dl {
                    for fact in &copy.facts {
                        div { dt { "{fact.label}" }
                            dd { ul {
                                for entry in &fact.entries {
                                    li {
                                        if let Some(href) = &entry.href { a { href: "{href}", "{entry.text}" } }
                                        else { "{entry.text}" }
                                    }
                                }
                            } }
                        }
                    }
                }
            }
        }
        section { class: "bio-more",
            div { class: "wrap bio-more-grid",
                div { class: "bio-peers",
                    p { class: "eyebrow", "Also at the firm" }
                    for other in site.attorneys.iter().filter(|other| other.path != copy.path) { {peer(other)} }
                }
                div { class: "bio-cta",
                    p { class: "eyebrow", "Contact us" }
                    h2 { class: "split", span { "Start a" } " " span { "conversation." } }
                    a { class: "btn", href: CONTACT, "Contact us" }
                }
            }
        }
    }
}

fn peer(copy: &AttorneyCopy) -> Element {
    rsx! {
        a { class: "bio-peer", href: "{copy.path}",
            span { class: "bio-peer-photo", aria_hidden: "true",
                img { src: "{ASSETS}/{portrait_variant(&copy.portrait, \"small\")}", alt: "", width: "240", height: "300", loading: "lazy" }
            }
            span { class: "bio-peer-text", b { "{copy.title}" } em { "View profile" } }
        }
    }
}

fn portrait_variant(path: &str, variant: &str) -> String {
    let stem = path.strip_suffix(".jpg").unwrap_or(path);
    format!("{stem}-{variant}.jpg")
}

fn telephone(phone: &str) -> String {
    crate::components::site_footer::tel_href(phone)
}

#[component]
fn Contact(phone: String, consent: String, lead: LeadCaptureContext) -> Element {
    rsx! {
        div { class: "wrap legal-head",
            p { class: "eyebrow", "Contact us" }
            h1 { "Start a conversation." }
            p { class: "contact-lead", "A few lines about what you need is all it takes to start." br {} "We’ll get back to you quickly." }
        }
        div { class: "wrap practice-page legal-page",
            article { class: "practice-main",
                form { class: "contact-form", action: "/leads", method: "post",
                    input { r#type: "hidden", name: "csrf_token", value: "{lead.csrf_token}" }
                    input { r#type: "hidden", name: "source_path", value: "{lead.source_path}" }
                    input { r#type: "hidden", name: "consent_version", value: "{consent}" }
                    crate::components::Honeypot { name: "website".to_string() }
                    div { class: "field",
                        label { r#for: "cf-name", "Name" }
                        input { class: "nav-input", id: "cf-name", name: "name", r#type: "text", autocomplete: "name", required: true, maxlength: "120" }
                    }
                    div { class: "field-row",
                        {crate::components::form::Field::email("Email", "email", "")
                            .id("cf-email").autocomplete("email").required().maxlength(254).render()}
                        div { class: "field",
                            label { r#for: "cf-phone", "Phone " span { class: "opt", "(optional)" } }
                            input { class: "nav-input", id: "cf-phone", name: "phone", r#type: "tel", autocomplete: "tel", maxlength: "32" }
                        }
                    }
                    div { class: "field",
                        label { r#for: "cf-need", "How can we help?" }
                        textarea { class: "nav-input", id: "cf-need", name: "concern", rows: "6", required: true, maxlength: "600",
                            placeholder: "A few lines is plenty: what it is about, where things stand and any deadlines."
                        }
                    }
                    p { class: "form-disclaimer", "Sending this form doesn’t by itself create an attorney–client relationship. Please don’t include confidential information until we’ve confirmed we can take your matter. "
                        a { href: "/legal/privacy-policy.html", "Privacy Policy" }
                    }
                    if let Some(sitekey) = lead.hcaptcha_sitekey { HCaptchaWidget { sitekey } }
                    button { class: "nav-contact-submit btn", r#type: "submit", "Send" }
                }
            }
            aside { class: "practice-side",
                div { class: "side-box side-contact",
                    p { class: "eyebrow", "Prefer to call or email?" }
                    p { a { href: telephone(&phone), "{phone}" } br {} a { href: "mailto:{EMAIL}", "{EMAIL}" } }
                }
            }
        }
    }
}
