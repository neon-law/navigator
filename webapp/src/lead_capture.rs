//! The shared public lead-capture form.
//!
//! The page copy is resolved by the publishing brand and arrives here as a
//! small wasm-safe value. The portal supplies the signed double-submit token
//! and the source path through the request-extension seam.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{CopyRun, Field, FormCard, Heading, Honeypot};

/// The effective-date identifier carried with the linked SMS policy.
pub const SMS_POLICY_VERSION: &str = "2026-09-18";

const SMS_PHONE_HELPER: &str = "Optional. Message frequency varies. Message and data rates may apply. Reply STOP to opt out or HELP for help. Our Privacy Policy and texting terms explain how we text and what we keep.";
const SMS_LABEL: &str = "Yes, {site_name} may send me text messages about this inquiry at this number, including automated texts. Texting is not a condition of hiring the firm.";

/// The copy the brand shows next to a lead form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LeadCaptureCopy {
    pub consent_sentence: String,
    pub phone_helper: String,
    pub sms_label: String,
}

impl LeadCaptureCopy {
    /// The exact SMS wording the form renders, as one evidence value.
    #[must_use]
    pub fn sms_consent_version(&self) -> String {
        format!("{}\n{}", self.phone_helper, self.sms_label)
    }
}

/// The per-request values the portal supplies to a public page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LeadCaptureContext {
    pub csrf_token: String,
    pub source_path: String,
    /// The deployment's public hCaptcha sitekey; `None` renders no challenge.
    #[serde(default)]
    pub hcaptcha_sitekey: Option<String>,
}

/// The hCaptcha widget script, loaded only by a page that renders the widget.
pub const HCAPTCHA_SCRIPT_SRC: &str = "https://js.hcaptcha.com/1/api.js";

/// The class the hCaptcha script mounts its challenge into. The portal's page
/// CSP admits the hCaptcha origins only when a rendered page carries it.
pub const HCAPTCHA_WIDGET_CLASS: &str = "h-captcha";

/// The hCaptcha challenge, placed inside a form that posts to `/leads`. The
/// solved token is posted as `h-captcha-response`.
#[component]
pub fn HCaptchaWidget(sitekey: String) -> Element {
    rsx! {
        div { class: HCAPTCHA_WIDGET_CLASS, "data-sitekey": "{sitekey}" }
        script { src: HCAPTCHA_SCRIPT_SRC, r#async: true, defer: true }
    }
}

/// Derive the current SMS evidence from server-owned copy and policy data.
///
/// The public form receives its copy from the same catalog values. This
/// server-side seam is what the submission handler uses; no requester field is
/// accepted as evidence.
#[must_use]
pub fn server_sms_consent_version(site_name: &str) -> String {
    format!(
        "{SMS_PHONE_HELPER}\n{}",
        SMS_LABEL.replace("{site_name}", site_name)
    )
}

/// Render a public lead form beside the page's ordinary mail action.
#[component]
pub fn LeadCaptureForm(
    copy: LeadCaptureCopy,
    context: LeadCaptureContext,
    #[props(default = "Send".to_string())] submit_label: String,
) -> Element {
    rsx! {
        FormCard {
            title: "Lead capture".to_string(),
            action: "/leads".to_string(),
            submit_label,
            heading: Heading::Hidden,
            csrf_token: Some(context.csrf_token),
            fields: lead_capture_fields(&copy),
            extra_fields: Some(rsx! {
                Honeypot { name: "website".to_string() }
                input { r#type: "hidden", name: "source_path", value: "{context.source_path}" }
                input { r#type: "hidden", name: "consent_version", value: "{copy.consent_sentence}" }
                if let Some(sitekey) = context.hcaptcha_sitekey.clone() {
                    HCaptchaWidget { sitekey }
                }
            }),
        }
    }
}

/// Build the contact fields shared by the standalone form and composed public
/// forms. Contact values are submitted; page-specific answers should remain
/// unnamed when they must stay in the browser.
pub(crate) fn lead_capture_fields(copy: &LeadCaptureCopy) -> Vec<Field> {
    vec![
        Field::email("Email", "email", "")
            .required()
            .autocomplete("email")
            .maxlength(254),
        Field::input("Mobile phone", "phone", "", "tel")
            .autocomplete("tel")
            .maxlength(32)
            .help_runs(linked_runs(
                &copy.phone_helper,
                "Privacy Policy and texting terms",
                "/privacy#text-messaging-sms",
            )),
        Field::checkbox(copy.sms_label.clone(), "sms_consent", "on", false).help_runs(linked_runs(
            &copy.consent_sentence,
            "Privacy Policy",
            "/privacy",
        )),
    ]
}

fn linked_runs(text: &str, token: &str, href: &str) -> Vec<CopyRun> {
    let Some((before, after)) = text.split_once(token) else {
        return vec![CopyRun {
            text: text.to_string(),
            emphasis: false,
            href: None,
        }];
    };
    let mut runs = Vec::with_capacity(3);
    if !before.is_empty() {
        runs.push(CopyRun {
            text: before.to_string(),
            emphasis: false,
            href: None,
        });
    }
    runs.push(CopyRun {
        text: token.to_string(),
        emphasis: false,
        href: Some(href.to_string()),
    });
    if !after.is_empty() {
        runs.push(CopyRun {
            text: after.to_string(),
            emphasis: false,
            href: None,
        });
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::assert_forms_accessible;

    fn render(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[test]
    fn renders_the_form_copy_links_and_double_submit_fields() {
        fn app() -> Element {
            rsx! {
                LeadCaptureForm {
                    copy: LeadCaptureCopy {
                        consent_sentence: "By sending this, you agree that Neon Law may email you about this inquiry. Sending it does not make you a client, and nothing on this page is legal advice. See our Privacy Policy.".to_string(),
                        phone_helper: "Optional. Message frequency varies. Message and data rates may apply. Reply STOP to opt out or HELP for help. Our Privacy Policy and texting terms explain how we text and what we keep.".to_string(),
                        sms_label: "Yes, Neon Law may send me text messages about this inquiry at this number, including automated texts. Texting is not a condition of hiring the firm.".to_string(),
                    },
                    context: LeadCaptureContext {
                        csrf_token: "csrf-token".to_string(),
                        source_path: "/services".to_string(),
                        hcaptcha_sitekey: None,
                    },
                }
            }
        }

        let html = render(app);
        for expected in [
            "nav-form",
            "name=\"email\"",
            "name=\"phone\"",
            "name=\"sms_consent\"",
            "name=\"website\"",
            "value=\"csrf-token\"",
            "value=\"/services\"",
            "href=\"/privacy\"",
            "href=\"/privacy#text-messaging-sms\"",
            "class=\"nav-input\"",
            "class=\"nav-checkbox\"",
            "class=\"nav-btn nav-btn--primary\"",
            ">Send<",
        ] {
            assert!(html.contains(expected), "missing {expected}: {html}");
        }
        assert!(html.contains(r#"aria-label="Lead capture""#), "{html}");
        assert!(!html.contains("sms_consent_version"), "{html}");
        assert!(!html.contains("sms_policy_version"), "{html}");
        assert_forms_accessible(&html, "lead form");
        assert!(
            !html.contains("h-captcha"),
            "no sitekey, no challenge: {html}"
        );
    }

    #[test]
    fn a_sitekey_renders_the_hcaptcha_challenge_inside_the_form() {
        fn app() -> Element {
            rsx! {
                LeadCaptureForm {
                    copy: LeadCaptureCopy::default(),
                    context: LeadCaptureContext {
                        csrf_token: "csrf-token".to_string(),
                        source_path: "/contact".to_string(),
                        hcaptcha_sitekey: Some("site-key".to_string()),
                    },
                }
            }
        }

        let html = render(app);
        let widget = html
            .find(r#"class="h-captcha" data-sitekey="site-key""#)
            .unwrap_or_else(|| panic!("widget: {html}"));
        let form_end = html
            .find("</form>")
            .unwrap_or_else(|| panic!("form: {html}"));
        assert!(
            widget < form_end,
            "the token must post with the form: {html}"
        );
        assert!(html.contains(HCAPTCHA_SCRIPT_SRC), "{html}");
    }

    /// The notice under the texting consent reads after it, full width — not
    /// as a second column beside the checkbox. The checkbox row is a flex row,
    /// so the theme must let it wrap and give the help line its own line.
    #[test]
    fn the_notice_drops_below_the_texting_consent_in_one_column() {
        let theme =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/public/css/theme.css");
        let css = std::fs::read_to_string(&theme)
            .unwrap_or_else(|e| panic!("the theme stylesheet must be readable: {e}"));
        let rule = |selector: &str| -> String {
            let at = css
                .find(&format!("{selector} {{"))
                .unwrap_or_else(|| panic!("theme.css must style `{selector}`"));
            let body = &css[at..];
            body[..body.find('}').unwrap_or(body.len())].to_string()
        };

        assert!(
            rule(".nav-field--check").contains("flex-wrap: wrap"),
            "the checkbox row must wrap so its notice sits below it"
        );
        assert!(
            rule(".nav-field--check > .nav-field__help,\n.nav-field--check > .nav-field__error")
                .contains("flex-basis: 100%"),
            "the notice must take the full row, not a column beside the label"
        );
    }
}
