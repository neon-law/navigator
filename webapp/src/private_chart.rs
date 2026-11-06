//! Opt-in request-local Astrology Chart Section for authorized Matter Pages.

mod browser;
mod draft;
mod state;

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

type PageState =
    state::ChartState<draft::Draft, crate::astrology_chart::calculated::CalculatedChart>;

/// Server-resolved eligibility only. Birth inputs and results never cross the SSR seam.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChartSectionView {
    #[default]
    Unavailable,
    ReadOnly,
    Available {
        project_id: Uuid,
        csrf_token: String,
    },
}

/// Missing request context is different from an explicitly ordinary session.
#[cfg(feature = "server")]
pub(crate) fn section_view(
    project_id: Uuid,
    lens: crate::sections::PageLens,
    csrf: Option<&str>,
    read_only: Option<bool>,
) -> ChartSectionView {
    if read_only == Some(true) {
        ChartSectionView::ReadOnly
    } else if read_only == Some(false) && lens != crate::sections::PageLens::Clerk {
        match csrf.filter(|token| !token.is_empty()) {
            Some(token) => ChartSectionView::Available {
                project_id,
                csrf_token: token.to_owned(),
            },
            None => ChartSectionView::Unavailable,
        }
    } else {
        ChartSectionView::Unavailable
    }
}

impl std::fmt::Debug for ChartSectionView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("Unavailable"),
            Self::ReadOnly => formatter.write_str("ReadOnly"),
            Self::Available { project_id, .. } => formatter
                .debug_struct("Available")
                .field("project_id", project_id)
                .field("csrf_token", &"[redacted]")
                .finish(),
        }
    }
}

#[component]
pub fn PrivateChartSection(view: ChartSectionView) -> Element {
    match view {
        ChartSectionView::Available {
            project_id,
            csrf_token,
        } if !csrf_token.is_empty() => rsx! {
            PrivateChartForm { key: "{project_id}", project_id, csrf_token }
        },
        ChartSectionView::ReadOnly => rsx! {
            section {
                class: "private-chart matter-section",
                "data-section": "astrology_chart",
                crate::components::Panel {
                    id: "section-astrology_chart".to_owned(),
                    title: "Astrology chart".to_owned(),
                    p {
                        "This is a read-only view. Personal inputs and calculation controls are unavailable."
                    }
                }
            }
        },
        _ => rsx! {
            section {
                class: "private-chart matter-section",
                "data-section": "astrology_chart",
                crate::components::Panel {
                    id: "section-astrology_chart".to_owned(),
                    title: "Astrology chart".to_owned(),
                    p { class: "matter-section__empty",
                        "Private calculation is unavailable in this view."
                    }
                }
            }
        },
    }
}

#[component]
fn PrivateChartForm(project_id: Uuid, csrf_token: String) -> Element {
    let mut state = use_signal(move || PageState::new(project_id));
    let ready = use_signal(|| false);
    let runtime = use_hook(browser::Runtime::default);
    let cleanup = runtime.clone();
    use_drop(move || cleanup.dispose());
    let mounted = runtime.clone();
    let submit = runtime.clone();
    let submit_csrf = csrf_token.clone();
    let clear = runtime.clone();
    let edit_runtime = runtime.clone();
    let edit = EventHandler::new(move |(field, value): (Input, String)| {
        edit_runtime.cancel();
        state.write().edit(|draft| field.set(draft, value));
    });
    let toggle_runtime = runtime.clone();
    let toggle = EventHandler::new(move |(body, selected): (String, bool)| {
        toggle_runtime.cancel();
        state.write().edit(|draft| {
            draft.bodies.retain(|name| name != &body);
            if selected {
                draft.bodies.push(body);
            }
        });
    });
    let consent_runtime = runtime.clone();
    let consent = EventHandler::new(move |checked: bool| {
        consent_runtime.cancel();
        state.write().edit(|draft| draft.consent = checked);
    });
    let (draft, chart, status) = {
        let current = state.read();
        (current.draft.clone(), current.chart.clone(), current.status)
    };
    let disabled = !ready() || status == state::Status::Calculating;
    let action = format!("/app/api/projects/{project_id}/astrology-chart");
    rsx! {
        section {
            class: "private-chart matter-section",
            "data-section": "astrology_chart",
            "data-private-chart": "true",
            onmounted: move |event| mounted.mounted(&event, state, ready),
            crate::components::Panel {
                id: "section-astrology_chart".to_owned(),
                title: "Astrology chart".to_owned(),
                p {
                    "Optional personal reflection. Inputs and results belong only to this page; they are not saved as a chart or added to chart history."
                }
                noscript {
                    p { "Enable JavaScript to calculate a private chart." }
                }
                form {
                    method: "post",
                    action,
                    autocomplete: "off",
                    aria_label: "Private astrology chart calculation",
                    onsubmit: move |event| {
                        event.prevent_default();
                        if !ready() || state.read().status == state::Status::Calculating {
                            return;
                        }
                        let payload = state.read().draft.payload();
                        match payload {
                            Ok(payload) => {
                                let ticket = state.write().begin();
                                submit.submit(state, ticket, payload, submit_csrf.clone(), project_id);
                            }
                            Err(message) => state.write().status = state::Status::Refused(message),
                        }
                    },
                    input {
                        r#type: "hidden",
                        name: "_csrf",
                        value: "{csrf_token}",
                    }
                    fieldset { disabled, class: "private-chart__fields",
                        legend { "Birth inputs" }
                        label { class: "nav-field",
                            span { class: "nav-label", "Clock precision" }
                            select {
                                class: "nav-input",
                                name: "precision",
                                value: "{draft.precision}",
                                "data-private-input": "true",
                                oninput: move |event| edit.call((Input::Precision, event.value())),
                                option { value: "exact", "Exact" }
                                option { value: "approximate", "Approximate" }
                                option { value: "unknown", "Unknown" }
                            }
                        }
                        PrivateInput {
                            label: "Birth date",
                            name: "birth-date",
                            kind: "date",
                            value: draft.date,
                            min: "1900-01-01",
                            max: "2049-12-31",
                            oninput: move |value| edit.call((Input::Date, value)),
                        }
                        if draft.precision != "unknown" {
                            PrivateInput {
                                label: "Local birth time",
                                name: "birth-time",
                                kind: "time",
                                value: draft.time,
                                oninput: move |value| edit.call((Input::Time, value)),
                            }
                            PrivateInput {
                                label: "Explicit UTC offset in seconds (if clock repeats)",
                                name: "birth-offset",
                                kind: "number",
                                value: draft.offset_seconds,
                                required: false,
                                min: "-86400",
                                max: "86400",
                                step: "1",
                                oninput: move |value| edit.call((Input::Offset, value)),
                            }
                        }
                        if draft.precision == "approximate" {
                            PrivateInput {
                                label: "Clock uncertainty in seconds",
                                name: "birth-uncertainty",
                                kind: "number",
                                value: draft.uncertainty_seconds,
                                min: "1",
                                max: "4294967295",
                                step: "1",
                                oninput: move |value| edit.call((Input::Uncertainty, value)),
                            }
                        }
                        PrivateInput {
                            label: "IANA time zone",
                            name: "birth-zone",
                            kind: "text",
                            value: draft.zone,
                            oninput: move |value| edit.call((Input::Zone, value)),
                        }
                        PrivateInput {
                            label: "Longitude (east-positive degrees)",
                            name: "birth-longitude",
                            kind: "number",
                            value: draft.longitude,
                            min: "-180",
                            max: "180",
                            oninput: move |value| edit.call((Input::Longitude, value)),
                        }
                        PrivateInput {
                            label: "Latitude (north-positive degrees)",
                            name: "birth-latitude",
                            kind: "number",
                            value: draft.latitude,
                            min: "-90",
                            max: "90",
                            oninput: move |value| edit.call((Input::Latitude, value)),
                        }
                        label { class: "nav-field",
                            span { class: "nav-label", "House convention" }
                            select {
                                class: "nav-input",
                                name: "houses",
                                value: "{draft.houses}",
                                disabled: draft.precision != "exact",
                                "data-private-input": "true",
                                oninput: move |event| edit.call((Input::Houses, event.value())),
                                option { value: "none", "No houses" }
                                option { value: "equal", "Equal" }
                                option { value: "whole_sign", "Whole sign" }
                            }
                        }
                        p { class: "nav-help",
                            "Only an exact clock supports houses. The validated lunar node is the Mean North Node; true node, sidereal charts, Placidus and composite charts are unavailable."
                        }
                        fieldset { class: "private-chart__bodies",
                            legend { "Selected bodies" }
                            for body in ephemeris::lines::LINE_BODIES {
                                label { class: "chart-toggle",
                                    input {
                                        class: "nav-checkbox",
                                        r#type: "checkbox",
                                        name: "bodies",
                                        value: body.name(),
                                        checked: draft.bodies.iter().any(|name| name == body.name()),
                                        "data-private-input": "true",
                                        oninput: move |event| toggle.call((body.name().to_owned(), event.checked())),
                                    }
                                    if body == ephemeris::lines::LineBody::MeanNode {
                                        "Mean North Node"
                                    } else {
                                        "{crate::ephemeris_labels::body_display_name(body)}"
                                    }
                                }
                            }
                        }
                        label { class: "chart-toggle",
                            input {
                                class: "nav-checkbox",
                                r#type: "checkbox",
                                name: "consent",
                                checked: draft.consent,
                                required: true,
                                "data-private-input": "true",
                                oninput: move |event| consent.call(event.checked()),
                            }
                            "I consent to using these inputs for this calculation and personal reflection."
                        }
                        button {
                            class: "nav-btn nav-btn--primary",
                            r#type: "submit",
                            "Calculate chart"
                        }
                    }
                    button {
                        class: "nav-btn",
                        r#type: "button",
                        "data-private-clear": "true",
                        onclick: move |_| {
                            clear.cancel();
                            state.write().clear();
                        },
                        "Clear inputs and result"
                    }
                }
                match status {
                    state::Status::Calculating => rsx! {
                        p { role: "status", "Calculating…" }
                    },
                    state::Status::Complete => rsx! {
                        p { role: "status", "Calculation complete." }
                    },
                    state::Status::Refused(message) => rsx! {
                        p { role: "alert", "{message}" }
                    },
                    state::Status::Idle => rsx! {
                        p { "Clear or leave this page to discard its inputs and result." }
                    },
                }
                if let Some(chart) = chart {
                    div { "data-private-result": "true",
                        crate::astrology_chart::calculated::CalculatedAstrologyChart { chart }
                    }
                }
            }
        }
    }
}

#[component]
fn PrivateInput(
    label: &'static str,
    name: &'static str,
    kind: &'static str,
    value: String,
    oninput: EventHandler<String>,
    #[props(default = true)] required: bool,
    #[props(default = "any")] step: &'static str,
    #[props(default)] min: Option<&'static str>,
    #[props(default)] max: Option<&'static str>,
) -> Element {
    rsx! {
        label { class: "nav-field",
            span { class: "nav-label", "{label}" }
            input {
                class: "nav-input",
                r#type: kind,
                name,
                value,
                required,
                min,
                max,
                step,
                autocomplete: "off",
                maxlength: "128",
                "data-private-input": "true",
                oninput: move |event| oninput.call(event.value()),
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Input {
    Precision,
    Date,
    Time,
    Zone,
    Offset,
    Uncertainty,
    Longitude,
    Latitude,
    Houses,
}
impl Input {
    fn set(self, draft: &mut draft::Draft, value: String) {
        match self {
            Self::Precision => {
                draft.precision = value;
                draft.houses = "none".into();
                if draft.precision == "unknown" {
                    draft.time.clear();
                    draft.offset_seconds.clear();
                }
                if draft.precision != "approximate" {
                    draft.uncertainty_seconds.clear();
                }
            }
            Self::Date => draft.date = value,
            Self::Time => draft.time = value,
            Self::Zone => draft.zone = value,
            Self::Offset => draft.offset_seconds = value,
            Self::Uncertainty => draft.uncertainty_seconds = value,
            Self::Longitude => draft.longitude = value,
            Self::Latitude => draft.latitude = value,
            Self::Houses => draft.houses = value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available() -> ChartSectionView {
        ChartSectionView::Available {
            project_id: Uuid::nil(),
            csrf_token: "synthetic-private-session-token".into(),
        }
    }

    fn render(view: ChartSectionView) -> String {
        let mut dom =
            VirtualDom::new_with_props(PrivateChartSection, PrivateChartSectionProps { view });
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[cfg(feature = "server")]
    #[test]
    fn calculation_controls_require_explicit_writable_request_context() {
        use crate::sections::PageLens;
        for lens in [PageLens::Client, PageLens::Lawyer] {
            assert!(matches!(
                section_view(Uuid::nil(), lens, Some("token"), Some(false)),
                ChartSectionView::Available { .. }
            ));
            for csrf in [None, Some("")] {
                assert_eq!(
                    section_view(Uuid::nil(), lens, csrf, Some(false)),
                    ChartSectionView::Unavailable
                );
            }
            assert_eq!(
                section_view(Uuid::nil(), lens, Some("token"), None),
                ChartSectionView::Unavailable
            );
            assert_eq!(
                section_view(Uuid::nil(), lens, Some("token"), Some(true)),
                ChartSectionView::ReadOnly
            );
        }
        assert_eq!(
            section_view(Uuid::nil(), PageLens::Clerk, Some("token"), Some(false)),
            ChartSectionView::Unavailable
        );
        assert!(
            matches!(
                crate::sections::LoadContext::empty(PageLens::Lawyer).private_chart,
                ChartSectionView::Unavailable
            ),
            "offline preview never receives write eligibility"
        );
    }

    #[test]
    fn eligibility_debug_redacts_the_session_credential() {
        let text = format!("{:?}", available());
        assert!(
            !text.contains("synthetic-private-session-token"),
            "debug formatting cannot disclose a session credential"
        );
        assert!(
            text.contains(&Uuid::nil().to_string()),
            "Project metadata remains useful"
        );
    }

    #[test]
    fn authorized_ssr_has_only_a_blank_disabled_post_form() {
        let html = render(available());
        crate::components::form::assert_forms_accessible(&html, "private chart");
        assert!(
            html.contains("method=\"post\""),
            "a degraded form must never default to GET"
        );
        assert!(
            html.contains("/app/api/projects/00000000-0000-0000-0000-000000000000/astrology-chart")
        );
        assert!(
            html.contains("disabled"),
            "personal inputs stay disabled before hydration"
        );
        assert!(html.contains("name=\"_csrf\""));
        assert!(html.contains("personal reflection"));
        assert!(
            !html.contains("<svg"),
            "SSR carries no private calculated result"
        );
        let value = serde_json::to_string(&available()).unwrap();
        for private_field in [
            "birth_time",
            "positions",
            "metadata",
            "latitude_deg",
            "longitude_deg",
        ] {
            assert!(
                !value.contains(private_field),
                "hydration carries eligibility only"
            );
        }
    }

    #[test]
    fn readonly_and_missing_eligibility_have_no_personal_input_controls() {
        for view in [ChartSectionView::ReadOnly, ChartSectionView::Unavailable] {
            let html = render(view);
            assert!(!html.contains("<form") && !html.contains("<input"));
        }
        assert!(render(ChartSectionView::ReadOnly).contains("read-only"));
    }
}
