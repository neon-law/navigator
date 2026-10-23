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

/// The global shortcuts for the pages a portal has: the `?` overlay, then a
/// chord for each page in `pages`. A page left out has no chord, so it is
/// neither fired nor listed.
#[must_use]
pub fn global_shortcuts(pages: &[(Page, &str)]) -> Vec<Shortcut> {
    let mut shortcuts = vec![Shortcut::new(
        "?",
        "Show keyboard shortcuts",
        Scope::Global,
        Action::Help,
    )];
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

/// The global shortcuts and the `?` overlay's dialog, once per page.
///
/// The dialog is a native modal `<dialog>`: the platform supplies the focus
/// trap, the inert background and `Esc`. Its title labels it for assistive
/// technology. The script fills the list from every declaration on the page.
#[component]
pub fn ShortcutHost() -> Element {
    let json = declarations_json(&global_shortcuts(&app_pages()));
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(keys, ["?", "g m"], "only matters has a top-level route");
        let none = global_shortcuts(&[]);
        assert_eq!(none.len(), 1, "the overlay key is always there");
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
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../server/public/js/keyboard-shortcuts.js");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        for action in ["help", "navigate", "next", "choose"] {
            assert!(
                source.contains(&format!("case \"{action}\"")),
                "the script has no `{action}` action"
            );
        }
        for hook in [
            DECLARATION_ATTRIBUTE,
            HELP_DIALOG_ID,
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
}
