//! Native entry point for the Nautilus mobile shell. The pages, and their
//! tests, live in `webapp::mobile`; this binary only launches them.

fn main() {
    dioxus::launch(webapp::mobile::MobileApp);
}
