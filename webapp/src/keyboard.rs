//! The keyboard-shortcut registry — the one list every shortcut is declared in.
//!
//! A page declares what it answers to as a key, a description and a [`Scope`],
//! and the `?` help overlay lists exactly those declarations. The browser half,
//! `server/public/js/keyboard-shortcuts.js`, reads them back out of the page, so
//! a key that works is a key that is listed and a listed key works: there is no
//! second table to drift.
//!
//! The notation and the chord map:
//!
//! * a key is written as a person reads it — `?`, `1`, `Mod+Enter`,
//!   `Alt+ArrowLeft`; `Mod` is ⌘ on Apple platforms and Ctrl elsewhere;
//! * a chord is keys separated by spaces — `g m` is G, then M — and must be
//!   finished within [`CHORD_TIMEOUT_MS`] of its last key;
//! * a digit range such as `1-9` is one declaration for the whole row of keys.
//!
//! **What fires where.** A bare key never fires while focus is in a text input,
//! textarea, select or contenteditable region: that is typing. A declaration
//! opts in with [`Shortcut::in_fields`], which only a modified key such as
//! `Mod+Enter` should.
//!
//! **Where it mounts.** [`ShortcutHost`] rides in [`crate::components::AppProfileMenu`],
//! the one component every authenticated page's chrome renders, so no page wires
//! it. A page with shortcuts of its own renders [`PageShortcuts`]; the
//! questionnaire's [`crate::components::QuestionStage`] does.

use crate::app_chrome::get_palette_destinations;
use crate::components::AppNavLink;
use dioxus::prelude::*;
use serde::Serialize;

/// Same-origin script that reads the declarations and acts on them. Dioxus
/// dedupes `document::Script` by this `src`.
pub const KEYBOARD_SCRIPT_HREF: &str = "/public/js/keyboard-shortcuts.js";

/// How long a half-typed chord waits for its next key. The host emits it as
/// `data-chord-timeout-ms` and the script reads it from there.
pub const CHORD_TIMEOUT_MS: u32 = 1000;

/// The attribute that marks a block of declarations for the script to collect.
pub const DECLARATION_ATTRIBUTE: &str = "data-nav-shortcuts";

/// The `id` of the help overlay's `<dialog>`.
pub const HELP_DIALOG_ID: &str = "nav-shortcuts-dialog";

/// The `id` of the `/` palette's `<dialog>`.
pub const PALETTE_DIALOG_ID: &str = "nav-palette-dialog";

/// The attribute that marks the palette's role-scoped destinations for the
/// script to read.
pub const PALETTE_DESTINATIONS_ATTRIBUTE: &str = "data-nav-palette-destinations";

/// The palette's filter-field hook.
pub const PALETTE_FILTER_ATTRIBUTE: &str = "data-nav-palette-filter";

/// The palette's results-list hook.
pub const PALETTE_RESULTS_ATTRIBUTE: &str = "data-nav-palette-results";

/// The palette's close-control hook.
pub const PALETTE_CLOSE_ATTRIBUTE: &str = "data-nav-palette-close";

/// The attribute a palette row carries its destination on.
pub const PALETTE_OPTION_HREF_ATTRIBUTE: &str = "data-href";

/// `global` shortcuts work on every page; `page` shortcuts only while it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Global,
    Page,
}

/// What the script does when a shortcut fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Open the `?` overlay.
    Help,
    /// Open the `/` search palette.
    OpenPalette,
    /// Go to [`Shortcut::href`].
    Navigate,
    /// Submit the questionnaire's question form (the server advances it, or
    /// completes the questionnaire on the last question).
    Next,
    /// Return to the prior answerable question without changing answers.
    Back,
    /// Click the numbered radio or checkbox option of the question.
    Choose,
}

/// One declaration: a key, what it does, where it applies, and how it is run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Shortcut {
    pub keys: String,
    pub description: String,
    pub scope: Scope,
    pub action: Action,
    /// Destination of an [`Action::Navigate`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// Fire even while focus is in a text field.
    pub in_fields: bool,
}

impl Shortcut {
    fn new(keys: &str, description: &str, scope: Scope, action: Action) -> Self {
        Self {
            keys: keys.to_string(),
            description: description.to_string(),
            scope,
            action,
            href: None,
            in_fields: false,
        }
    }
}

/// A main page a chord can move to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Matters,
    Notations,
    Documents,
}

/// The global navigation chords, defined once: the page, its chord, and the
/// overlay's row label.
pub const NAVIGATION_CHORDS: [(Page, &str, &str); 3] = [
    (Page::Matters, "g m", "Go to matters"),
    (Page::Notations, "g n", "Go to notations"),
    (Page::Documents, "g d", "Go to documents"),
];

/// The main pages this application has a top-level route for. A matter's
/// notations and documents live under the matter, not behind a list of their
/// own, so only matters is here and the other two chords are left out.
#[must_use]
pub fn app_pages() -> Vec<(Page, &'static str)> {
    vec![(Page::Matters, crate::app_chrome::APP_PROJECTS_HREF)]
}

/// The global shortcuts for the pages a portal has: the `?` overlay, the `/`
/// palette, then a chord for each page in `pages`. A page left out has no
/// chord, so it is neither fired nor listed.
#[must_use]
pub fn global_shortcuts(pages: &[(Page, &str)]) -> Vec<Shortcut> {
    let mut shortcuts = vec![
        Shortcut::new("?", "Show keyboard shortcuts", Scope::Global, Action::Help),
        Shortcut::new("/", "Open search", Scope::Global, Action::OpenPalette),
    ];
    for (page, keys, description) in NAVIGATION_CHORDS {
        if let Some((_, href)) = pages.iter().find(|(candidate, _)| *candidate == page) {
            shortcuts.push(Shortcut {
                href: Some((*href).to_string()),
                ..Shortcut::new(keys, description, Scope::Global, Action::Navigate)
            });
        }
    }
    shortcuts
}

/// The questionnaire's shortcuts. `has_choices` is whether the current question
/// is a radio or checkbox question, which is what makes the numbered keys
/// meaningful.
///
/// `can_go_back` is true only when the current question has an earlier
/// answerable state. Back keys are intentionally ignored in editable fields;
/// this preserves the browser's native text-editing behavior.
#[must_use]
pub fn questionnaire_shortcuts(has_choices: bool, can_go_back: bool) -> Vec<Shortcut> {
    let mut shortcuts = vec![Shortcut {
        in_fields: true,
        ..Shortcut::new(
            "Mod+Enter",
            "Next question, or submit on the last",
            Scope::Page,
            Action::Next,
        )
    }];
    if can_go_back {
        shortcuts.push(Shortcut {
            ..Shortcut::new(
                "Mod+Backspace",
                "Previous question",
                Scope::Page,
                Action::Back,
            )
        });
        shortcuts.push(Shortcut {
            ..Shortcut::new(
                "Alt+ArrowLeft",
                "Previous question",
                Scope::Page,
                Action::Back,
            )
        });
    }
    if has_choices {
        shortcuts.push(Shortcut::new(
            "1-9",
            "Choose the numbered option",
            Scope::Page,
            Action::Choose,
        ));
    }
    shortcuts
}

/// Why a key notation is not valid.
#[derive(Debug, PartialEq, Eq)]
pub struct KeyError(pub String);

/// Check a key notation: modifiers from `Mod`/`Alt`/`Shift`, then one key, and
/// space-separated steps for a chord. Every declared key must pass, so the
/// script can rely on the grammar.
///
/// # Errors
///
/// Returns the reason when a step is empty or names an unknown modifier.
pub fn validate_keys(spec: &str) -> Result<(), KeyError> {
    let steps: Vec<&str> = spec.split(' ').filter(|step| !step.is_empty()).collect();
    if steps.is_empty() {
        return Err(KeyError("an empty key".to_string()));
    }
    for step in steps {
        let key = if step == "+" || step.ends_with("++") {
            "+"
        } else {
            step.rsplit('+').next().unwrap_or_default()
        };
        if key.is_empty() {
            return Err(KeyError(format!("no key in `{step}`")));
        }
        let head = &step[..step.len() - key.len()];
        for modifier in head.split('+').filter(|part| !part.is_empty()) {
            if !matches!(
                modifier.to_ascii_lowercase().as_str(),
                "mod" | "alt" | "shift"
            ) {
                return Err(KeyError(format!(
                    "unknown modifier `{modifier}` in `{step}`"
                )));
            }
        }
    }
    Ok(())
}

/// The declarations as the JSON the script reads.
fn declarations_json(shortcuts: &[Shortcut]) -> String {
    serde_json::to_string(shortcuts).unwrap_or_else(|_| "[]".to_string())
}

/// Declares a page's own shortcuts, for the script to collect and the overlay
/// to list under "On this page". Renders nothing visible.
#[component]
pub fn PageShortcuts(shortcuts: Vec<Shortcut>) -> Element {
    let json = declarations_json(&shortcuts);
    rsx! {
        div { hidden: true, "data-nav-shortcuts": "{json}" }
    }
}

/// The global shortcuts, the `?` overlay's dialog, and the `/` palette's
/// dialog, once per page.
///
/// Each dialog is a native modal `<dialog>`: the platform supplies the focus
/// trap, the inert background and `Esc`, and closed, its children are inert.
/// Their titles label them for assistive technology. The script fills the help
/// list from every declaration on the page, and filters the palette over the
/// role-scoped destinations the server resolved for this request.
#[component]
pub fn ShortcutHost() -> Element {
    let json = declarations_json(&global_shortcuts(&app_pages()));
    let resource = use_server_future(get_palette_destinations)?;
    // Clone out of the read guard before rendering so the borrow does not
    // outlive it (the `rsx!` output escapes this scope). Unresolved — a direct
    // hit on the endpoint outside the auth gate, or a failed fetch — is an
    // empty list: the palette degrades to no rows rather than a wider set.
    let destinations = match &*resource.read() {
        Some(Ok(destinations)) => destinations.clone(),
        _ => Vec::new(),
    };
    rsx! {
        document::Script { src: KEYBOARD_SCRIPT_HREF, defer: true }
        div {
            hidden: true,
            "data-nav-shortcuts": "{json}",
            "data-chord-timeout-ms": "{CHORD_TIMEOUT_MS}",
        }
        dialog {
            id: HELP_DIALOG_ID,
            class: "nav-shortcuts-dialog nav-card",
            "aria-labelledby": "{HELP_DIALOG_ID}-title",
            div { class: "nav-shortcuts-dialog__header",
                h2 { id: "{HELP_DIALOG_ID}-title", class: "nav-shortcuts-dialog__title",
                    "Keyboard shortcuts"
                }
                button {
                    class: "nav-shortcuts-dialog__close",
                    r#type: "button",
                    "aria-label": "Close",
                    "data-nav-shortcuts-close": "true",
                    span { "aria-hidden": "true", "×" }
                }
            }
            div { class: "nav-shortcuts-dialog__body", "data-nav-shortcuts-list": "true" }
        }
        PaletteDialog { destinations }
    }
}

/// The `/` palette's dialog, beside the `?` overlay's: a filter field and the
/// viewer's role-scoped destinations.
///
/// `destinations` arrives resolved — [`crate::app_chrome::get_palette_destinations`]
/// during SSR, carried in the `data-nav-palette-destinations` attribute the
/// same way the declarations ride their block. The browser half reads the
/// attribute and filters what the server already scoped; nothing here
/// recomputes or widens the set.
#[component]
fn PaletteDialog(destinations: Vec<AppNavLink>) -> Element {
    let json = serde_json::to_string(&destinations).unwrap_or_else(|_| "[]".to_string());
    rsx! {
        dialog {
            id: PALETTE_DIALOG_ID,
            class: "nav-palette-dialog nav-card",
            "aria-labelledby": "{PALETTE_DIALOG_ID}-title",
            "data-nav-palette-destinations": "{json}",
            div { class: "nav-palette-dialog__header",
                h2 { id: "{PALETTE_DIALOG_ID}-title", class: "nav-palette-dialog__title",
                    "Search"
                }
                button {
                    class: "nav-palette-dialog__close",
                    r#type: "button",
                    "aria-label": "Close",
                    "data-nav-palette-close": "true",
                    span { "aria-hidden": "true", "×" }
                }
            }
            // The structural a11y gate (server/tests/routes.rs) requires every
            // visible input to carry an id and a matching `<label for>`; the
            // visually-hidden label names it without altering the dialog's layout.
            label {
                r#for: "{PALETTE_DIALOG_ID}-filter",
                class: "nav-visually-hidden",
                "Filter destinations",
            }
            input {
                id: "{PALETTE_DIALOG_ID}-filter",
                class: "nav-palette-dialog__filter",
                r#type: "search",
                "data-nav-palette-filter": "true",
                placeholder: "Search destinations…",
                "aria-label": "Filter destinations",
            }
            ul {
                class: "nav-palette-dialog__results",
                "data-nav-palette-results": "true",
                role: "listbox",
                "aria-label": "Destinations",
                for destination in destinations.iter() {
                    li {
                        class: "nav-palette-dialog__option",
                        role: "option",
                        "data-href": "{destination.href}",
                        "{destination.label}"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_chrome::palette_destinations;

    fn ssr(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    /// The declarations a rendered page carries, in document order — what the
    /// script collects, read back out of the HTML.
    fn declared(html: &str) -> Vec<serde_json::Value> {
        let marker = format!("{DECLARATION_ATTRIBUTE}=\"");
        let mut found = Vec::new();
        let mut rest = html;
        while let Some(start) = rest.find(&marker) {
            let after = &rest[start + marker.len()..];
            let end = after.find('"').expect("attribute closes");
            let raw = after[..end]
                .replace("&#34;", "\"")
                .replace("&quot;", "\"")
                .replace("&amp;", "&")
                .replace("&#x27;", "'");
            let parsed: Vec<serde_json::Value> = serde_json::from_str(&raw).expect("valid JSON");
            found.extend(parsed);
            rest = &after[end..];
        }
        found
    }

    #[test]
    fn every_declared_key_is_valid_notation() {
        let all = global_shortcuts(&[
            (Page::Matters, "/m"),
            (Page::Notations, "/n"),
            (Page::Documents, "/d"),
        ])
        .into_iter()
        .chain(questionnaire_shortcuts(true, true));
        for shortcut in all {
            assert_eq!(validate_keys(&shortcut.keys), Ok(()), "{}", shortcut.keys);
        }
    }

    #[test]
    fn the_notation_rejects_what_the_script_cannot_read() {
        assert_eq!(validate_keys("Mod+Enter"), Ok(()));
        assert_eq!(validate_keys("g m"), Ok(()));
        assert_eq!(validate_keys("/"), Ok(()));
        assert_eq!(validate_keys("Mod++"), Ok(()));
        assert_eq!(validate_keys("+"), Ok(()));
        assert!(validate_keys("").is_err());
        assert!(validate_keys("Hyper+x").is_err());
        assert!(validate_keys("Mod+").is_err());
    }

    #[test]
    fn the_chord_map_is_g_m_g_n_g_d() {
        let map: Vec<_> = NAVIGATION_CHORDS.iter().map(|(_, keys, _)| *keys).collect();
        assert_eq!(map, ["g m", "g n", "g d"]);
    }

    #[test]
    fn a_page_the_application_does_not_have_gets_no_chord() {
        let keys: Vec<String> = global_shortcuts(&app_pages())
            .into_iter()
            .map(|shortcut| shortcut.keys)
            .collect();
        assert_eq!(
            keys,
            ["?", "/", "g m"],
            "only matters has a top-level route"
        );
        let none = global_shortcuts(&[]);
        assert_eq!(
            none.len(),
            2,
            "the overlay key and the palette key are always there"
        );
    }

    #[test]
    fn the_palette_key_opens_the_search_palette() {
        let palette = global_shortcuts(&app_pages())
            .into_iter()
            .find(|shortcut| shortcut.keys == "/")
            .expect("a global `/` declaration");
        assert_eq!(palette.action, Action::OpenPalette);
        assert_eq!(palette.scope, Scope::Global);
        assert_eq!(palette.description, "Open search");
        assert_eq!(
            palette.href, None,
            "the palette opens instead of navigating"
        );
        assert!(!palette.in_fields, "a bare key never fires in a field");
    }

    /// The help overlay lists every declaration the page carries, so the
    /// rendered host's declarations — what the script builds the list from —
    /// must include the `/` row.
    #[test]
    fn the_host_declares_the_palette_key_for_the_help_list() {
        fn app() -> Element {
            rsx! { ShortcutHost {} }
        }
        let html = ssr(app);
        let row = declared(&html)
            .into_iter()
            .find(|row| row["keys"] == "/")
            .expect("the rendered declarations include `/`");
        assert_eq!(row["action"], "open_palette", "{row:?}");
        assert_eq!(row["in_fields"], false, "{row:?}");
        assert_eq!(row["scope"], "global", "{row:?}");
    }

    #[test]
    fn a_chord_names_the_page_it_goes_to() {
        let matters = global_shortcuts(&app_pages())
            .into_iter()
            .find(|shortcut| shortcut.keys == "g m")
            .expect("g m");
        assert_eq!(matters.action, Action::Navigate);
        assert_eq!(matters.href.as_deref(), Some("/app/projects"));
        assert!(!matters.in_fields, "a chord never fires in a field");
    }

    #[test]
    fn only_modified_enter_fires_inside_a_text_field() {
        for shortcut in global_shortcuts(&app_pages())
            .into_iter()
            .chain(questionnaire_shortcuts(true, true))
        {
            assert_eq!(
                shortcut.in_fields,
                shortcut.keys == "Mod+Enter",
                "{}",
                shortcut.keys
            );
        }
    }

    #[test]
    fn the_numbered_keys_are_offered_only_for_a_choice_question() {
        let keys = |has_choices| -> Vec<String> {
            questionnaire_shortcuts(has_choices, true)
                .into_iter()
                .map(|shortcut| shortcut.keys)
                .collect()
        };
        assert_eq!(
            keys(true),
            ["Mod+Enter", "Mod+Backspace", "Alt+ArrowLeft", "1-9"]
        );
        assert_eq!(keys(false), ["Mod+Enter", "Mod+Backspace", "Alt+ArrowLeft"]);
    }

    #[test]
    fn previous_question_keys_are_only_declared_after_the_first_question() {
        let keys = |can_go_back| {
            questionnaire_shortcuts(false, can_go_back)
                .into_iter()
                .map(|shortcut| shortcut.keys)
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(false), ["Mod+Enter"]);
        assert_eq!(keys(true), ["Mod+Enter", "Mod+Backspace", "Alt+ArrowLeft"]);
    }

    #[test]
    fn the_host_declares_exactly_the_global_registry() {
        fn app() -> Element {
            rsx! { ShortcutHost {} }
        }
        let html = ssr(app);
        let expected: Vec<serde_json::Value> = global_shortcuts(&app_pages())
            .iter()
            .map(|shortcut| serde_json::to_value(shortcut).expect("serializes"))
            .collect();
        assert_eq!(declared(&html), expected, "{html}");
        assert!(
            html.contains("data-chord-timeout-ms=\"1000\""),
            "the timeout the script reads is the one defined here: {html}"
        );
    }

    #[test]
    fn the_overlay_is_a_labelled_modal_dialog_with_a_close_control() {
        fn app() -> Element {
            rsx! { ShortcutHost {} }
        }
        let html = ssr(app);
        assert!(html.contains("<dialog"), "{html}");
        assert!(
            html.contains(&format!("aria-labelledby=\"{HELP_DIALOG_ID}-title\"")),
            "{html}"
        );
        assert!(
            html.contains(&format!("id=\"{HELP_DIALOG_ID}-title\"")),
            "the label the dialog names exists: {html}"
        );
        assert!(html.contains("Keyboard shortcuts"), "{html}");
        assert!(html.contains("aria-label=\"Close\""), "{html}");
        assert!(
            !html.contains("open=") && !html.contains(" open "),
            "closed until `?`: {html}"
        );
    }

    /// The shipped script's source — what the browser actually runs.
    fn shipped_script() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../server/public/js/keyboard-shortcuts.js");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
    }

    /// One top-level function of the shipped script, up to its closing brace —
    /// enough to assert what a single behavior does and in what order.
    fn function_body(source: &str, name: &str) -> String {
        let start = source
            .find(&format!("function {name}("))
            .unwrap_or_else(|| panic!("the script has no `{name}`"));
        let rest = &source[start..];
        let end = rest
            .find("\n  }\n")
            .unwrap_or_else(|| panic!("`{name}` never closes"));
        rest[..end].to_string()
    }

    /// The value of one attribute on the rendered HTML, with Dioxus's entity
    /// escapes read back — what the script reads, but out of the markup.
    fn attribute_value(html: &str, name: &str) -> Option<String> {
        let marker = format!("{name}=\"");
        let start = html.find(&marker)? + marker.len();
        let rest = &html[start..];
        let end = rest.find('"').expect("attribute closes");
        Some(
            rest[..end]
                .replace("&#34;", "\"")
                .replace("&quot;", "\"")
                .replace("&amp;", "&")
                .replace("&#x27;", "'"),
        )
    }

    /// The palette dialog rendered with an explicit destination list — the
    /// pure surface the host delegates to.
    fn palette_ssr(destinations: Vec<AppNavLink>) -> String {
        let mut dom =
            VirtualDom::new_with_props(PaletteDialog, PaletteDialogProps { destinations });
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    /// A closed native modal with every hook the script will read: the
    /// destinations attribute, the filter field, the listbox, the close
    /// control. No `open` attribute — nothing opens without `showModal()`.
    #[test]
    fn the_palette_dialog_is_a_closed_dialog_with_the_script_hooks() {
        let out = palette_ssr(palette_destinations(crate::people::ViewerRole::Owner));
        assert!(out.contains("<dialog"), "{out}");
        assert!(
            out.contains(&format!("id=\"{PALETTE_DIALOG_ID}\"")),
            "{out}"
        );
        assert!(
            !out.contains("open=") && !out.contains(" open "),
            "closed until the script opens it: {out}"
        );
        for hook in [
            PALETTE_DESTINATIONS_ATTRIBUTE,
            PALETTE_FILTER_ATTRIBUTE,
            PALETTE_RESULTS_ATTRIBUTE,
            PALETTE_CLOSE_ATTRIBUTE,
            PALETTE_OPTION_HREF_ATTRIBUTE,
        ] {
            assert!(out.contains(hook), "the markup never emits `{hook}`: {out}");
        }
        assert!(out.contains("role=\"listbox\""), "{out}");
        assert!(out.contains("role=\"option\""), "{out}");
        assert!(
            out.contains(r#"type="search""#),
            "the filter field is a search input: {out}"
        );
    }

    /// The destinations attribute is exactly the list the component was given,
    /// serialized — the script filters over what the server already scoped.
    #[test]
    fn the_palette_dialog_carries_the_destinations_it_was_given() {
        let destinations = vec![
            AppNavLink::new("Projects", crate::app_chrome::APP_PROJECTS_HREF),
            AppNavLink::new("Profile", crate::app_chrome::APP_PROFILE_HREF),
        ];
        let out = palette_ssr(destinations.clone());
        let carried = attribute_value(&out, PALETTE_DESTINATIONS_ATTRIBUTE)
            .expect("the destinations attribute renders");
        assert_eq!(
            carried,
            serde_json::to_string(&destinations).expect("serializes")
        );
        assert_eq!(
            out.matches(PALETTE_OPTION_HREF_ATTRIBUTE).count(),
            destinations.len(),
            "one row per destination: {out}"
        );
    }

    /// A client's palette — what the server resolved for a client-role page —
    /// advertises no firm home, no inbox, no admin door. The navbar's own
    /// boundary, carried into the palette.
    #[test]
    fn a_client_palette_lists_no_firm_or_admin_destination() {
        let out = palette_ssr(palette_destinations(crate::people::ViewerRole::Client));
        assert!(out.contains(r#"data-href="/app/projects""#), "{out}");
        assert!(out.contains(r#"data-href="/app/profile""#), "{out}");
        for refused in ["/app/team", "/app/inbox", "/app/admin"] {
            assert!(
                !out.contains(refused),
                "a client palette advertises {refused}: {out}"
            );
        }
    }

    /// An owner's palette carries the firm and admin destinations their
    /// navbar already shows, plus the clause library the row omits.
    #[test]
    fn an_owner_palette_lists_the_firm_and_admin_destinations() {
        let out = palette_ssr(palette_destinations(crate::people::ViewerRole::Owner));
        for expected in [
            crate::app_chrome::APP_PROJECTS_HREF,
            crate::app_chrome::APP_TEAM_HREF,
            crate::inbox::INBOX_PATH,
            crate::app_chrome::APP_OWNER_HREF,
            crate::clause_library::CLAUSES_PATH,
            crate::app_chrome::APP_PROFILE_HREF,
        ] {
            assert!(
                out.contains(&format!("{PALETTE_OPTION_HREF_ATTRIBUTE}=\"{expected}\"")),
                "the owner palette misses {expected}: {out}"
            );
        }
    }

    /// The host renders both dialogs: the `?` overlay the registry has always
    /// carried and the palette beside it. The host's own destinations are the
    /// server's answer for this request.
    #[test]
    fn the_host_renders_the_palette_dialog_beside_the_help_one() {
        fn app() -> Element {
            rsx! { ShortcutHost {} }
        }
        let html = ssr(app);
        assert!(
            html.contains(&format!("id=\"{HELP_DIALOG_ID}\"")),
            "the help overlay: {html}"
        );
        assert!(
            html.contains(&format!("id=\"{PALETTE_DIALOG_ID}\"")),
            "the palette beside it: {html}"
        );
        assert!(html.contains(PALETTE_FILTER_ATTRIBUTE), "{html}");
        assert!(html.contains(PALETTE_RESULTS_ATTRIBUTE), "{html}");
        // The destinations are the server's answer for this request — under
        // `--features server` the function resolves right here, defaulting to
        // the least-privileged tier. Whatever the tier, it is a JSON array.
        let carried = attribute_value(&html, PALETTE_DESTINATIONS_ATTRIBUTE)
            .expect("the destinations attribute renders");
        assert!(
            serde_json::from_str::<serde_json::Value>(&carried)
                .expect("valid JSON")
                .is_array(),
            "{carried}"
        );
    }

    #[test]
    fn page_shortcuts_are_declared_for_the_overlay_without_rendering_visibly() {
        fn app() -> Element {
            rsx! { PageShortcuts { shortcuts: questionnaire_shortcuts(true, true) } }
        }
        let html = ssr(app);
        assert!(html.contains("hidden"), "{html}");
        let rows = declared(&html);
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().any(|row| row["action"] == "back"), "{rows:?}");
        assert!(rows.iter().all(|row| row["scope"] == "page"), "{rows:?}");
    }

    /// The script the host loads is the file we ship: it reads the declarations
    /// the host emits, runs every action the registry can name, and keeps its
    /// behavior out of inline handlers and `eval`.
    #[test]
    fn the_script_handles_every_action_and_hook_the_registry_emits() {
        let source = shipped_script();
        for action in ["help", "open_palette", "navigate", "next", "choose"] {
            assert!(
                source.contains(&format!("case \"{action}\"")),
                "the script has no `{action}` action"
            );
        }
        for hook in [
            DECLARATION_ATTRIBUTE,
            HELP_DIALOG_ID,
            PALETTE_DIALOG_ID,
            PALETTE_FILTER_ATTRIBUTE,
            PALETTE_RESULTS_ATTRIBUTE,
            PALETTE_CLOSE_ATTRIBUTE,
            "data-nav-shortcuts-list",
            "data-nav-shortcuts-close",
            "data-nav-questionnaire",
            "data-chord-timeout-ms",
        ] {
            assert!(source.contains(hook), "the script never reads `{hook}`");
        }
        assert!(!source.contains("eval("), "no eval");
        assert!(
            !source.contains("innerHTML"),
            "the list is built from text nodes"
        );
        assert!(!source.contains("onclick"), "no inline handlers");
    }

    /// The palette opens the way the help overlay always has: the focus
    /// snapshot is taken before the dialog opens, the filter is focused, and
    /// a half-typed chord lapses. Closing — by Escape, the close control, or
    /// the `close` event the platform fires — hands focus back.
    #[test]
    fn the_script_opens_the_palette_and_restores_focus() {
        let source = shipped_script();
        let open = function_body(&source, "openPalette");
        assert!(
            open.contains("paletteReturnFocus = document.activeElement"),
            "the palette takes the focus snapshot: {open}"
        );
        let snapshot = open
            .find("paletteReturnFocus = document.activeElement")
            .expect("the snapshot");
        let modal = open
            .find("showModal")
            .unwrap_or_else(|| panic!("the palette never opens: {open}"));
        assert!(
            snapshot < modal,
            "the snapshot is taken before the dialog opens: {open}"
        );
        assert!(
            open.contains("PALETTE_FILTER")
                && source.contains(&format!(
                    "var PALETTE_FILTER = \"{PALETTE_FILTER_ATTRIBUTE}\""
                )),
            "the palette focuses the filter: {open}"
        );
        assert!(
            open.contains("reset()"),
            "a half-typed chord lapses: {open}"
        );
        assert!(
            open.contains("palette()"),
            "the palette opens its own dialog: {open}"
        );
        let close = function_body(&source, "closePalette");
        assert!(
            close.contains("paletteIsOpen()"),
            "closing a shut palette does nothing: {close}"
        );
        assert!(
            source.contains("event.target.id === PALETTE_ID"),
            "the platform's close event restores focus: {source}"
        );
    }

    /// Typing narrows the palette with a client-side substring match, the
    /// selection resets to the first match, and a filter that matches nothing
    /// says so instead of leaving a silently empty list.
    #[test]
    fn the_script_filters_the_palette_and_names_the_empty_result() {
        let source = shipped_script();
        assert!(
            source.contains("addEventListener(\"input\""),
            "typing reaches the filter through a delegated listener: {source}"
        );
        let filter = function_body(&source, "filterPalette");
        assert!(
            filter.contains("toLowerCase()"),
            "matching ignores case: {filter}"
        );
        assert!(
            filter.contains("hidden"),
            "rows hide; the server's set is never rebuilt: {filter}"
        );
        assert!(
            filter.contains("No matching destinations"),
            "an empty filter names itself: {filter}"
        );
        assert!(
            filter.contains("selectPaletteRow"),
            "the selection resets with the list: {filter}"
        );
        // The filter's own typing must never re-open or navigate: the input
        // listener only ever narrows.
        let listener_start = source
            .find("addEventListener(\"input\"")
            .expect("the input listener");
        let listener = &source[listener_start..listener_start + 400];
        assert!(
            listener.contains("filterPalette(target.value)"),
            "the filter listener narrows the list: {listener}"
        );
        assert!(
            !listener.contains("location.assign") && !listener.contains("openPalette"),
            "the filter listener never navigates or reopens: {listener}"
        );
    }

    /// The arrows move the selection and announce it, and Enter closes the
    /// palette before navigating to the selected destination — the same
    /// `window.location.assign` idiom every navigation in the script uses.
    #[test]
    fn the_script_moves_the_selection_and_jumps_with_enter() {
        let source = shipped_script();
        let owner = source
            .find("if (paletteIsOpen()) {")
            .expect("the palette owns the keyboard");
        let owner_body = &source[owner..];
        for (key, handler) in [
            ("ArrowDown", "movePaletteSelection(1)"),
            ("ArrowUp", "movePaletteSelection(-1)"),
        ] {
            assert!(
                owner_body.contains(&format!("event.key === \"{key}\""))
                    && owner_body.contains(handler),
                "the palette answers {key} by moving the selection: {owner_body}"
            );
        }
        let select = function_body(&source, "selectPaletteRow");
        assert!(
            select.contains("aria-selected"),
            "the selection is announced: {select}"
        );
        let choose = function_body(&source, "choosePaletteSelection");
        let close = choose
            .find("closePalette()")
            .unwrap_or_else(|| panic!("Enter closes the palette: {choose}"));
        let assign = choose
            .find("window.location.assign")
            .unwrap_or_else(|| panic!("Enter navigates: {choose}"));
        assert!(
            close < assign,
            "the palette closes before the jump: {choose}"
        );
        assert!(
            choose.contains("data-href"),
            "Enter follows the selected row's destination"
        );
    }

    /// Exactly one overlay owns the keyboard: the palette is asked first —
    /// before the help overlay and before the chord machine — so `?` does
    /// nothing while the palette is up, `/` nothing while help is up, and a
    /// bare `/` never fires inside a text field.
    #[test]
    fn the_palette_owns_the_keyboard_before_anything_else_fires() {
        let source = shipped_script();
        let listener = source
            .find("document.addEventListener(\"keydown\"")
            .expect("the delegated keydown listener");
        let within = &source[listener..];
        let ownership = within
            .find("if (paletteIsOpen()) {")
            .expect("the listener asks the palette first");
        let help = within
            .find("if (isOpen()) {")
            .expect("then the help overlay");
        let chords = source
            .find("advance(all, 0, event, editable)")
            .expect("the chord machine");
        assert!(
            ownership < help,
            "the palette is asked before the help overlay"
        );
        assert!(
            source.find("if (paletteIsOpen()) {").expect("ownership") < chords,
            "the palette is asked before the chord machine"
        );
        let owner_body = &within[ownership..ownership + 800];
        for key in ["Escape", "ArrowDown", "ArrowUp", "Enter"] {
            assert!(
                owner_body.contains(&format!("event.key === \"{key}\"")),
                "the ownership block answers {key}: {owner_body}"
            );
        }
        assert!(
            owner_body.contains("event.preventDefault()"),
            "the arrows and Enter do not also scroll or submit: {owner_body}"
        );
        // The bare-key guard the registry relies on: `/` declares
        // `in_fields: false`, and the dispatcher honors it — so typing `/`
        // inside a text field never opens the palette.
        assert!(
            source.contains("if (editable && !shortcut.in_fields) {"),
            "the dispatcher skips bare keys inside fields"
        );
    }
}
