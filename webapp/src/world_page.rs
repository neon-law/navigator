//! The Vesta `/world` page: the calculated astrocartography surface, served
//! through the Dioxus SSR port on Vesta hosts only.
//!
//! Content-backed like the contact page: the portal router pre-resolves the
//! page's [`WorldContent`] from the mounted brand and injects it through
//! `ServeConfig::context_providers`; [`world_page_view`] reads it back. The
//! specimen chart section composes the public `AstrocartographyDesign`
//! component from the map work (#233, #256); the birth form stages people
//! client-side and submits through a typed server function.
//!
//! # Birth-data privacy
//!
//! A visitor's birth details are the most sensitive data this site ever
//! touches. They live in the form's signals and the body of one server
//! function call, and are discarded when the response renders: nothing is
//! persisted, logged, forwarded to the tile server, or sent to analytics.
//! The form carries no native `action`, so a degraded client can never put
//! birth data in a URL; [`no_birth_data_travels_off_origin`] scans the
//! rendered markup to keep it that way.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::aster::{AsterArtwork, AsterPose};
use crate::components::{PublicShell, SiteHeader, SiteNavLink, SocialMeta};
use crate::public_chrome::{PublicChrome, PublicFooter};

/// How many people one chart may carry: the primary person plus five
/// additional ones. Submissions beyond this are answered per person with a
/// labeled notice rather than dropped silently.
pub const MAX_PEOPLE: usize = 6;

/// The resolved `/world` content — the portal router builds it from the
/// mounted brand and injects it; the wasm-safe carrier across the
/// server-function boundary.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct WorldContent {
    /// The full document `<title>` (`"Vesta | World"`), pre-formatted
    /// portal-side to match the head.
    pub head_title: String,
    pub meta_description: String,
    /// The page `<h1>`.
    pub page_title: String,
    /// The resolved URL of the canonical Aster board for the `AsterArtwork`
    /// viewports. Resolved server-side through the `asset_url` seam, which
    /// the wasm client build does not link.
    pub aster_asset_href: String,
    /// The bundled country display names, server-resolved from the offline
    /// `GeoNames` index (see `geotime::countries`), for the form's country
    /// options.
    pub country_names: Vec<String>,
}

/// The [`WorldContent`] injected into the render context by the portal router.
#[derive(Clone, Default)]
pub struct InjectedWorld(pub WorldContent);

/// Everything the page renders.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct WorldPageView {
    pub chrome: PublicChrome,
    pub content: WorldContent,
}

/// Resolve the chrome from the process brand and the world content from the
/// injected [`InjectedWorld`] context.
#[server]
pub async fn world_page_view() -> Result<WorldPageView, ServerFnError> {
    let content =
        crate::public_chrome::copy_from_request_or_context(consume_context::<InjectedWorld>)
            .await
            .0;
    Ok(WorldPageView {
        chrome: crate::public_chrome::firm_public_chrome_from_context().await,
        content,
    })
}

/// The page's route entry.
#[component]
pub fn WorldPageEntry() -> Element {
    let resource = use_server_future(world_page_view)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        _ => return rsx! {},
    };
    rsx! {
        WorldPage {
            chrome: view.chrome,
            content: view.content,
        }
    }
}

/// One person's staged birth details, edited in the form before submission.
#[derive(Clone, PartialEq, Default)]
pub struct PersonForm {
    /// Optional display label for the chart ("Nick", "Ada"). The chart can be
    /// cast without it.
    pub name: String,
    /// ISO-style `YYYY-MM-DD`, validated server-side.
    pub date: String,
    /// Whether the visitor knows the birth time.
    pub time_known: bool,
    /// 24-hour `HH:MM`, only meaningful when [`PersonForm::time_known`].
    pub time: String,
    /// The birth city's display name, resolved server-side against the
    /// offline index.
    pub city: String,
    /// The birth country's display name, from [`WorldContent::country_names`].
    pub country: String,
}

impl PersonForm {
    /// A blank person row with the time marked unknown, the honest default.
    #[must_use]
    pub fn new() -> Self {
        Self {
            time_known: false,
            ..Self::default()
        }
    }
}

/// One person's resolved answer. Either the place resolved to coordinates, or
/// a labeled, per-field explanation with suggestions for refining the entry.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub enum PersonOutcome {
    /// The birth place resolved; the chart can be drawn for it.
    Resolved {
        name: String,
        city: String,
        country: String,
        latitude: f64,
        longitude: f64,
        /// True when the time was unknown and noon was assumed — the reading
        /// is reduced accuracy, labeled as such.
        reduced_accuracy: bool,
    },
    /// Why this person's place did not resolve, addressed to the visitor.
    Unresolved(UnresolvedBirth),
}

/// A per-field resolution failure with closest-match suggestions.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct UnresolvedBirth {
    /// The name the results address this person by — their display name when
    /// given, otherwise their position in the chart.
    pub display_name: String,
    /// Which field to fix ("Birth date", "Birth place", "People").
    pub field_label: String,
    /// What went wrong and how to fix it, in the page's plain register.
    pub message: String,
    /// Closest-match place suggestions from the offline index.
    pub suggestions: Vec<String>,
}

/// Resolve staged birth entries through the offline index. Request-local: the
/// entries ride this one call's body and are dropped when the response
/// renders. Nothing persists, logs, or forwards them.
///
/// # Errors
///
/// A transport failure surfaces as its [`ServerFnError`]; per-person input
/// problems come back as [`PersonOutcome::Unresolved`] so one bad field never
/// fails the whole family's chart.
#[server]
// A server function's wire shape is `async` by contract even when its body
// never awaits; the macro's generated transport requires it.
#[allow(clippy::unused_async)]
pub async fn resolve_birth_data(
    entries: Vec<BirthEntry>,
) -> Result<Vec<PersonOutcome>, ServerFnError> {
    Ok(entries
        .iter()
        .take(MAX_PEOPLE)
        .enumerate()
        .map(|(position, entry)| resolve_person(position, entry))
        .chain(
            entries
                .iter()
                .enumerate()
                .skip(MAX_PEOPLE)
                .map(|(position, entry)| {
                    PersonOutcome::Unresolved(UnresolvedBirth {
                        display_name: entry.display_name(position),
                        field_label: "People".to_string(),
                        message: format!("A chart carries up to {MAX_PEOPLE} people."),
                        suggestions: Vec::new(),
                    })
                }),
        )
        .collect())
}

/// The server function's wire entry: the page's staged form flattened to what
/// the calculation needs. Not server-gated — the wasm client names the type
/// to serialize its form state onto the wire.
#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct BirthEntry {
    /// Optional display name for the chart.
    pub name: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// Optional 24-hour `HH:MM`; `None` means unknown, and noon is assumed.
    pub time: Option<String>,
    /// The city's display name as typed or chosen.
    pub city: String,
    /// The country's display name as chosen.
    pub country: String,
}

impl BirthEntry {
    /// The name the results address this person by: their display name when
    /// given, otherwise their position in the chart.
    fn display_name(&self, position: usize) -> String {
        let trimmed = self.name.trim();
        if trimmed.is_empty() {
            format!("Person {}", position + 1)
        } else {
            trimmed.to_string()
        }
    }
}

/// The server-side resolution of one entry: date and time validated, the
/// place resolved through the offline index. Server-only so the wasm client
/// never carries the `GeoNames` index or the tzdata.
#[cfg(feature = "server")]
fn resolve_person(position: usize, entry: &BirthEntry) -> PersonOutcome {
    use chrono::{Datelike, NaiveDate};

    let display_name = entry.display_name(position);

    let Some(date) = NaiveDate::parse_from_str(entry.date.trim(), "%Y-%m-%d").ok() else {
        return unresolved(
            display_name,
            "Birth date",
            "Enter the birth date as a complete date — for example 1990-06-15.",
        );
    };
    if !(MIN_EPHEMERIS_YEAR..=MAX_EPHEMERIS_YEAR).contains(&date.year()) {
        return unresolved(
            display_name,
            "Birth date",
            format!(
                "The birth date falls outside the calculated range ({MIN_EPHEMERIS_YEAR}–\
                 {MAX_EPHEMERIS_YEAR}), so the lines cannot be drawn for it."
            )
            .as_str(),
        );
    }

    let time = match entry.time.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(given) => match chrono::NaiveTime::parse_from_str(given, "%H:%M").ok() {
            Some(parsed) => Some(parsed),
            None => {
                return unresolved(
                    display_name,
                    "Birth time",
                    "Enter the birth time in 24-hour form — for example 14:30 — or clear \
                         the field to mark the time unknown.",
                );
            }
        },
    };

    match geotime::resolve(entry.city.trim(), entry.country.trim(), date, time) {
        Ok(geo) => PersonOutcome::Resolved {
            name: display_name,
            city: geo.city.to_string(),
            country: entry.country.trim().to_string(),
            latitude: geo.latitude,
            longitude: geo.longitude,
            reduced_accuracy: time.is_none(),
        },
        Err(geotime::ResolveError::UnknownCountry { suggestions, .. }) => {
            PersonOutcome::Unresolved(UnresolvedBirth {
                display_name,
                field_label: "Birth country".to_string(),
                message: "No country matched that name. Choose the closest option from the \
                          country list, or refine the spelling."
                    .to_string(),
                suggestions: suggestions
                    .iter()
                    .map(|suggestion| suggestion.name.to_string())
                    .collect(),
            })
        }
        Err(geotime::ResolveError::UnknownCity {
            city,
            country,
            suggestions,
        }) => PersonOutcome::Unresolved(UnresolvedBirth {
            display_name,
            field_label: "Birth place".to_string(),
            message: format_unresolved_city(&city, &country),
            suggestions: suggestions
                .iter()
                .map(|suggestion| suggestion.name.to_string())
                .collect(),
        }),
    }
}

/// The validated calculation window: the years the golden anchors (#214)
/// cover, outside which the line engine's answers are not verified.
#[cfg(feature = "server")]
const MIN_EPHEMERIS_YEAR: i32 = 1900;

/// See [`MIN_EPHEMERIS_YEAR`].
#[cfg(feature = "server")]
const MAX_EPHEMERIS_YEAR: i32 = 2050;

/// A labeled, addressable failure for one person.
#[cfg(feature = "server")]
fn unresolved(display_name: String, field_label: &str, message: &str) -> PersonOutcome {
    PersonOutcome::Unresolved(UnresolvedBirth {
        display_name,
        field_label: field_label.to_string(),
        message: message.to_string(),
        suggestions: Vec::new(),
    })
}

/// The wording for a country that resolved but had no matching city, with the
/// country named and the suggestions carried separately.
#[cfg(feature = "server")]
fn format_unresolved_city(city: &str, country: &str) -> String {
    format!(
        "We could not find {city} in {country}. Check the spelling, or pick a nearby \
         city from the suggestions — the lines move little within a region."
    )
}

/// The pure `/world` page: the specimen chart section inside the public shell,
/// with the birth form, the Aster guide, and the privacy note. Prop-driven, so
/// it server-renders and unit-tests without a server future.
#[component]
pub fn WorldPage(chrome: PublicChrome, content: WorldContent) -> Element {
    let header = rsx! {
        SiteHeader {
            brand_name: chrome.brand_name.clone(),
            home_href: chrome.home_href.clone(),
            logo_href: chrome.logo_href.clone(),
            destinations: chrome
                .destinations
                .iter()
                .map(|link| SiteNavLink::new(link.label.clone(), link.href.clone()))
                .collect(),
            utility: chrome
                .utility
                .iter()
                .map(|link| SiteNavLink::new(link.label.clone(), link.href.clone()))
                .collect(),
        }
    };
    let footer = rsx! {
        PublicFooter { chrome: chrome.clone() }
    };
    rsx! {
        document::Title { "{content.head_title}" }
        document::Meta { name: "description", content: "{content.meta_description}" }
        SocialMeta {
            title: content.head_title.clone(),
            description: content.meta_description.clone(),
            og_description: None,
            site_name: chrome.brand_name.clone(),
            image: chrome.social_image.clone(),
        }
        PublicShell { header, footer,
            article { class: "world-page",
                header { class: "world-page__hero",
                    p { class: "world-page__eyebrow", "Places · Legacies · Lines" }
                    h1 { "{content.page_title}" }
                    p { class: "world-page__intro",
                        "Look up the planetary lines that were overhead at a moment that \
                         matters — a birth, a beginning, a place you are considering — and \
                         read the sky the way a navigator reads a chart."
                    }
                    p { class: "world-page__privacy", role: "note",
                        "Birth details are calculated within your request, shown back to \
                         you in the response, and discarded with it. Nothing is stored, \
                         logged, shared with the map's tile provider, or sent to analytics."
                    }
                }
                AsterGuide { aster_asset_href: content.aster_asset_href.clone() }
                WorldMapSection {}
                BirthFormSection { country_names: content.country_names.clone() }
            }
        }
    }
}

/// Aster the Astrocartographer, framed from the canonical board, introducing
/// the reading. Decorative artwork: the nearby copy carries the meaning.
#[component]
fn AsterGuide(aster_asset_href: String) -> Element {
    rsx! {
        aside { class: "world-page__aster", "aria-label": "A note from Aster, the guide",
            AsterArtwork { src: aster_asset_href, pose: AsterPose::Guide }
            p {
                "I'm Aster, the Astrocartographer. A chart is not a verdict — it is a \
                 map of what the sky was doing. Read it the way you would read a map of \
                 rivers and ridges: as terrain, not as fate."
            }
        }
    }
}

/// The specimen chart section: the fully server-rendered calculated specimen
/// with its provenance, the static SVG map, the `MapLibre` island, and the map
/// reading text. Composes the public `AstrocartographyDesign` component.
#[component]
fn WorldMapSection() -> Element {
    rsx! {
        section { class: "world-page__map", "aria-label": "Calculated specimen chart",
            crate::astrocartography::AstrocartographyDesign {}
        }
    }
}

/// The birth-data form: one primary person plus staged additional people,
/// submitted through the [`resolve_birth_data`] server function. No native
/// form action — the submit handler keeps birth details out of any URL.
#[component]
fn BirthFormSection(country_names: Vec<String>) -> Element {
    let mut staged = use_signal(|| vec![PersonForm::new()]);
    let mut outcomes: Signal<Option<Vec<PersonOutcome>>> = use_signal(|| None);
    let mut failed = use_signal(|| false);
    let mut submitting = use_signal(|| false);

    let stage_person = move |_| {
        if staged().len() < MAX_PEOPLE {
            staged.push(PersonForm::new());
        }
    };
    rsx! {
        section { class: "world-page__form", "aria-label": "Birth details",
            h2 { "Your chart" }
            p {
                "Enter birth details to see how the sky stood for a person and a place. \
                 Birth time is optional — when it is unknown, the chart is cast for noon \
                 and labeled as reduced accuracy."
            }
            for (slot, person) in staged.iter().enumerate() {
                div { class: "world-person", role: "group", "aria-label": "{person_label(slot)}",
                    h3 { "{person_label(slot)}" }
                    div { class: "world-person__grid",
                        labeled_input {
                            label: person_label(slot),
                            field: "name",
                            value: person.name.clone(),
                            placeholder: "Name (optional)",
                            oninput: move |value: String| staged.with_mut(|people| people[slot].name = value),
                        }
                        labeled_input {
                            label: person_label(slot),
                            field: "birth date",
                            value: person.date.clone(),
                            placeholder: "1990-06-15",
                            oninput: move |value: String| staged.with_mut(|people| people[slot].date = value),
                        }
                        div { class: "world-person__time",
                            label {
                                input {
                                    class: "nav-checkbox",
                                    r#type: "checkbox",
                                    checked: person.time_known,
                                    onchange: move |_| staged.with_mut(|people| people[slot].time_known = !people[slot].time_known),
                                }
                                "I know this birth time"
                            }
                            if person.time_known {
                                labeled_input {
                                    label: person_label(slot),
                                    field: "birth time",
                                    value: person.time.clone(),
                                    placeholder: "14:30",
                                    oninput: move |value: String| staged.with_mut(|people| people[slot].time = value),
                                }
                            }
                        }
                        labeled_input {
                            label: person_label(slot),
                            field: "birth city",
                            value: person.city.clone(),
                            placeholder: "City of birth",
                            oninput: move |value: String| staged.with_mut(|people| people[slot].city = value),
                        }
                        div { class: "world-person__country",
                            label { "Birth country" }
                            select {
                                class: "nav-select",
                                "aria-label": "{person_label(slot)} birth country",
                                value: "{person.country}",
                                onchange: move |event| {
                                    let chosen = event.value();
                                    staged.with_mut(|people| people[slot].country = chosen);
                                },
                                option { value: "", "Select a country…" }
                                for name in country_names.iter() {
                                    option { value: "{name}", "{name}" }
                                }
                            }
                        }
                    }
                    if slot > 0 {
                        button { class: "nav-btn", r#type: "button",
                            onclick: move |_| staged.with_mut(|people| { people.remove(slot); }),
                            "Remove {person_label(slot)}"
                        }
                    }
                }
            }
            if staged().len() < MAX_PEOPLE {
                button { class: "nav-btn", r#type: "button", onclick: stage_person,
                    "Add another person"
                }
            }
            button {
                class: "nav-btn nav-btn--primary",
                r#type: "button",
                disabled: submitting(),
                onclick: move |_| {
                    submitting.set(true);
                    failed.set(false);
                    let entries: Vec<BirthEntry> = staged()
                        .iter()
                        .map(|person| BirthEntry {
                            name: person.name.clone(),
                            date: person.date.clone(),
                            time: person.time_known.then(|| person.time.clone()),
                            city: person.city.clone(),
                            country: person.country.clone(),
                        })
                        .collect();
                    spawn(async move {
                        match resolve_birth_data(entries).await {
                            Ok(resolved) => {
                                outcomes.set(Some(resolved));
                                failed.set(false);
                            }
                            Err(_) => {
                                failed.set(true);
                            }
                        }
                        submitting.set(false);
                    });
                },
                if submitting() { "Calculating…" } else { "Draw the lines" }
            }
            if failed() {
                p { class: "nav-form-error", role: "alert",
                    "The chart could not be calculated. Check the connection and try again."
                }
            }
            BirthResults { outcomes: outcomes() }
        }
    }
}

/// The results panel: the resolved places read back to the submitter, with
/// labeled failures and suggestions for anything that did not resolve.
#[component]
fn BirthResults(outcomes: Option<Vec<PersonOutcome>>) -> Element {
    let Some(outcomes) = outcomes else {
        return rsx! {};
    };
    rsx! {
        div { class: "world-results", "aria-live": "polite",
            h3 { "The chart" }
            for outcome in outcomes {
                match outcome {
                    PersonOutcome::Resolved { name, city, country, latitude, longitude, reduced_accuracy } => rsx! {
                        div { class: "world-results__person",
                            h4 { "{name}" }
                            p {
                                "Birth place resolved: {city}, {country} ({latitude:.4}, {longitude:.4})."
                            }
                            if reduced_accuracy {
                                p { "The birth time was unknown, so the chart is cast for noon \
                                     local time — a reduced-accuracy reading."
                                }
                            }
                            p { class: "world-results__next",
                                "The full line drawing for this chart arrives with the \
                                 calculation pipeline; the specimen chart above shows the \
                                 engine's output for a fixed instant."
                            }
                        }
                    },
                    PersonOutcome::Unresolved(failure) => rsx! {
                        div { class: "world-results__person world-results__unresolved",
                            h4 { "{failure.display_name}" }
                            p { role: "alert", "{failure.field_label}: {failure.message}" }
                            if !failure.suggestions.is_empty() {
                                p { "Did you mean: " }
                                ul {
                                    for suggestion in failure.suggestions {
                                        li { "{suggestion}" }
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// The accessible row label for person `slot`.
fn person_label(slot: usize) -> String {
    if slot == 0 {
        "Primary person".to_string()
    } else {
        format!("Person {}", slot + 1)
    }
}

/// A labeled text input for one person field.
#[component]
fn labeled_input(
    label: String,
    field: &'static str,
    value: String,
    placeholder: &'static str,
    oninput: EventHandler<String>,
) -> Element {
    let id = format!("{label} {field}").to_lowercase().replace(' ', "-");
    rsx! {
        div { class: "world-person__field",
            label { r#for: "{id}", "{field_label(field)}" }
            input {
                class: "nav-input",
                id: "{id}",
                r#type: "text",
                "aria-label": "{label} {field}",
                value: "{value}",
                placeholder,
                oninput: move |event| oninput.call(event.value()),
            }
        }
    }
}

/// The visible label text for a person field ("name" renders as "Name").
fn field_label(field: &str) -> String {
    let mut chars = field.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Server-render `app` the way the SSR port does.
    fn render(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    /// A realistic chrome: same-origin home and logo hrefs, so the privacy
    /// scan exercises real link shapes rather than empty defaults.
    fn chrome() -> PublicChrome {
        let mut chrome = PublicChrome {
            brand_name: "Vesta".to_string(),
            ..Default::default()
        };
        chrome.home_href = "/".to_string();
        chrome.logo_href = "/public/brand/vesta.svg".to_string();
        chrome
    }

    fn content() -> WorldContent {
        WorldContent {
            head_title: "Vesta | World".to_string(),
            meta_description: "Calculated astrocartography.".to_string(),
            page_title: "World".to_string(),
            aster_asset_href: "/public/img/vesta-aster/aster-canon-7d379560.png".to_string(),
            country_names: vec!["United States".to_string(), "Ireland".to_string()],
        }
    }

    fn app() -> Element {
        rsx! {
            WorldPage {
                chrome: chrome(),
                content: content(),
            }
        }
    }

    /// The page renders the hero, the Aster guide, the specimen map, and the
    /// birth form inside the public shell.
    #[test]
    fn renders_hero_aster_map_and_form() {
        let html = render(app);
        assert!(html.contains("World"), "page title: {html}");
        assert!(html.contains("Aster, the Astrocartographer"), "{html}");
        assert!(html.contains("aster-artwork"), "Aster viewport: {html}");
        assert!(
            html.contains("astro-map-design"),
            "specimen section: {html}"
        );
        assert!(html.contains("Your chart"), "form heading: {html}");
        assert!(html.contains("Primary person"), "form rows: {html}");
        assert!(
            html.contains("Select a country…"),
            "country options: {html}"
        );
        assert!(html.contains("Add another person"), "staged people: {html}");
        // The specimen's calculated-chart notice and its provenance span.
        assert!(html.contains("Calculated specimen chart"), "{html}");
        assert!(html.contains("astro-map-provenance"), "{html}");
    }

    /// The privacy note names every place the data does not go.
    #[test]
    fn privacy_note_names_every_exclusion() {
        let html = render(app);
        for phrase in ["Nothing is stored, logged", "tile provider", "analytics"] {
            assert!(html.contains(phrase), "{phrase}: {html}");
        }
    }

    /// Birth data never travels off origin: no native form action (which would
    /// put details in a URL), and no remote destination in any rendered href —
    /// only the static Natural Earth attribution and the chrome's repository
    /// link, which carry no visitor data.
    #[test]
    fn no_birth_data_travels_off_origin() {
        let html = render(app);
        assert!(!html.contains("<form"), "no native form: {html}");
        for attribute in [
            "action=",
            "fetch(",
            "XMLHttpRequest",
            "navigator.sendBeacon",
        ] {
            assert!(!html.contains(attribute), "{attribute}: {html}");
        }
        for url in [
            "https://www.naturalearthdata.com",
            "https://github.com/nvkelso/natural-earth-vector",
            "https://github.com/neon-law/navigator",
        ] {
            assert!(html.contains(url), "known static link present: {url}");
        }
        let remote = html.matches("href=\"http").count()
            + html.matches("src=\"http").count()
            + html.matches("src='http").count();
        // The allowed hosts, each appearing once as an href: the map data's
        // terms of use, the land-silhouette source, and the firm's repo link.
        let allowed = html
            .matches("href=\"https://www.naturalearthdata.com")
            .count()
            + html
                .matches("href=\"https://github.com/nvkelso/natural-earth-vector")
                .count()
            + html
                .matches("href=\"https://github.com/neon-law/navigator")
                .count();
        assert_eq!(remote, allowed, "every remote link is allowlisted");
    }

    /// The form's controls are labeled for assistive technology.
    #[test]
    fn form_fields_are_labeled() {
        let html = render(app);
        assert!(
            html.contains("aria-label=\"Primary person name\""),
            "name label: {html}"
        );
        assert!(
            html.contains("aria-label=\"Primary person birth country\""),
            "country label: {html}"
        );
        assert!(html.contains("<label"), "{html}");
    }

    /// Unresolved places read back with a labeled field, the plain-language
    /// reason, and suggestions.
    #[test]
    fn unresolved_place_reads_back_with_suggestions() {
        fn results_app() -> Element {
            rsx! {
                BirthResults {
                    outcomes: Some(vec![PersonOutcome::Unresolved(UnresolvedBirth {
                        display_name: "Person 2".to_string(),
                        field_label: "Birth place".to_string(),
                        message: format_unresolved_city("Portand", "Ireland"),
                        suggestions: vec!["Portland".to_string()],
                    })]),
                }
            }
        }
        let html = render(results_app);
        assert!(html.contains("Birth place"), "{html}");
        assert!(html.contains("Portand in Ireland"), "{html}");
        // Dioxus SSR escapes the apostrophe in "didn't"-style copy.
        assert!(html.contains("Portland"), "suggestions: {html}");
        assert!(html.contains("aria-live=\"polite\""), "{html}");
    }

    /// A resolved place reads back with coordinates and the reduced-accuracy
    /// label when the time was unknown.
    #[test]
    fn resolved_place_reads_back_with_accuracy_label() {
        fn results_app() -> Element {
            rsx! {
                BirthResults {
                    outcomes: Some(vec![PersonOutcome::Resolved {
                        name: "Ada".to_string(),
                        city: "Dublin".to_string(),
                        country: "Ireland".to_string(),
                        latitude: 53.349_8,
                        longitude: -6.260_3,
                        reduced_accuracy: true,
                    }]),
                }
            }
        }
        let html = render(results_app);
        assert!(html.contains("Dublin, Ireland"), "{html}");
        assert!(html.contains("53.3498"), "{html}");
        assert!(html.contains("reduced-accuracy"), "{html}");
    }
}
