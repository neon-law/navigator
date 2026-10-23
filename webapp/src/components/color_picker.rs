//! A native hex input with an unnamed picker; only the hex value is submitted.
use dioxus::prelude::*;

/// Keep the text field usable before hydration and under the same-origin CSP.
#[component]
pub fn ColorPicker(
    id: String,
    name: String,
    value: String,
    required: bool,
    described_by: Option<String>,
    invalid: Option<String>,
) -> Element {
    let swatch = if value.len() == 7
        && value.starts_with('#')
        && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
    {
        Some(value.clone())
    } else {
        None
    };
    rsx! {
        document::Script { src: "/public/js/color-picker.js", defer: true }
        div { class: "nav-color-picker", "data-color-picker": "",
            input {
                class: "nav-input", r#type: "text", id: "{id}", name,
                value, required, placeholder: "#rrggbb",
                "aria-describedby": described_by, "aria-invalid": invalid,
            }
            input {
                class: "nav-color-picker__swatch", r#type: "color", value: swatch,
                "aria-label": "Choose colour",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_hex_field_submits_and_invalid_text_is_preserved() {
        let html = dioxus_ssr::render_element(rsx! {
            ColorPicker {
                id: "primary", name: "primary_color", value: "invalid",
                required: true, described_by: Some("primary-help".into()),
                invalid: Some("true".into()),
            }
        });
        assert_eq!(html.matches("name=\"primary_color\"").count(), 1);
        assert!(html.contains("value=\"invalid\""));
        assert!(html.contains("type=\"color\""));
        assert!(html.contains("aria-describedby=\"primary-help\""));
        assert!(html.contains("aria-invalid=\"true\""));
    }
}
