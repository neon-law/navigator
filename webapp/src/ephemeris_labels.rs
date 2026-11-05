//! Shared labels for the validated finite ephemeris body inventory.

use ephemeris::lines::{LineBody, LINE_BODIES};

pub(crate) fn line_body_named(name: &str) -> Option<LineBody> {
    LINE_BODIES.iter().copied().find(|body| body.name() == name)
}

pub(crate) fn body_symbol(body: LineBody) -> &'static str {
    match body {
        LineBody::Sun => "☉",
        LineBody::Moon => "☽",
        LineBody::Mercury => "☿",
        LineBody::Venus => "♀",
        LineBody::Mars => "♂",
        LineBody::Jupiter => "♃",
        LineBody::Saturn => "♄",
        LineBody::Uranus => "♅",
        LineBody::Neptune => "♆",
        LineBody::Pluto => "♇",
        LineBody::Chiron => "⚷",
        LineBody::MeanNode => "☊",
    }
}

pub(crate) fn body_display_name(body: LineBody) -> &'static str {
    match body {
        LineBody::Sun => "Sun",
        LineBody::Moon => "Moon",
        LineBody::Mercury => "Mercury",
        LineBody::Venus => "Venus",
        LineBody::Mars => "Mars",
        LineBody::Jupiter => "Jupiter",
        LineBody::Saturn => "Saturn",
        LineBody::Uranus => "Uranus",
        LineBody::Neptune => "Neptune",
        LineBody::Pluto => "Pluto",
        LineBody::Chiron => "Chiron",
        LineBody::MeanNode => "North Node",
    }
}
