//! Browser-local incident guidance with a separate contact submission.
//! Incident answers are not submitted and do not value a claim.

use super::CyberInjuryContent;
use crate::components::Honeypot;
use crate::lead_capture::{
    lead_capture_fields, HCaptchaWidget, LeadCaptureContext, LeadCaptureCopy,
};
use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Answers {
    incident: usize,
    deadline_check: bool,
    records_saved: bool,
}

fn answers(incident: &str, timing: &str, records: &str) -> Option<Answers> {
    let incident = match incident {
        "collision" => 0,
        "medical" => 1,
        "data" => 2,
        "stalking" => 3,
        _ => return None,
    };
    let deadline_check = match timing {
        "recent" | "months" => false,
        "older" | "unsure" => true,
        _ => return None,
    };
    let records_saved = match records {
        "yes" => true,
        "no" => false,
        _ => return None,
    };
    Some(Answers {
        incident,
        deadline_check,
        records_saved,
    })
}

#[component]
pub(super) fn Assessment(
    copy: CyberInjuryContent,
    consultation_href: String,
    lead_capture: Option<LeadCaptureCopy>,
    lead_context: LeadCaptureContext,
) -> Element {
    let mut incident = use_signal(String::new);
    let mut timing = use_signal(String::new);
    let mut records = use_signal(String::new);
    let mut result = use_signal(|| None::<Answers>);
    let mut invalid = use_signal(|| false);
    rsx! {
        form {
            class: "cyber-assessment-card",
            action: "/leads",
            method: "post",
            "aria-label": "{copy.form_title}",
            if lead_capture.is_some() {
                input { r#type: "hidden", name: "_csrf", value: "{lead_context.csrf_token}" }
                input { r#type: "hidden", name: "source_path", value: "{lead_context.source_path}" }
                if let Some(contact_copy) = lead_capture.as_ref() {
                    input { r#type: "hidden", name: "consent_version", value: "{contact_copy.consent_sentence}" }
                }
                Honeypot { name: "website".to_string() }
            }
            div { class: "cyber-form-title", h3 { "{copy.form_title}" } span { "{copy.form_duration}" } }
            label { r#for: "cyber-incident", "{copy.incident_label}" }
            select { class: "nav-select", id: "cyber-incident", required: true, value: "{incident}", oninput: move |event| { incident.set(event.value()); result.set(None); },
                option { value: "", disabled: true, selected: incident().is_empty(), "{copy.incident_placeholder}" }
                option { value: "collision", "{copy.incident_options[0]}" }
                option { value: "medical", "{copy.incident_options[1]}" }
                option { value: "data", "{copy.incident_options[2]}" }
                option { value: "stalking", "{copy.incident_options[3]}" }
            }
            label { r#for: "cyber-timing", "{copy.timing_label}" }
            select { class: "nav-select", id: "cyber-timing", required: true, value: "{timing}", oninput: move |event| { timing.set(event.value()); result.set(None); },
                option { value: "", disabled: true, selected: timing().is_empty(), "{copy.timing_placeholder}" }
                option { value: "recent", "{copy.timing_options[0]}" }
                option { value: "months", "{copy.timing_options[1]}" }
                option { value: "older", "{copy.timing_options[2]}" }
                option { value: "unsure", "{copy.timing_options[3]}" }
            }
            fieldset {
                legend { "{copy.care_label}" }
                for (index, value) in ["yes", "no"].into_iter().enumerate() {
                    label { class: "cyber-radio",
                        input { class: "nav-radio__input", r#type: "radio", value, checked: records() == value, onchange: move |_| { records.set(value.to_string()); result.set(None); } }
                        "{copy.care_options[index]}"
                    }
                }
            }
            button { class: "nav-btn nav-btn--primary cyber-button", r#type: "button", onclick: move |_| { let next = answers(&incident(), &timing(), &records()); invalid.set(next.is_none()); result.set(next); }, "{copy.submit_label}" span { "aria-hidden": "true", "↗" } }
            if invalid() { p { role: "alert", "{copy.validation_error}" } }
            p { class: "cyber-fine", "{copy.assessment_note}" }
            div { "aria-live": "polite", "aria-atomic": "true",
                if let Some(answer) = result() {
                    section { class: "cyber-result", "aria-labelledby": "cyber-result-title",
                        span { class: "cyber-eyebrow", "{copy.result_eyebrow}" }
                        h3 { id: "cyber-result-title", if answer.deadline_check { "{copy.deadline_heading}" } else { "{copy.recent_heading}" } }
                        p { if answer.deadline_check { "{copy.deadline_result}" } else { "{copy.recent_result}" } }
                        ul {
                            li { "{copy.evidence_tips[answer.incident]}" }
                            li { if answer.records_saved { "{copy.record_tips[0]}" } else { "{copy.record_tips[1]}" } }
                            li { "{copy.deadline_tip}" }
                        }
                        p { class: "cyber-fine", "{copy.result_note}" }
                        a { class: "cyber-button", href: consultation_href, "{copy.consultation_label}" span { "aria-hidden": "true", "↗" } }
                    }
                }
            }
            if let Some(contact_copy) = lead_capture {
                div { class: "cyber-assessment-contact",
                    h3 { "Get in touch" }
                    for field in lead_capture_fields(&contact_copy).iter() {
                        {field.render()}
                    }
                    if let Some(sitekey) = lead_context.hcaptcha_sitekey.clone() {
                        HCaptchaWidget { sitekey }
                    }
                    button { class: "nav-btn nav-btn--primary cyber-button cyber-send-button", r#type: "submit", "Send" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_or_unknown_answers_never_produce_an_assessment() {
        for (incident, timing, records) in [
            ("", "recent", "yes"),
            ("collision", "", "yes"),
            ("collision", "recent", ""),
            ("invalid", "older", "no"),
        ] {
            assert!(answers(incident, timing, records).is_none());
        }
    }

    #[test]
    fn older_and_unknown_dates_request_a_deadline_check() {
        for timing in ["older", "unsure"] {
            let answer = answers("stalking", timing, "no").unwrap();
            assert!(answer.deadline_check);
            assert!(!answer.records_saved);
            assert_eq!(answer.incident, 3);
        }
        for incident in ["collision", "medical", "data", "stalking"] {
            assert!(answers(incident, "recent", "yes").is_some());
        }
        assert!(
            !answers("collision", "recent", "yes")
                .unwrap()
                .deadline_check
        );
    }
}
