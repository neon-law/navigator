//! Web entry point for the Same Day Legal mobile shell. The pages, and their
//! tests, live in `webapp::mobile`; this binary only launches them for the
//! local `hudson same-day preview` server.

fn main() {
    dioxus::launch(webapp::mobile::MobileApp);
}
