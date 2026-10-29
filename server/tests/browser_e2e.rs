#![allow(clippy::doc_markdown)]
//! Browser-driven end-to-end test against a live KIND cluster.
//!
//! The whole workspace is Rust only, and the WebDriver protocol
//! gets us a real Chromium session against `localhost:8080`.
//!
//! ## Prerequisites
//!
//! 1. KIND cluster up and seeded:
//!    `cargo run --release -p cli -- dev deploy` (see `AGENTS.md`, Local KIND development).
//! 2. Lawyer has the `lawyer` role granted in the store (see `AGENTS.md`, Authentication and lawyer access).
//! 3. `chromedriver` (or `geckodriver`) running on
//!    `http://localhost:9515`:
//!
//!    ```sh
//!    chromedriver --port=9515
//!    ```
//!
//! ## Run
//!
//! These tests are not `#[ignore]`'d: each probes for the harness
//! ([`new_client_or_skip`]) and skips cleanly when chromedriver or the
//! web server isn't reachable, so a plain `cargo test` (and CI without
//! the harness) stays green. With the harness up they run automatically:
//!
//! ```sh
//! cargo test -p server --test browser_e2e -- --test-threads=1
//! ```
//!
//! `NAV_BASE_URL` overrides the target (default `http://localhost:8080`);
//! `WEBDRIVER_URL` overrides the chromedriver location.

use std::env;
use std::time::Duration;

use anyhow::Context;
use fantoccini::key::Key;
use fantoccini::{Client, Locator};
use features::portfolio_acceptance::{self, Lens};
use features::webdriver::{
    base_url, click_and_reach, login_as_admin, login_as_clerk, login_as_client, login_as_lawyer,
    login_as_owner, new_client_or_skip, require_harness, scroll_and_js_click, wait_for_path,
    wait_for_text, wait_for_text_reloading,
};
use uuid::Uuid;

#[path = "common/practice_preview.rs"]
mod practice_preview;

/// Nautilus collects contact details before sign-in and keeps them when going Back.
#[tokio::test]
async fn nautilus_intake_preserves_answers_and_reviews_before_sign_in() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    c.goto(&format!("{}/nautilus#enrollment", base_url()))
        .await
        .expect("Nautilus");
    wait_for_text(&c, "Your contact details", Duration::from_secs(20)).await;
    let email = c
        .find(Locator::Css("[data-nautilus-intake] [name=email]"))
        .await
        .expect("email");
    email
        .send_keys("visitor@example.com")
        .await
        .expect("synthetic email");
    c.find(Locator::XPath("//button[normalize-space()='Continue']"))
        .await
        .expect("continue")
        .click()
        .await
        .expect("contact choices");
    wait_for_text(&c, "Your contact choices", Duration::from_secs(5)).await;
    c.find(Locator::XPath("//button[normalize-space()='Continue']"))
        .await
        .expect("continue")
        .click()
        .await
        .expect("review");
    wait_for_text(&c, "Review and continue", Duration::from_secs(5)).await;
    let review = c
        .find(Locator::Css("[data-nautilus-intake] dl"))
        .await
        .expect("review details")
        .text()
        .await
        .expect("review text");
    assert!(review.contains("visitor@example.com"));
    for _ in 0..2 {
        c.find(Locator::XPath("//button[normalize-space()='Back']"))
            .await
            .expect("back")
            .click()
            .await
            .expect("previous step");
    }
    let preserved = c
        .execute(
            "return document.querySelector('[data-nautilus-intake] [name=email]').value",
            vec![],
        )
        .await
        .expect("preserved email");
    assert_eq!(preserved, serde_json::json!("visitor@example.com"));
    for width in [390, 1280] {
        set_practice_viewport(&c, width, 844).await;
        let fits = c
            .execute(
                "return document.documentElement.scrollWidth <= window.innerWidth",
                vec![],
            )
            .await
            .expect("responsive page");
        assert_eq!(
            fits,
            serde_json::json!(true),
            "Nautilus overflows at {width}px"
        );
    }
    c.close().await.expect("close browser");
}

/// A verified fixture session reaches the configured booking handoff.
#[tokio::test]
async fn nautilus_signed_in_view_offers_enrollment_booking() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;
    c.goto(&format!("{}/nautilus#enrollment", base_url()))
        .await
        .expect("Nautilus booking");
    wait_for_text(&c, "You are signed in.", Duration::from_secs(20)).await;
    let booking = c
        .find(Locator::Css("#enrollment a.nav-btn"))
        .await
        .expect("booking action");
    assert_eq!(
        booking.text().await.expect("booking label"),
        "Book your enrollment appointment"
    );
    assert!(booking
        .attr("href")
        .await
        .expect("booking address")
        .expect("href")
        .starts_with("https://"));
    assert!(c.find(Locator::Css("#enrollment form")).await.is_err());
    std::fs::create_dir_all("/tmp/navigator-screenshots").expect("capture directory");
    std::fs::write(
        "/tmp/navigator-screenshots/nautilus-booking.png",
        c.screenshot().await.expect("booking capture"),
    )
    .expect("save booking capture");
    c.close().await.expect("close browser");
}

/// Markdown diagrams render while malformed source remains readable.
#[tokio::test]
async fn markdown_mermaid_viewer_renders_and_preserves_source_on_failure() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    c.goto(&format!("{}/glossary", base_url()))
        .await
        .expect("Markdown glossary page");
    let rendered = c
        .execute_async(
            r"const done = arguments[arguments.length - 1];
               const until = Date.now() + 20000;
               function check() {
                 const frames = Array.from(document.querySelectorAll('.nav-mermaid iframe'));
                 if (frames.some(frame => frame.style.visibility === 'visible')) return done(true);
                 if (Date.now() > until) return done(false);
                 setTimeout(check, 50);
               }
               check();",
            vec![],
        )
        .await
        .expect("wait for a Mermaid diagram");
    assert_eq!(rendered, serde_json::json!(true));
    let fallback = c
        .execute_async(
            r"const done = arguments[arguments.length - 1];
               const pre = document.createElement('pre');
               const code = document.createElement('code');
               code.className = 'language-mermaid';
               code.textContent = 'not a valid diagram';
               pre.append(code);
               document.body.append(pre);
               const until = Date.now() + 20000;
               function check() {
                 const figure = pre.closest('.nav-mermaid');
                 if (figure && figure.querySelector('[role=status]').textContent.includes('unavailable')) {
                   return done(figure.querySelector('details').open &&
                     code.textContent === 'not a valid diagram' &&
                     figure.querySelector('iframe').getAttribute('sandbox') === 'allow-scripts');
                 }
                 if (Date.now() > until) return done(false);
                 setTimeout(check, 50);
               }
               check();",
            vec![],
        )
        .await
        .expect("malformed diagram keeps its source");
    assert_eq!(fallback, serde_json::json!(true));
    c.close().await.expect("close browser");
}

/// Prepared brands render and navigate at phone and desktop sizes before launch.
#[tokio::test]
async fn prepared_practice_brands_have_responsive_pages_and_a_working_mobile_menu() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    let preview = practice_preview::PracticePreview::start().await;
    let capture_dir = std::path::Path::new("/tmp/navigator-screenshots");
    std::fs::create_dir_all(capture_dir).expect("capture directory");
    for (key, origin) in &preview.sites {
        for (width, height) in [(320, 700), (390, 844), (1280, 900)] {
            if width == 320 && *key != views::brand::BrandKey::Vesta {
                continue;
            }
            set_practice_viewport(&c, width, height).await;
            let paths = std::iter::once("/")
                .chain(std::iter::once("/services"))
                .chain(key.explainer_pages().iter().map(|(path, _)| *path));
            for path in paths {
                c.goto(&format!("{origin}{path}"))
                    .await
                    .expect("practice page");
                let mark = key
                    .resolve_branding(&views::brand::DEFAULT_BRANDING)
                    .firm
                    .site_name;
                wait_for_text(&c, mark, Duration::from_secs(10)).await;
                let widths = c.execute("return [document.documentElement.scrollWidth, document.documentElement.clientWidth];", vec![])
                    .await.expect("page widths");
                let widths = widths.as_array().expect("width pair");
                assert!(
                    widths[1].as_u64().expect("viewport width") <= u64::from(width),
                    "requested viewport applied"
                );
                assert!(
                    widths[0].as_u64() <= widths[1].as_u64(),
                    "{key:?} {path} overflows at {width}px"
                );
                if *key == views::brand::BrandKey::Vesta && path == "/" {
                    assert_video_rail(&c, width, capture_dir).await;
                    let layout = c.execute(
                        "const p=document.querySelector('.vesta-plan__term'); const r=p.getBoundingClientRect(); const a=p.parentElement.getBoundingClientRect(); return {fits:r.left>=a.left&&r.right<=a.right, priced:document.querySelector('.vesta-plan__price')?.textContent.trim()==='From $1,111', columns:getComputedStyle(document.querySelector('.vesta-hero')).gridTemplateColumns.split(' ').length};",
                        vec![],
                    ).await.expect("Vesta plan layout");
                    assert_eq!(layout["fits"], true, "plan fits its column at {width}px");
                    assert_eq!(
                        layout["priced"], true,
                        "the basic plan publishes its approved starting price"
                    );
                    assert_eq!(layout["columns"], if width < 768 { 1 } else { 2 });
                    c.execute(
                        "document.querySelector('#estate-plan').scrollIntoView({block:'start'});",
                        vec![],
                    )
                    .await
                    .expect("show Vesta pricing");
                    std::fs::write(
                        capture_dir.join(format!("vesta-pricing-{width}.png")),
                        c.screenshot().await.expect("Vesta capture"),
                    )
                    .expect("save Vesta capture");
                }
                if width == 390 && path == "/" && *key != views::brand::BrandKey::Vesta {
                    std::fs::write(
                        capture_dir.join(format!("{}-mobile-home.png", key.as_str())),
                        c.screenshot().await.expect("home capture"),
                    )
                    .expect("save home capture");
                    c.find(Locator::Css(".site-header__burger"))
                        .await
                        .expect("mobile menu")
                        .click()
                        .await
                        .expect("open menu");
                    let contact = c
                        .find(Locator::Css(
                            ".site-header__links a[href^='https://calendar.notion.so/']",
                        ))
                        .await
                        .expect("calendar contact link");
                    assert!(contact.is_displayed().await.expect("contact visibility"));
                    std::fs::write(
                        capture_dir.join(format!("{}-mobile-menu.png", key.as_str())),
                        c.screenshot().await.expect("menu capture"),
                    )
                    .expect("save menu capture");
                }
            }
        }
    }
    c.close().await.expect("close browser");
}

async fn assert_video_rail(c: &Client, width: u32, capture_dir: &std::path::Path) {
    let rail = c.execute(
        "const e=document.querySelector('.nav-video-rail'); if(!e) return null; const first=e.querySelector('video'); e.scrollLeft=e.scrollWidth; return {moves:e.scrollLeft>0, horizontal:e.scrollWidth>e.clientWidth, portrait:first&&first.getBoundingClientRect().height>first.getBoundingClientRect().width};",
        vec![],
    ).await.expect("Vesta video rail layout");
    assert_eq!(
        rail["horizontal"], true,
        "Vesta has a horizontal video rail at {width}px"
    );
    assert_eq!(
        rail["moves"], true,
        "Vesta's video rail scrolls at {width}px"
    );
    assert_eq!(
        rail["portrait"], true,
        "Vesta video cards stay portrait at {width}px"
    );
    if width == 1280 {
        for (frame, fraction) in [0.0, 0.5, 1.0].into_iter().enumerate() {
            c.execute("const e=document.querySelector('.nav-video-rail'); e.scrollIntoView({block:'start'}); e.scrollLeft=(e.scrollWidth-e.clientWidth)*arguments[0];", vec![serde_json::json!(fraction)]).await.expect("scroll video rail");
            tokio::time::sleep(Duration::from_millis(200)).await;
            std::fs::write(
                capture_dir.join(format!("vesta-rail-{frame}.png")),
                c.screenshot().await.expect("video rail screenshot"),
            )
            .expect("save video rail frame");
        }
    }
}

/// Device metrics avoid Chromium's minimum desktop window width on phone checks.
async fn set_practice_viewport(c: &Client, width: u32, height: u32) {
    c.set_window_size(width, height)
        .await
        .expect("browser window");
    let session = c
        .session_id()
        .await
        .expect("WebDriver session")
        .expect("active session");
    reqwest::Client::new()
        .post(format!("{}/session/{session}/goog/cdp/execute", features::webdriver::webdriver_url().trim_end_matches('/')))
        .json(&serde_json::json!({
            "cmd": "Emulation.setDeviceMetricsOverride",
            "params": { "width": width, "height": height, "deviceScaleFactor": 1, "mobile": width < 768 },
        }))
        .send().await.expect("set device metrics")
        .error_for_status().expect("device metrics accepted");
}

/// Extract the notation id from a `/app/lawyer/notations/:id/step` path.
fn notation_id_from_step_path(path: &str) -> Option<Uuid> {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match segments.as_slice() {
        ["app", "lawyer", "notations", id, "step"] => Uuid::parse_str(id).ok(),
        _ => None,
    }
}

#[test]
fn notation_id_from_step_path_accepts_lawyer_step_path() {
    let id = Uuid::parse_str("018f42d4-bf4a-73fe-9f7f-8a6c51f0c6b0").unwrap();

    assert_eq!(
        notation_id_from_step_path(
            "/app/lawyer/notations/018f42d4-bf4a-73fe-9f7f-8a6c51f0c6b0/step"
        ),
        Some(id)
    );
}

#[test]
fn notation_id_from_step_path_rejects_non_lawyer_step_path() {
    assert_eq!(
        notation_id_from_step_path(
            "/app/admin/notations/018f42d4-bf4a-73fe-9f7f-8a6c51f0c6b0/step"
        ),
        None
    );
}

#[tokio::test]
async fn home_page_renders() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    c.goto(&format!("{}/", base_url())).await.unwrap();
    let title = c.title().await.unwrap();
    assert!(
        title.to_lowercase().contains("neon law"),
        "expected `neon law` in page title, got `{title}`",
    );
    c.close().await.unwrap();
}

#[tokio::test]
async fn design_page_renders_the_dioxus_gallery() {
    // The /design gallery is a shared Navigator tool behind the session
    // boundary (#732) and renders the Dioxus Components, styled by the Dioxus
    // Components theme. After sign-in a real browser must show the inline-SVG
    // icons (no icon webfont) and a themed card — the SSR content the
    // render_handler produces, readable on load.
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;
    c.goto(&format!("{}/design", base_url())).await.unwrap();
    // Inline SVG icons, rendered by the Dioxus Icon component.
    let _icon = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("svg.nav-icon"))
        .await
        .unwrap();
    // A themed card is present.
    let _card = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css(".nav-card"))
        .await
        .unwrap();
    // The brand-token swatches render, proving the theme stylesheet loaded and
    // the chip rules exist (an SSR test only sees the class name).
    let _swatch = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css(".design-swatch__chip"))
        .await
        .unwrap();
    // The URL-contract demo table renders, with a real `?sort=` header anchor.
    let _sort_link = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css(".nav-table th a[href*=\"sort=\"]"))
        .await
        .unwrap();
    c.close().await.unwrap();
}

/// The SVG specimens hydrate and update their rendered geometry from native controls.
#[tokio::test]
async fn astrology_design_controls_update_the_chart_and_map() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    c.goto(&format!("{}/design#astrology-chart", base_url()))
        .await
        .unwrap();
    let node = "//section[@id='astrology-chart']//button[contains(., 'North Node')]";
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let button = c
            .find(Locator::XPath(node))
            .await
            .expect("North Node control");
        button.click().await.expect("select a chart position");
        if button.attr("aria-pressed").await.unwrap().as_deref() == Some("true") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "chart did not hydrate"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        c.find_all(Locator::Css("#astrology-chart svg line"))
            .await
            .unwrap()
            .len()
            > 12
    );
    c.find(Locator::XPath(
        "//section[@id='astrology-chart']//button[contains(., 'Hide aspects')]",
    ))
    .await
    .unwrap()
    .click()
    .await
    .unwrap();
    c.wait()
        .at_most(Duration::from_secs(5))
        .for_element(Locator::XPath(
            "//section[@id='astrology-chart']//button[contains(., 'Show aspects')]",
        ))
        .await
        .expect("aspect visibility updated");
    assert_eq!(
        c.find_all(Locator::Css("#astrology-chart svg line"))
            .await
            .unwrap()
            .len(),
        12
    );
    assert_eq!(
        c.find_all(Locator::Css("#astrocartography .astro-map-line"))
            .await
            .unwrap()
            .len(),
        4
    );
    c.find(Locator::Css("#astrocartography input[type='checkbox']"))
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    wait_for_text(
        &c,
        "This illustrative line is hidden.",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        c.find_all(Locator::Css("#astrocartography .astro-map-line"))
            .await
            .unwrap()
            .len(),
        3
    );
    c.close().await.unwrap();
}

#[tokio::test]
async fn lawyer_logs_in_and_reaches_the_signed_in_chrome() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;

    // Sanity: the signed-in chrome is present. The nav does not name the
    // viewer, so this looks for the sign-out link rather than a per-role
    // desk.
    let _ = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("a[href^='/auth/logout']"))
        .await
        .unwrap();

    // A signed-in 403, 404, or 500 renders that same sign-out link, so the
    // signal only means "signed-in chrome" once the error body is ruled out.
    assert!(
        c.find_all(Locator::Css(".error-page"))
            .await
            .unwrap()
            .is_empty(),
        "landed on an error page after signing in as lawyer",
    );

    c.close().await.unwrap();
}

/// Fresh `dev` boot smoke: unlike the upload scenario below this test
/// creates no data. It proves the Rauthy Lawyer harness can work
/// the sample-matter fixture that startup seeded.
#[tokio::test]
async fn stock_local_personas_reach_the_litigation_matter_through_their_own_lenses() {
    // This is the presenter dry run: the Rauthy fixture, development seed,
    // and browser-visible authorization all agree without a manual SQL grant.
    let Some(lawyer_browser) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&lawyer_browser).await;
    // The stock `lawyer` login is a paralegal participant on every seeded
    // litigation matter, so it reaches the project through the *lawyer* lens at
    // `/app/projects` — the workbench backed by `visible_projects_as_lawyer`. The
    // reload-aware wait rides out a workbench that is still settling immediately
    // after the deploy's rollout.
    wait_for_text_reloading(
        &lawyer_browser,
        &format!("{}/app/projects", base_url()),
        "Cruller v. Prine",
        Duration::from_secs(30),
    )
    .await;
    lawyer_browser.close().await.unwrap();

    let Some(client_browser) = new_client_or_skip().await else {
        return;
    };
    // Virgo is the seeded client of record, so the same matter is reached
    // through the *client* lens at `/app/projects`, which renders
    // `visible_projects_as_client` for a client-tier caller.
    login_as_client(&client_browser).await;
    wait_for_text_reloading(
        &client_browser,
        &format!("{}/app/projects", base_url()),
        "Cruller v. Prine",
        Duration::from_secs(30),
    )
    .await;
    client_browser.close().await.unwrap();
}

/// Run the shared synthetic portfolio scenarios through the real local
/// Rauthy login and browser renderer. The same table drives the API runner;
/// every visible and absent fact, route, actor, and screenshot path is read
/// from `features::portfolio_acceptance::SCENARIOS`.
#[tokio::test]
async fn shared_portfolio_scenarios_match_the_api_in_a_real_browser() {
    let Some(lawyer) = new_client_or_skip().await else {
        return;
    };
    let (first_created, second_unchanged) = prepare_browser_fixture()
        .await
        .expect("apply the explicit synthetic portfolio twice to this task's KIND store");
    assert!(
        second_unchanged > 0,
        "the repeat apply must confirm existing fixture records"
    );

    login_as_lawyer(&lawyer).await;
    let counts = portfolio_acceptance::ApplyCounts {
        first_created,
        second_created: 0,
        second_unchanged,
    };
    let mut results = Vec::new();
    for scenario in portfolio_acceptance::SCENARIOS
        .iter()
        .filter(|scenario| scenario.lens == Lens::Firm)
    {
        let (actual_status, check) = visit_acceptance_scenario(&lawyer, scenario.id).await;
        results.push(portfolio_acceptance::ScenarioResult {
            scenario_id: scenario.id,
            actual_status,
            passed: check.is_ok(),
            error: check.err(),
        });
    }
    lawyer.close().await.expect("close firm browser");

    let Some(client) = new_client_or_skip().await else {
        panic!("browser harness became unavailable during acceptance walkthrough");
    };
    login_as_client(&client).await;
    for scenario in portfolio_acceptance::SCENARIOS
        .iter()
        .filter(|scenario| scenario.lens == Lens::Client)
    {
        let (actual_status, check) = visit_acceptance_scenario(&client, scenario.id).await;
        results.push(portfolio_acceptance::ScenarioResult {
            scenario_id: scenario.id,
            actual_status,
            passed: check.is_ok(),
            error: check.err(),
        });
    }
    client.close().await.expect("close client browser");

    let mut report = portfolio_acceptance::report_value(&results, counts);
    report["synthetic_only"] = serde_json::json!(true);
    let report_path =
        std::path::Path::new("/tmp/navigator-screenshots/eng-821/browser-report.json");
    std::fs::create_dir_all(report_path.parent().expect("report parent"))
        .expect("create synthetic screenshot directory");
    std::fs::write(
        report_path,
        serde_json::to_vec_pretty(&report).expect("serialize browser acceptance report"),
    )
    .expect("write browser acceptance report");
    assert!(
        results.iter().all(|result| result.passed),
        "browser acceptance failures: {:?}",
        results
            .iter()
            .filter(|result| !result.passed)
            .map(|result| (&result.scenario_id, &result.error))
            .collect::<Vec<_>>(),
    );
}

async fn visit_acceptance_scenario(c: &Client, id: &str) -> (u16, Result<(), String>) {
    let scenario = portfolio_acceptance::scenario(id)
        .expect("browser scenario ID comes from the shared table");
    c.goto(&format!("{}{route}", base_url(), route = scenario.route))
        .await
        .unwrap_or_else(|error| panic!("{} navigation failed: {error}", scenario.id));
    wait_for_path(c, scenario.route, Duration::from_secs(20)).await;
    let status = u16::try_from(
        c
        .execute(
            "return fetch(window.location.href, {cache: 'no-store'}).then(response => response.status);",
            vec![],
        )
        .await
        .unwrap_or_else(|error| panic!("{} HTTP status read failed: {error}", scenario.id))
        .as_u64()
        .unwrap_or_else(|| panic!("{} browser did not return an HTTP status", scenario.id)),
    )
    .unwrap_or_else(|_| panic!("{} browser HTTP status did not fit u16", scenario.id));
    let body = c
        .execute("return document.body.innerText || '';", vec![])
        .await
        .unwrap_or_else(|error| panic!("{} page text read failed: {error}", scenario.id))
        .as_str()
        .unwrap_or_else(|| panic!("{} browser did not return page text", scenario.id))
        .to_owned();
    let check = portfolio_acceptance::check_result(id, status, &body);

    let screenshot_path =
        std::path::Path::new("/tmp/navigator-screenshots/eng-821").join(scenario.screenshot);
    std::fs::create_dir_all(screenshot_path.parent().expect("screenshot parent"))
        .expect("create synthetic screenshot directory");
    std::fs::write(
        screenshot_path,
        c.screenshot()
            .await
            .expect("capture synthetic browser checkpoint"),
    )
    .expect("write synthetic browser checkpoint");
    (status, check)
}

/// Apply the explicit staging fixture to the task's KIND store, bridge the
/// Rauthy actors, and publish the generated portal bundle for browser access.
async fn prepare_browser_fixture() -> anyhow::Result<(usize, usize)> {
    let surreal = store::surreal::connect_from_env()
        .await
        .context("connect to task KIND store")?;
    let storage = cloud::from_env().await.context("connect to task storage")?;
    let first = store::synthetic_portfolio::apply(
        &surreal,
        &storage,
        store::DeploymentEnvironment::Dev,
        Some(portfolio_acceptance::TARGET),
    )
    .await
    .context("apply versioned portfolio to task KIND store")?;
    let second = store::synthetic_portfolio::apply(
        &surreal,
        &storage,
        store::DeploymentEnvironment::Dev,
        Some(portfolio_acceptance::TARGET),
    )
    .await
    .context("repeat versioned portfolio apply to task KIND store")?;
    anyhow::ensure!(
        first.version == store::synthetic_portfolio::PORTFOLIO_VERSION
            && first.target == portfolio_acceptance::TARGET
            && second.version == first.version
            && second.target == first.target
            && second.created() == 0,
        "portfolio repeat apply was not stable: v{} {} creates, then v{} {} creates",
        first.version,
        first.created(),
        second.version,
        second.created(),
    );
    portfolio_acceptance::bridge_acceptance_people(&surreal).await;
    portfolio_acceptance::add_firm_only_document(&surreal, &storage).await;
    Ok((first.created(), second.unchanged()))
}

#[tokio::test]
async fn client_can_continue_pending_intake_from_the_matter_page() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let client = store::persons::find_by_email_ci(&surreal, "client@neonlaw.com")
        .await
        .expect("look up the browser-harness client person")
        .expect("the browser harness requires the seeded client");
    let template = store::templates::resolve(&surreal, None, "onboarding__letter")
        .await
        .expect("look up the shipped onboarding template")
        .expect("the browser harness requires the shipped onboarding template");
    let unique = Uuid::now_v7();
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("client-intake-{unique}"),
            name: format!("Client Intake {unique}"),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .expect("seed the synthetic client-intake matter");
    store::projects::add_participation(&surreal, project.id, client.id, "client")
        .await
        .expect("scope the seeded client onto the synthetic matter");
    store::notations::create(
        &surreal,
        &store::notations::NewNotation::new(
            template.id,
            client.id,
            project.id,
            workflows::StateName::BEGIN,
        ),
    )
    .await
    .expect("seed the pending client intake");

    login_as_client(&c).await;
    c.goto(&format!("{}/app/projects/{}", base_url(), project.code))
        .await
        .expect("open the synthetic matter page");
    wait_for_text(&c, "Continue intake", Duration::from_secs(20)).await;
    let link = c
        .find(Locator::Css("a[href*='/intake/']"))
        .await
        .expect("the matter page renders the intake continuation");
    assert_eq!(link.text().await.unwrap(), "Continue intake");

    if std::env::var("NAV_ENG_402_SHOTS").as_deref() == Ok("1") {
        let dir = std::path::Path::new("/tmp/navigator-screenshots");
        std::fs::create_dir_all(dir).expect("create screenshot directory");
        std::fs::write(
            dir.join("eng-402-client-intake.png"),
            c.screenshot().await.expect("capture matter page"),
        )
        .expect("write matter-page screenshot");
    }
    c.close().await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn lawyer_walks_the_full_retainer_questionnaire_end_to_end() {
    // Drives every leg of the stepwise retainer flow in a real
    // browser:
    //   1. POST /app/lawyer/retainers/new          → /step
    //   2. POST /step × 7 (one question each) → result page
    //
    // Preconditions (beyond the module's chromedriver + KIND
    // requirements): the `onboarding__letter` template must
    // be seeded via `navigator site seed templates/`, and
    // `store/seeds/Question.yaml` must be seeded so the record-backed
    // walker question codes (`entity`, `address`, `person`, `project`) are
    // looked up successfully.
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;

    // The seven answers we'll submit, in walker order (entity →
    // address__principal_office → person__client → project__engagement →
    // custom_datetime__engagement_start_date → custom_text__engagement_scope
    // → custom_single_choice__governing_law).
    // Not every step ends up rendering the literal string here — see
    // `set_step_answer_script` below — but each is unique enough that
    // whatever the walk actually submits can be fished back out of the
    // rendered result page.
    // `custom_datetime__engagement_start_date` renders a native
    // `<input type="datetime-local">` (its `Question.answer_type` is the
    // generic `datetime`, not `custom_datetime`) — the value must be a full
    // `YYYY-MM-DDTHH:MM` string, or the browser's own value setter silently
    // blanks an incomplete one before the form ever submits it.
    // `entity` and `address__principal_office` fall through
    // `question_fields`'s default arm to the same plain `<input name="value">`
    // as `project__engagement` (no dedicated `answer_type` branch exists for
    // either). `person__client` is a `person` question instead: `start_post`
    // seeds the client as a project participant before the walk's first
    // question ever renders, so `question_fields`'s `"person"` arm always
    // finds a non-empty candidate list and renders a `<select>`, never the
    // plain input this comment used to promise (#329/ENG-454).
    let client_email = format!("walk-{}@example.com", std::process::id());
    let answers = [
        "Aurora Ridge Holdings LLC",
        "742 Meridian Ave, Reno, NV 89502",
        "Libra",
        "Estate Plan — Libra",
        "2026-09-01T00:00",
        "Draft and file the matter documents.",
        "Nevada",
    ];

    // --- Step 0: create the Notation -------------------------
    c.goto(&format!("{}/app/lawyer/retainers/new", base_url()))
        .await
        .unwrap();
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("input[name='client_email']"))
        .await
        .unwrap();
    // Set values via JS instead of send_keys (chromedriver
    // intermittently drops keystrokes on freshly-rendered forms).
    // `dispatchEvent('input')` keeps framework input listeners
    // happy and runs the browser's `required` validation against
    // the new value before we submit.
    let set_input_script = "\
        const target = document.querySelector(arguments[0]); \
        target.value = arguments[1]; \
        target.dispatchEvent(new Event('input', {bubbles: true})); \
        target.dispatchEvent(new Event('change', {bubbles: true})); \
        return target.value;";
    c.execute(
        set_input_script,
        vec![
            serde_json::Value::String("input[name='client_email']".into()),
            serde_json::Value::String(client_email.clone()),
        ],
    )
    .await
    .unwrap();
    // `retainer_template_code` renders as a <select> dropdown (the
    // onboarding-template picker), not a text input — target the element
    // that actually exists, or `querySelector` returns null and the
    // `.value =` assignment throws.
    c.execute(
        set_input_script,
        vec![
            serde_json::Value::String("select[name='retainer_template_code']".into()),
            serde_json::Value::String("onboarding__letter".into()),
        ],
    )
    .await
    .unwrap();
    // Submit the form directly — bypasses any quirks around
    // submit-button click-event delivery in fresh-loaded DOM.
    c.execute(
        "document.querySelector('form.admin-form').submit(); return true;",
        vec![],
    )
    .await
    .unwrap();

    // POST /retainers/new redirects to /app/lawyer/notations/:id/step.
    // Capture the id while we're here — we'll use it after the
    // walk to read the journal directly and confirm exactly
    // eight `notation_event` rows landed on the
    // questionnaire timeline.
    let started = std::time::Instant::now();
    let notation_id = loop {
        let url = c.current_url().await.unwrap();
        if let Some(id) = notation_id_from_step_path(url.path()) {
            break id;
        }
        assert!(
            started.elapsed() <= Duration::from_secs(10),
            "never landed on /app/lawyer/notations/:id/step; last URL was {url}",
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    // --- Steps 1–7: walk the questionnaire -------------------
    // `question_fields` renders a real `<select>` for a `person` question
    // with project-scoped candidates (`person__client` always has at least
    // the client, seeded by `start_post` before the walk begins) and an
    // `<input type="radio">` group for a `radio` question
    // (`custom_single_choice__governing_law`'s own choices). So a plain
    // `input[name="value"], textarea[name="value"]` is not on every step,
    // and each `Field::radio` option is itself an `input[name="value"]`:
    // `set_step_answer_and_submit_script` below must check for a radio group
    // *first*, or the generic text/textarea selector matches the first radio
    // and sets its `.value` without checking it (the form then submits with
    // no `value` at all). The script picks whichever control rendered,
    // submits the form in the same synchronous call, and reports both what
    // lands in the journal (`raw`) and what the rendered retainer shows
    // (`rendered`). A `<select>` pick can't honor the literal typed
    // `answers[i]` (there is no free-text fallback once real candidates
    // exist; see `webapp::components::question_fields`'s `"person"` arm),
    // and a radio answer's stored value is the choice *key* (`"nevada"`),
    // rendered back to its label (`"Nevada"`) only at template-substitution
    // time via `workflows::notation_session::choice_label`.
    let set_step_answer_and_submit_script = "\
        const desired = arguments[0]; \
        let rendered = desired; \
        let raw = desired; \
        const radios = document.querySelectorAll(\
          'input[type=\"radio\"][name=\"value\"]'); \
        if (radios.length > 0) { \
          const key = desired.toLowerCase(); \
          const target = Array.from(radios).find(r => r.value === key) || radios[0]; \
          target.checked = true; \
          target.dispatchEvent(new Event('input', {bubbles: true})); \
          target.dispatchEvent(new Event('change', {bubbles: true})); \
          raw = target.value; \
        } else { \
          const text = document.querySelector(\
            'input:not([type=\"radio\"]):not([type=\"checkbox\"])[name=\"value\"], \
             textarea[name=\"value\"]'); \
          if (text) { \
            text.value = desired; \
            text.dispatchEvent(new Event('input', {bubbles: true})); \
            text.dispatchEvent(new Event('change', {bubbles: true})); \
          } else { \
            const select = document.querySelector('select[name=\"value\"]'); \
            if (select) { \
              const options = Array.from(select.options).filter(\
                o => o.value && !o.disabled); \
              const chosen = options[0]; \
              select.value = chosen.value; \
              select.dispatchEvent(new Event('input', {bubbles: true})); \
              select.dispatchEvent(new Event('change', {bubbles: true})); \
              rendered = chosen.value; \
              raw = chosen.value; \
            } else { \
              throw new Error('no answer control found for this step'); \
            } \
          } \
        } \
        document.querySelector('form.admin-form').submit(); \
        return {rendered, raw};";

    // What actually lands in the rendered retainer per step — the literal
    // `answers[i]` for a text/textarea control, the picked candidate's id
    // for a `<select>`, or the choice label for a radio (see the script
    // above). `journal_values` is the raw stored answer, which only differs
    // from `submitted` for a radio question (the choice key, not its
    // label).
    let mut submitted: Vec<String> = answers.iter().map(|s| (*s).to_string()).collect();
    let mut journal_values: Vec<String> = answers.iter().map(|s| (*s).to_string()).collect();
    for (i, value) in answers.iter().enumerate() {
        // Each step renders "Step N of 7" (`nav-stepper__count`) — wait for
        // the right one to be sure we're looking at the form we expect.
        wait_for_text(&c, &format!("Step {} of 7", i + 1), Duration::from_secs(10)).await;

        // Set the answer value and submit in one JS round trip (chromedriver
        // send_keys is unreliable on freshly-rendered forms, and a separate
        // submit call risks losing a just-set radio `checked` to a
        // hydration re-render in between).
        let actual = c
            .execute(
                set_step_answer_and_submit_script,
                vec![serde_json::Value::String((*value).to_string())],
            )
            .await
            .unwrap();
        if let Some(rendered) = actual.get("rendered").and_then(serde_json::Value::as_str) {
            submitted[i] = rendered.to_string();
        }
        if let Some(raw) = actual.get("raw").and_then(serde_json::Value::as_str) {
            journal_values[i] = raw.to_string();
        }
    }

    // --- Result page: parked at the lawyer_review human gate --
    // The second submit completes intake and parks the notation at
    // `lawyer_review` — a true human gate. No PDF is rendered on the
    // client's completion request; the result page shows the parked
    // state, the substituted template body, and the attorney approve
    // action.
    wait_for_text(&c, "Awaiting attorney review", Duration::from_secs(20)).await;
    let src = c.source().await.unwrap();
    for value in &submitted {
        assert!(
            src.contains(value.as_str()),
            "rendered retainer is missing `{value}`",
        );
    }

    // --- Attorney writes the fee terms as a clause -----------
    // The engagement agreement leaves the fee to a custom clause, and the
    // send gate refuses an empty one (`ClausesRequired`). Add one before
    // approving; a
    // clause routes the notation back through review, so it has to land before
    // the approve rather than after it.
    c.goto(&format!(
        "{}/app/lawyer/notations/{notation_id}/clauses",
        base_url()
    ))
    .await
    .unwrap();
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("textarea[name='body']"))
        .await
        .unwrap();
    c.execute(
        "const t = document.querySelector('textarea[name=\"body\"]'); \
         t.value = arguments[0]; \
         t.dispatchEvent(new Event('input', {bubbles: true})); \
         document.querySelector('form.admin-form').submit(); return true;",
        vec![serde_json::Value::String(
            "The Firm's fee for this engagement is a flat fee stated to the Client in writing."
                .into(),
        )],
    )
    .await
    .unwrap();
    // The clause POST redirects to the clauses list; return to the review
    // screen, which now offers approve with a non-empty fee clause.
    wait_for_text_reloading(
        &c,
        &format!("{}/app/lawyer/notations/{notation_id}/review", base_url()),
        "Awaiting attorney review",
        Duration::from_secs(20),
    )
    .await;

    // --- Attorney approves: renders + parks the PDF ----------
    // Submitting the approve form fires `approved`; the worker renders +
    // persists the PDF on entering `generate_pdf__*` and the page offers
    // the deliberate send action.
    c.execute(
        "document.querySelector('form[action$=\"/approve-send\"]').submit(); return true;",
        vec![],
    )
    .await
    .unwrap();
    wait_for_text(&c, "Send for signature", Duration::from_secs(20)).await;

    // --- Attorney sends: the binding envelope goes out -------
    c.execute(
        "document.querySelector('form[action$=\"/send\"]').submit(); return true;",
        vec![],
    )
    .await
    .unwrap();
    wait_for_text(&c, "sent_for_signature__pending", Duration::from_secs(20)).await;

    c.close().await.unwrap();

    // --- Journal: read `notation_event` from SurrealDB --------
    // Talks to the in-cluster SurrealDB through `navigator dev up`'s
    // port-forward; `NAVIGATOR_SURREAL_ENDPOINT` etc. are exported by
    // `.devx/env`. We read through the same `store::notation_events` seam
    // the worker writes through, so the assertion exercises the wire
    // shape end-to-end without shelling out to a client.
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let mut events = store::notation_events::for_notation(&surreal, notation_id)
        .await
        .expect("read notation_event from surreal");
    events.retain(|e| e.machine_kind == store::notation_events::MACHINE_QUESTIONNAIRE);

    // Eight rows: BEGIN → entity → address__principal_office → person__client →
    // project__engagement → custom_datetime__engagement_start_date →
    // custom_text__engagement_scope → custom_single_choice__governing_law →
    // END. The walker signals the worker once per question (seven times) and
    // once more for the trailer-to-END in the last POST.
    assert_eq!(
        events.len(),
        8,
        "expected 8 questionnaire transitions for notation {notation_id}, got {events:?}",
    );
    let states: Vec<(&str, &str, &str)> = events
        .iter()
        .map(|e| {
            (
                e.from_state.as_str(),
                e.to_state.as_str(),
                e.condition.as_str(),
            )
        })
        .collect();
    assert_eq!(
        states,
        vec![
            ("BEGIN", "entity", "_"),
            ("entity", "address__principal_office", "_"),
            ("address__principal_office", "person__client", "_"),
            ("person__client", "project__engagement", "_"),
            (
                "project__engagement",
                "custom_datetime__engagement_start_date",
                "_"
            ),
            (
                "custom_datetime__engagement_start_date",
                "custom_text__engagement_scope",
                "_"
            ),
            (
                "custom_text__engagement_scope",
                "custom_single_choice__governing_law",
                "_"
            ),
            ("custom_single_choice__governing_law", "END", "_"),
        ],
        "questionnaire walked the wrong path",
    );
    // Payload assertions: the walker now threads the respondent's
    // answer through the signal so each of the eight answered
    // transitions carries `{"answer_value": "..."}`. The trailing
    // `custom_single_choice__governing_law → END` row has no answer and stays
    // NULL. Build the expected JSON via the same `answer_payload`
    // helper the worker uses so a future change to the JSON shape
    // can't desync the test from production.
    let expected_payloads: Vec<Option<String>> = journal_values
        .iter()
        .map(|v| Some(store::notation_events::answer_payload(v)))
        .chain(std::iter::once(None))
        .collect();
    let actual_payloads: Vec<Option<String>> = events.iter().map(|e| e.payload.clone()).collect();
    assert_eq!(
        actual_payloads, expected_payloads,
        "journal payload column does not match the answers the walker submitted",
    );
}

/// One Chrome DevTools Protocol command, sent through chromedriver's
/// `goog/cdp/execute` endpoint.
///
/// Keyboard input goes this way rather than through WebDriver key actions
/// because those are translated by the operating system's keyboard layout (on
/// a Dvorak machine pressing `g` delivers another character) and are dropped
/// intermittently on a freshly hydrated form. `Input.dispatchKeyEvent` raises
/// the same trusted `keydown`/`keyup` a physical key does, with exactly the
/// `key`, `code` and modifiers asked for.
#[derive(Debug)]
struct Cdp {
    command: &'static str,
    params: serde_json::Value,
}

impl fantoccini::wd::WebDriverCompatibleCommand for Cdp {
    fn endpoint(
        &self,
        base_url: &url::Url,
        session_id: Option<&str>,
    ) -> Result<url::Url, url::ParseError> {
        base_url.join(&format!(
            "/session/{}/goog/cdp/execute",
            session_id.unwrap_or_default()
        ))
    }

    fn method_and_body(&self, _request_url: &url::Url) -> (axum::http::Method, Option<String>) {
        (
            axum::http::Method::POST,
            Some(serde_json::json!({"cmd": self.command, "params": self.params}).to_string()),
        )
    }
}

const SHIFT: u32 = 8;
const CONTROL: u32 = 2;

/// Raise a bubbling `keydown` from script. Used when Chrome treats the
/// headless window as hidden and drops trusted CDP key events. The page
/// listener cannot tell this event from a physical key: it reads `key`,
/// `code`, and the modifier flags.
async fn dispatch_keydown_from_script(
    c: &fantoccini::Client,
    key: &str,
    code: &str,
    modifiers: u32,
) {
    eprintln!("browser_e2e: input to the page was dropped; sending {key} from script");
    c.execute(
        "const target = document.activeElement || document.body; \
         target.dispatchEvent(new KeyboardEvent('keydown', { \
           key: arguments[0], code: arguments[1], bubbles: true, cancelable: true, \
           ctrlKey: (arguments[2] & 2) !== 0, shiftKey: (arguments[2] & 8) !== 0 })); \
         return true;",
        vec![
            serde_json::Value::String(key.to_string()),
            serde_json::Value::String(code.to_string()),
            serde_json::Value::from(modifiers),
        ],
    )
    .await
    .unwrap();
}

/// Press and release one key as a trusted browser event.
///
/// A headless page drops key events now and then, so the press is confirmed:
/// a capture listener on the document counts the `keydown`s the page actually
/// received, and the press is repeated only while none has arrived. A press
/// that landed is never repeated, so a shortcut cannot fire twice.
///
/// Returns `true` when the page only saw the script-dispatched fallback. A
/// chord's later keys can then skip the CDP retry budget entirely.
async fn press_key(
    c: &fantoccini::Client,
    key: &str,
    code: &str,
    virtual_key: u32,
    modifiers: u32,
    text: Option<&str>,
) -> bool {
    for command in ["Emulation.setFocusEmulationEnabled", "Page.bringToFront"] {
        let params = if command == "Page.bringToFront" {
            serde_json::json!({})
        } else {
            serde_json::json!({ "enabled": true })
        };
        c.issue_cmd(Cdp { command, params })
            .await
            .unwrap_or_else(|err| panic!("{command}: {err}"));
    }
    c.execute(
        "if (!window.__keydowns) { window.__keydowns = 0; \
           document.addEventListener('keydown', () => { window.__keydowns += 1; }, true); } \
         window.__keydowns = 0; return true;",
        vec![],
    )
    .await
    .unwrap();
    let mut params = serde_json::json!({
        "type": if text.is_none() { "rawKeyDown" } else { "keyDown" },
        "key": key,
        "code": code,
        "windowsVirtualKeyCode": virtual_key,
        "modifiers": modifiers,
    });
    if let Some(text) = text {
        params["text"] = text.into();
    }
    let mut used_script = false;
    for attempt in 0..4 {
        if attempt < 3 {
            c.issue_cmd(Cdp {
                command: "Input.dispatchKeyEvent",
                params: params.clone(),
            })
            .await
            .unwrap_or_else(|err| panic!("dispatch {key}: {err}"));
        } else {
            // The browser took three presses and delivered none: its window is
            // being treated as hidden (a locked or sleeping display does this).
            dispatch_keydown_from_script(c, key, code, modifiers).await;
            used_script = true;
        }
        // Delivered when the counter shows a `keydown`, or when the counter is
        // gone because the key navigated the page away (a chord, or Ctrl+Enter
        // submitting the question) — the effect a key is for.
        let started = std::time::Instant::now();
        let delivered = loop {
            let seen = c.execute("return window.__keydowns;", vec![]).await;
            let landed = match seen {
                Ok(count) => count.is_null() || count.as_u64().unwrap_or(0) > 0,
                Err(_) => true,
            };
            if landed {
                break true;
            }
            // Three delivery waits must leave time for the script fallback
            // inside the one-second chord after a trusted prefix.
            if started.elapsed() > Duration::from_millis(100) {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        };
        if delivered {
            break;
        }
        assert!(attempt < 3, "the page never received the {key} keydown");
    }
    let mut up = params;
    up["type"] = "keyUp".into();
    if up.get("text").is_some() {
        up.as_object_mut().unwrap().remove("text");
    }
    c.issue_cmd(Cdp {
        command: "Input.dispatchKeyEvent",
        params: up,
    })
    .await
    .ok();
    used_script
}

/// Press the key that types `ch` — a letter, a digit or `?`.
///
/// Returns `true` when Chrome dropped the trusted event and the script
/// fallback delivered it. See [`press_key`].
async fn press_char(c: &fantoccini::Client, ch: char) -> bool {
    let text = ch.to_string();
    match ch {
        'a'..='z' => {
            let code = format!("Key{}", ch.to_ascii_uppercase());
            press_key(
                c,
                &text,
                &code,
                u32::from(ch.to_ascii_uppercase()),
                0,
                Some(&text),
            )
            .await
        }
        'A'..='Z' => {
            let code = format!("Key{ch}");
            press_key(c, &text, &code, u32::from(ch), SHIFT, Some(&text)).await
        }
        '0'..='9' => {
            let code = format!("Digit{ch}");
            press_key(c, &text, &code, u32::from(ch), 0, Some(&text)).await
        }
        '?' => press_key(c, "?", "Slash", 191, SHIFT, Some("?")).await,
        other => panic!("press_char has no key for `{other}`"),
    }
}

/// Press a sequence of characters, keeping a chord inside the script's
/// timeout when Chrome drops CDP keys. The first key still tries the
/// trusted path; later keys of a dropped-input session go straight to
/// script without waiting for input the browser is already dropping.
async fn press_chars(c: &fantoccini::Client, chars: &[char]) {
    let mut used_script = false;
    for &ch in chars {
        if used_script {
            let text = ch.to_string();
            let code = format!("Key{}", ch.to_ascii_uppercase());
            dispatch_keydown_from_script(c, &text, &code, 0).await;
        } else {
            used_script = press_char(c, ch).await;
        }
    }
}

/// Put `text` in the focused control and tell the page, the way the suite's
/// other walks enter an answer: typing characters is not what is under test
/// here, the shortcuts around it are.
async fn type_text(c: &fantoccini::Client, text: &str) {
    c.execute(
        "const a = document.activeElement; a.value = arguments[0]; \
         a.dispatchEvent(new Event('input', {bubbles: true})); \
         a.dispatchEvent(new Event('change', {bubbles: true})); return true;",
        vec![serde_json::Value::String(text.to_string())],
    )
    .await
    .unwrap();
}

async fn press_escape(c: &fantoccini::Client) {
    press_key(c, "Escape", "Escape", 27, 0, None).await;
}

/// `Ctrl+Enter` — the questionnaire's "next question, or submit" chord.
async fn press_control_enter(c: &fantoccini::Client) {
    press_key(c, "Enter", "Enter", 13, CONTROL, None).await;
}

/// The question the walker is showing: its step counter text, or `None` once
/// the questionnaire has no question form (it completed).
async fn current_question(c: &fantoccini::Client) -> Option<String> {
    let value = c
        .execute(
            "const form = document.querySelector('form[data-nav-questionnaire]'); \
             if (!form) { return null; } \
             const count = document.querySelector('.nav-stepper__count'); \
             return count ? count.textContent : '';",
            vec![],
        )
        .await
        .unwrap();
    value.as_str().map(str::to_string)
}

/// Answer every question of the questionnaire on screen with the keyboard
/// alone, then return how many were answered. No pointer, no scripted value:
/// the script has already put the cursor in the question's first input, so
/// each answer is typed (or picked with a digit, or an arrow key) where focus
/// is, and `Ctrl+Enter` moves on. The only reads of the page are to decide
/// which keys the focused control wants.
async fn walk_questionnaire_with_the_keyboard(c: &fantoccini::Client) -> usize {
    let mut answered = 0;
    while let Some(count) = current_question(c).await {
        let focus = c
            .execute(
                "const a = document.activeElement; \
                 const form = document.querySelector('form[data-nav-questionnaire]'); \
                 return {inForm: form.contains(a), tag: a.tagName, type: a.type || ''};",
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(
            focus["inForm"], true,
            "{count}: focus did not land in the question's first input: {focus}"
        );
        match (
            focus["tag"].as_str().unwrap(),
            focus["type"].as_str().unwrap(),
        ) {
            ("INPUT", "radio" | "checkbox") => {
                let _ = press_char(c, '1').await;
            }
            ("INPUT", "datetime-local") => type_text(c, "2026-09-01T12:00").await,
            ("SELECT", _) => {
                c.execute(
                    "const s = document.activeElement; \
                     s.value = Array.from(s.options).find(o => o.value && !o.disabled).value; \
                     s.dispatchEvent(new Event('change', {bubbles: true})); return true;",
                    vec![],
                )
                .await
                .unwrap();
            }
            _ => type_text(c, &format!("Keyboard answer {}", answered + 1)).await,
        }
        press_control_enter(c).await;
        answered += 1;

        let started = std::time::Instant::now();
        while current_question(c).await.as_deref() == Some(count.as_str()) {
            assert!(
                started.elapsed() <= Duration::from_secs(15),
                "{count}: Ctrl+Enter never advanced the question"
            );
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
    answered
}

/// A `?` pressed in the focused field is typing: the keydown reaches the
/// field and the overlay stays shut. Focus starts in the question's first
/// input, put there by the page's script.
async fn assert_question_mark_in_a_field_is_typing(c: &fantoccini::Client) {
    c.execute(
        "window.__seen = []; \
         document.addEventListener('keydown', e => \
           window.__seen.push(e.key + '@' + e.target.tagName), true); \
         return true;",
        vec![],
    )
    .await
    .unwrap();
    press_char(c, '?').await;
    let seen = c.execute("return window.__seen;", vec![]).await.unwrap();
    assert_eq!(
        seen,
        serde_json::json!(["?@INPUT"]),
        "the ? reached the field"
    );
    let dialog_open = c
        .execute(
            "return document.querySelector('#nav-shortcuts-dialog').open;",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        dialog_open, false,
        "? opened the overlay from inside a field"
    );
}

/// `?` opens the overlay as a labelled modal dialog listing exactly `rows` (the
/// registry on this page), and `Esc` closes it.
async fn assert_overlay_lists(c: &fantoccini::Client, rows: &[&str]) {
    // Out of any field (the only scripted step: there is no key that blurs).
    c.execute("document.activeElement.blur(); return true;", vec![])
        .await
        .unwrap();
    press_char(c, '?').await;
    let overlay = c
        .execute(
            "const d = document.querySelector('#nav-shortcuts-dialog'); \
             return {open: d.open, modal: d.matches(':modal'), \
                     label: document.getElementById(d.getAttribute('aria-labelledby')).textContent, \
                     rows: Array.from(d.querySelectorAll('dt')).map(e => e.textContent)};",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(overlay["open"], true, "{overlay}");
    assert_eq!(overlay["modal"], true, "it is a modal dialog: {overlay}");
    assert_eq!(overlay["label"], "Keyboard shortcuts", "{overlay}");
    assert_eq!(
        overlay["rows"],
        serde_json::json!(rows),
        "the overlay lists something other than the registry: {overlay}"
    );
    if std::env::var("NAV_KEYBOARD_SHOTS").as_deref() == Ok("1") {
        let dir = std::path::Path::new("/tmp/navigator-screenshots");
        std::fs::create_dir_all(dir).expect("create screenshot directory");
        std::fs::write(
            dir.join(format!("keyboard-overlay-{}-rows.png", rows.len())),
            c.screenshot().await.expect("capture the overlay"),
        )
        .expect("write the overlay screenshot");
    }

    press_escape(c).await;
    let open = c
        .execute(
            "return document.querySelector('#nav-shortcuts-dialog').open;",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(open, false, "Esc did not close the overlay");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn lawyer_walks_the_whole_questionnaire_with_the_keyboard_alone() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;

    // Start the notation (the start form is not part of the questionnaire).
    let client_email = format!("keys-{}@example.com", std::process::id());
    c.goto(&format!("{}/app/lawyer/retainers/new", base_url()))
        .await
        .unwrap();
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("input[name='client_email']"))
        .await
        .unwrap();
    c.execute(
        "document.querySelector(\"input[name='client_email']\").value = arguments[0]; \
         document.querySelector(\"select[name='retainer_template_code']\").value = \
           'onboarding__letter'; \
         document.querySelector('form.admin-form').submit(); return true;",
        vec![serde_json::Value::String(client_email)],
    )
    .await
    .unwrap();
    let started = std::time::Instant::now();
    let notation_id = loop {
        let url = c.current_url().await.unwrap();
        if let Some(id) = notation_id_from_step_path(url.path()) {
            break id;
        }
        assert!(
            started.elapsed() <= Duration::from_secs(10),
            "never landed on the first question; last URL was {url}",
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    wait_for_text(&c, "Step 1 of 7", Duration::from_secs(10)).await;

    assert_question_mark_in_a_field_is_typing(&c).await;
    assert_overlay_lists(
        &c,
        &[
            "Show keyboard shortcuts",
            "Go to matters",
            "Next question, or submit on the last",
        ],
    )
    .await;
    // The blur above left focus on the page; reload so the question loads the
    // way a reader meets it, and the script puts the cursor in its first input.
    c.refresh().await.unwrap();
    wait_for_text(&c, "Step 1 of 7", Duration::from_secs(10)).await;

    let answered = walk_questionnaire_with_the_keyboard(&c).await;
    assert_eq!(
        answered, 7,
        "the retainer questionnaire has seven questions"
    );
    wait_for_text(&c, "Awaiting attorney review", Duration::from_secs(20)).await;
    c.close().await.unwrap();

    // Exactly one transition per answer: Ctrl+Enter submitted each question once.
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let mut events = store::notation_events::for_notation(&surreal, notation_id)
        .await
        .expect("read notation_event from surreal");
    events.retain(|e| e.machine_kind == store::notation_events::MACHINE_QUESTIONNAIRE);
    assert_eq!(events.len(), 8, "expected BEGIN→…→END, got {events:?}");
    assert_eq!(events.last().unwrap().to_state, "END");
}

#[tokio::test]
async fn g_m_goes_to_the_matters_page_from_a_signed_in_page() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;
    c.goto(&format!("{}/app/profile", base_url()))
        .await
        .unwrap();
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#nav-shortcuts-dialog"))
        .await
        .unwrap();
    // Focus is on the document, not in a field.
    c.execute("document.activeElement.blur(); return true;", vec![])
        .await
        .unwrap();
    press_chars(&c, &['g', 'm']).await;
    wait_url_ends(&c, "/app/projects").await;
    c.close().await.unwrap();
}

#[tokio::test]
async fn a_chord_survives_dropped_input_after_its_prefix() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;
    c.goto(&format!("{}/app/profile", base_url()))
        .await
        .unwrap();
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#nav-shortcuts-dialog"))
        .await
        .unwrap();
    c.execute("document.activeElement.blur(); return true;", vec![])
        .await
        .unwrap();
    press_char(&c, 'g').await;
    c.issue_cmd(Cdp {
        command: "Input.setIgnoreInputEvents",
        params: serde_json::json!({ "ignore": true }),
    })
    .await
    .unwrap();
    press_char(&c, 'm').await;
    wait_url_ends(&c, "/app/projects").await;
    c.close().await.unwrap();
}

#[tokio::test]
async fn question_mark_opens_the_shortcut_overlay_from_a_signed_in_page() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;
    c.goto(&format!("{}/app/profile", base_url()))
        .await
        .unwrap();
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#nav-shortcuts-dialog"))
        .await
        .unwrap();
    // This application has no documents or notations list, so only matters has
    // a chord and the overlay lists nothing for the others.
    assert_overlay_lists(&c, &["Show keyboard shortcuts", "Go to matters"]).await;
    c.close().await.unwrap();
}

#[tokio::test]
async fn a_chord_lapses_after_a_second() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;
    c.goto(&format!("{}/app/profile", base_url()))
        .await
        .unwrap();
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#nav-shortcuts-dialog"))
        .await
        .unwrap();
    c.execute("document.activeElement.blur(); return true;", vec![])
        .await
        .unwrap();
    // `g`, a second and a bit, then `m`: nothing happens.
    let before = c.current_url().await.unwrap();
    press_char(&c, 'g').await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    press_char(&c, 'm').await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        c.current_url().await.unwrap(),
        before,
        "a lapsed chord fired"
    );
    c.close().await.unwrap();
}

#[tokio::test]
async fn client_walks_the_whole_intake_with_the_keyboard_alone() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let client = store::persons::find_by_email_ci(&surreal, "client@neonlaw.com")
        .await
        .expect("look up the browser-harness client person")
        .expect("the browser harness requires the seeded client");
    let template = store::templates::resolve(&surreal, None, "onboarding__letter")
        .await
        .expect("look up the shipped onboarding template")
        .expect("the browser harness requires the shipped onboarding template");
    let unique = Uuid::now_v7();
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("keys-intake-{unique}"),
            name: format!("Keys Intake {unique}"),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .expect("seed the synthetic intake matter");
    store::projects::add_participation(&surreal, project.id, client.id, "client")
        .await
        .expect("scope the seeded client onto the synthetic matter");
    store::notations::create(
        &surreal,
        &store::notations::NewNotation::new(
            template.id,
            client.id,
            project.id,
            workflows::StateName::BEGIN,
        ),
    )
    .await
    .expect("seed the pending client intake");

    login_as_client(&c).await;
    c.goto(&format!("{}/app/projects/{}", base_url(), project.code))
        .await
        .unwrap();
    wait_for_text(&c, "Continue intake", Duration::from_secs(20)).await;
    let href = c
        .find(Locator::Css("a[href*='/intake/']"))
        .await
        .unwrap()
        .attr("href")
        .await
        .unwrap()
        .expect("the continuation link has an href");
    c.goto(&format!("{}{href}", base_url())).await.unwrap();
    c.wait()
        .at_most(Duration::from_secs(15))
        .for_element(Locator::Css("form[data-nav-questionnaire]"))
        .await
        .unwrap();

    let answered = walk_questionnaire_with_the_keyboard(&c).await;
    assert!(answered >= 1, "the intake asked no question");
    assert!(
        current_question(&c).await.is_none(),
        "the intake did not complete"
    );
    c.close().await.unwrap();
}

#[tokio::test]
async fn lawyer_user_can_hit_every_admin_route() {
    // Walks the same admin routes the in-process test
    // (`oidc_e2e::user_with_db_lawyer_role_can_hit_every_admin_route`)
    // covers, but through a real browser end-to-end. `/app/lawyer/people` is absent
    // since ENG-304 deleted the browser mirror — the one people surface is
    // `/app/admin/people`, which a lawyer is answered 403 at.
    let routes = [
        "/app/lawyer",
        "/app/admin/entities",
        "/app/admin/jurisdictions",
        "/app/admin/entity-types",
        "/app/admin/templates",
        "/app/admin/questions",
        "/app/admin/clauses",
        "/app/admin/clauses/governing_law",
        "/app/projects",
    ];

    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;

    // Each route should render without a server-error status. WebDriver does
    // not expose the HTTP status directly, so we check for a non-error body:
    // a signed-in view carries the sign-out link, and no rendered view carries
    // the error-page body. Sign-out is the broader signal — several back-office
    // pages emit it without a matter link — so it survives a page that has not
    // picked up the matter nav.
    for route in routes {
        c.goto(&format!("{}{route}", base_url())).await.unwrap();
        let nav_links = c
            .find_all(Locator::Css("a[href^='/auth/logout']"))
            .await
            .unwrap();
        assert!(
            !nav_links.is_empty(),
            "expected the signed-in nav on {route}; got no /auth/logout links — was access denied?",
        );
        assert!(
            c.find_all(Locator::Css(".error-page"))
                .await
                .unwrap()
                .is_empty(),
            "expected {route} to render its view; got an error page",
        );
    }

    c.close().await.unwrap();
}

/// The Clauses list's keyboard path: every control a keyboard user reaches
/// inside `main`, in sequential focus order (DOM order, since none sets a
/// positive `tabindex`), described as `tag#id` or the link's `href`. A control
/// with a negative `tabindex` or `disabled` is skipped by the keyboard, so it
/// is reported as `skipped:` and fails the walk below.
async fn clause_focus_order(c: &fantoccini::Client) -> Vec<String> {
    let order = c
        .execute(
            "return Array.from(document.querySelectorAll( \
               '#clause-library a[href], #clause-library input, \
                #clause-library select, #clause-library button')) \
             .map(e => (e.tabIndex < 0 || e.disabled ? 'skipped:' : '') + \
               (e.tagName === 'A' ? e.getAttribute('href') \
                 : e.tagName.toLowerCase() + (e.id ? '#' + e.id : '[' + e.type + ']')));",
            vec![],
        )
        .await
        .unwrap();
    serde_json::from_value(order).unwrap()
}

/// Submit the Clause search form as `Enter` in its field would: through the
/// form's own submit button, so the browser makes the real GET round trip.
async fn submit_clause_search(c: &fantoccini::Client) {
    c.execute(
        "const f = document.querySelector('form.clause-search'); \
         f.requestSubmit(f.querySelector('button[type=submit]')); return true;",
        vec![],
    )
    .await
    .unwrap();
}

/// Set a Clause search control's value as a keyboard user would leave it.
async fn set_clause_control(c: &fantoccini::Client, css: &str, value: &str) {
    c.execute(
        "const e = document.querySelector(arguments[0]); e.value = arguments[1]; \
         e.dispatchEvent(new Event('input', {bubbles: true})); \
         e.dispatchEvent(new Event('change', {bubbles: true})); return true;",
        vec![
            serde_json::Value::String(css.to_string()),
            serde_json::Value::String(value.to_string()),
        ],
    )
    .await
    .unwrap();
}

/// Wait until the page's query string satisfies `reached`, then for its
/// Clause list to render.
async fn wait_for_query(c: &fantoccini::Client, what: &str, reached: impl Fn(&str) -> bool) {
    let started = std::time::Instant::now();
    loop {
        let url = c.current_url().await.unwrap();
        if url.query().is_some_and(&reached) {
            break;
        }
        assert!(
            started.elapsed() <= Duration::from_secs(10),
            "{what}: still at {url}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#clause-library .clause-count"))
        .await
        .unwrap();
}

/// The Clause codes the list page shows, in row order.
async fn clause_rows(c: &fantoccini::Client) -> Vec<String> {
    let codes = c
        .execute(
            "return Array.from(document.querySelectorAll('tr.clause-row td:first-child a'))\
               .map(a => a.textContent.trim());",
            vec![],
        )
        .await
        .unwrap();
    serde_json::from_value(codes).unwrap()
}

/// The text of the list page's `role="status"` count.
async fn clause_count(c: &fantoccini::Client) -> String {
    c.find(Locator::Css("#clause-library .clause-count"))
        .await
        .expect("the list renders its count")
        .text()
        .await
        .unwrap()
}

/// The Clauses page in a browser: browse, search, the kind filter, the empty
/// result, and a Clause's detail, with each row's usage count agreeing with
/// the Templates its detail page names. Keyboard operability is the page's
/// structure: native controls in a GET form with a real submit button, then
/// one link per row, in that focus order with nothing skipped. (Synthesized
/// trusted key presses are dropped intermittently by headless Chrome, so the
/// walk submits the form through its button rather than pressing `Enter`.)
/// The router suite proves the queries; the axe walk audits both routes.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_lawyer_browses_searches_filters_and_opens_a_clause() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;
    c.goto(&format!("{}/app/admin/clauses", base_url()))
        .await
        .unwrap();
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#clause-library tr.clause-row"))
        .await
        .expect("the lawyer sees the Clause list");
    let every = clause_rows(&c).await;
    assert!(
        every.contains(&"governing_law".to_string()),
        "the library lists governing_law: {every:?}"
    );
    let retired = c
        .execute(
            "const a = document.querySelector(\"#clause-library a[href='/app/admin/clauses/governing_law']\"); return a.closest('tr').children[1].textContent;",
            vec![],
        )
        .await
        .unwrap();
    assert!(
        retired.as_str().unwrap().contains("retired"),
        "retired wording stays discoverable: {retired}"
    );
    click_and_reach(
        &c,
        "a[href='/app/admin/clauses/governing_law']",
        "/app/admin/clauses/governing_law",
        Duration::from_secs(10),
    )
    .await;
    let retired_detail = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#clause-detail"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(retired_detail.contains("governing_law@1 (retired)"));
    assert!(retired_detail.contains("new references are refused"));
    assert!(retired_detail.contains("Governing law"));
    c.goto(&format!("{}/app/admin/clauses", base_url()))
        .await
        .unwrap();
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#clause-library tr.clause-row"))
        .await
        .unwrap();
    let n = every.len();
    assert_eq!(
        clause_count(&c).await,
        format!("Showing {n} of {n} Clauses")
    );

    // Keyboard path: search field, kind menu, Search, then each row's link.
    let order = clause_focus_order(&c).await;
    let mut expected = vec![
        "input#clause-q".to_string(),
        "select#clause-kind".to_string(),
        "button[submit]".to_string(),
    ];
    expected.extend(
        every
            .iter()
            .map(|code| format!("/app/admin/clauses/{code}")),
    );
    assert_eq!(order, expected, "the Clauses keyboard path");

    // Search by code, case-insensitively.
    set_clause_control(&c, "#clause-q", "GOVERNING_law").await;
    submit_clause_search(&c).await;
    wait_for_query(&c, "the search submits", |q| q.contains("q=GOVERNING_law")).await;
    assert_eq!(
        clause_rows(&c).await,
        vec!["governing_law".to_string()],
        "search by code"
    );
    assert_eq!(clause_count(&c).await, format!("Showing 1 of {n} Clauses"));

    // Filter by kind: every row left is of that kind.
    let kind = c
        .execute(
            "return Array.from(document.querySelectorAll('#clause-kind option'))\
               .map(o => o.value).find(v => v);",
            vec![],
        )
        .await
        .unwrap()
        .as_str()
        .expect("the kind menu offers a kind")
        .to_string();
    set_clause_control(&c, "#clause-q", "").await;
    set_clause_control(&c, "#clause-kind", &kind).await;
    submit_clause_search(&c).await;
    wait_for_query(&c, "the kind filter submits", |q| {
        q.contains(&format!("kind={kind}")) && !q.contains("GOVERNING")
    })
    .await;
    let kinds = c
        .execute(
            "return Array.from(document.querySelectorAll('tr.clause-row td:nth-child(3)'))\
               .map(td => td.textContent.trim());",
            vec![],
        )
        .await
        .unwrap();
    let kinds: Vec<String> = serde_json::from_value(kinds).unwrap();
    assert!(
        !kinds.is_empty(),
        "the {kind} filter matches its own Clauses"
    );
    assert!(
        kinds.iter().all(|k| *k == kind),
        "the kind filter is exact: {kinds:?}"
    );

    // No match renders the empty row, not an error.
    c.goto(&format!(
        "{}/app/admin/clauses?q=zz-no-such-clause",
        base_url()
    ))
    .await
    .unwrap();
    wait_for_text(&c, "No Clauses match.", Duration::from_secs(10)).await;
    assert_eq!(clause_count(&c).await, format!("Showing 0 of {n} Clauses"));

    // Usage: the list's count for dispute_resolution, which the shipped
    // agreements pin, then its row link.
    c.goto(&format!(
        "{}/app/admin/clauses?q=dispute_resolution",
        base_url()
    ))
    .await
    .unwrap();
    let used_by = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("tr.clause-row td:nth-child(5)"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    click_and_reach(
        &c,
        "a[href='/app/admin/clauses/dispute_resolution']",
        "/app/admin/clauses/dispute_resolution",
        Duration::from_secs(10),
    )
    .await;
    let heading = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#clause-detail h1"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(heading, "dispute_resolution");
    let detail = c
        .execute(
            "const m = document.querySelector('#clause-detail'); return { \
               current: Array.from(m.querySelectorAll('section.clause-version h2')) \
                 .some(h => h.textContent.includes('(current)')), \
               body: (m.querySelector('pre.clause-body') || {}).textContent || '', \
               inputs: m.querySelectorAll('.clause-inputs dt').length, \
               noInputs: m.textContent.includes('No inputs.'), \
               back: !!m.querySelector(\"a[href='/app/admin/clauses']\"), \
               pinned: Array.from(m.querySelectorAll('.clause-templates li')) \
                 .map(li => li.textContent.trim()) };",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        detail["current"], true,
        "the current version is marked: {detail}"
    );
    assert!(
        !detail["body"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "the detail shows the body: {detail}"
    );
    assert!(
        detail["inputs"].as_u64().unwrap_or(0) > 0 || detail["noInputs"] == true,
        "the detail lists the inputs or says there are none: {detail}"
    );
    assert_eq!(detail["back"], true, "the detail links back to the list");
    let mut pinned: Vec<String> = serde_json::from_value(detail["pinned"].clone()).unwrap();
    pinned.sort();
    pinned.dedup();
    assert!(
        !pinned.is_empty(),
        "the shipped agreements pin dispute_resolution: {detail}"
    );
    let expected = match pinned.len() {
        0 => "No Templates".to_string(),
        1 => "1 Template".to_string(),
        n => format!("{n} Templates"),
    };
    assert_eq!(
        used_by, expected,
        "the list's usage count names the Templates the detail page lists: {pinned:?}"
    );
    eprintln!("clauses: {n} current; dispute_resolution used by {used_by}: {pinned:?}");

    std::fs::create_dir_all("/tmp/navigator-screenshots/clauses").unwrap();
    std::fs::write(
        "/tmp/navigator-screenshots/clauses/detail.png",
        c.screenshot().await.unwrap(),
    )
    .unwrap();
    c.close().await.unwrap();
}

/// A client is refused the Clauses pages in a browser: the 403 error page,
/// with no Clause row or body on it.
#[tokio::test]
async fn a_client_is_refused_the_clause_library() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_client(&c).await;
    for route in ["/app/admin/clauses", "/app/admin/clauses/governing_law"] {
        c.goto(&format!("{}{route}", base_url())).await.unwrap();
        c.wait()
            .at_most(Duration::from_secs(10))
            .for_element(Locator::Css(".error-page"))
            .await
            .unwrap_or_else(|_| panic!("{route} answers a client with the error page"));
        let source = c.source().await.unwrap();
        assert!(
            !source.contains("clause-row") && !source.contains("clause-body"),
            "{route} leaks the library to a client"
        );
    }
    c.close().await.unwrap();
}

async fn seed_inbox_walkthrough(db: &store::surreal::SurrealDb) -> store::inbox::IngestedInbound {
    let firm = store::firms::firm_id_for_brand_key(db, "neon")
        .await
        .unwrap()
        .unwrap();
    store::inbox::ingest_inbound(
        db,
        &store::inbox::IngestInbound {
            firm_id: firm,
            brand_key: Some("neon"),
            interaction_type: Some("message"),
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: Some("private"),
            channel: "sms",
            provider_account: "synthetic-receiving-account",
            provider_message_id: &Uuid::now_v7().to_string(),
            provider_thread_ref: None,
            external_address: "synthetic-sender",
            external_name: None,
            subject: Some("Synthetic Inbox walkthrough"),
            body: "Synthetic inquiry for the browser walkthrough.",
            source_page: None,
            source_channel: Some("sms"),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachment_asset_id: None,
            occurred_at: "2026-10-10T12:00:00Z",
        },
    )
    .await
    .unwrap()
}

async fn capture_inbox_frames(
    c: &Client,
    directory: &std::path::Path,
    frames: std::ops::Range<u32>,
) {
    for frame in frames {
        std::fs::write(
            directory.join(format!("frame-{frame:03}.png")),
            c.screenshot().await.unwrap(),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

async fn activate_inbox_link(c: &Client, selector: &str) {
    c.find(Locator::Css(selector)).await.unwrap();
    // The local macOS driver can drop native input. Activate the actual anchor
    // through its DOM click, as the questionnaire harness does for submission.
    c.execute(
        "document.querySelector(arguments[0]).click(); return true;",
        vec![selector.into()],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn lawyer_workbench_card_opens_the_unified_inbox() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;
    let db = store::surreal::connect_from_env().await.unwrap();
    let saved = seed_inbox_walkthrough(&db).await;
    c.goto(&format!("{}/app/lawyers", base_url()))
        .await
        .unwrap();
    let capture_dir = std::path::Path::new("/tmp/navigator-screenshots/inbox-walkthrough");
    std::fs::create_dir_all(capture_dir).unwrap();
    c.set_window_size(1280, 900).await.unwrap();
    capture_inbox_frames(&c, capture_dir, 0..8).await;
    activate_inbox_link(&c, "#lawyer-inbox-card a[href='/app/inbox']").await;
    if let Err(error) = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#inbox"))
        .await
    {
        panic!(
            "Inbox navigation failed: {error}; {}",
            c.source().await.unwrap()
        );
    }
    assert!(c
        .current_url()
        .await
        .unwrap()
        .path()
        .ends_with("/app/inbox"));
    capture_inbox_frames(&c, capture_dir, 8..16).await;
    activate_inbox_link(
        &c,
        "nav[aria-label='Filter inbox by Brand'] a[href='/app/inbox?brand=neon']",
    )
    .await;
    activate_inbox_link(
        &c,
        "nav[aria-label='Filter inbox by channel'] a[href='/app/inbox?channel=sms&brand=neon']",
    )
    .await;
    activate_inbox_link(
        &c,
        "nav[aria-label='Inbox unread filter'] a[href$='unread=true']",
    )
    .await;
    assert_eq!(
        c.current_url().await.unwrap().query(),
        Some("channel=sms&brand=neon&unread=true")
    );
    capture_inbox_frames(&c, capture_dir, 16..24).await;
    activate_inbox_link(&c, &format!("#inbox-row-{} a", saved.conversation_id)).await;
    wait_for_text(&c, "synthetic-receiving-account", Duration::from_secs(10)).await;
    wait_for_text(&c, "synthetic-sender", Duration::from_secs(10)).await;
    capture_inbox_frames(&c, capture_dir, 24..32).await;
    c.close().await.unwrap();
}

/// The public `/blog` lists every bundled Neon Post once beside a projected
/// Post, and the Owner's catalog explains each provider's eligibility from the
/// measured video without selecting an account.
#[tokio::test]
async fn the_blog_merges_its_listings_and_the_catalog_explains_video_eligibility() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let tag = Uuid::now_v7().simple().to_string();
    let video = format!("img/neon/social/e2e-{tag}.mp4");
    store::posts::record_video(&surreal, "neon", &video, 1080, 1920)
        .await
        .expect("record a synthetic portrait upload");
    let slug = format!("e2e-video-{tag}");
    let title = format!("E2E video {tag}");
    let now = chrono::Utc::now();
    store::posts::import_source(
        &surreal,
        &store::posts::SourcePost {
            brand_key: "neon",
            slug: &slug,
            title: &title,
            description: "A synthetic browser-test Post.",
            body_html: "<p>Synthetic</p>",
            body_markdown: None,
            status: "published",
            publish_at: now - chrono::Duration::minutes(1),
            video_asset: Some(&video),
            vertical_video: Some(true),
            tiktok_ai_generated: None,
            captions: store::posts::SourceCaptions::default(),
            accounts: &[],
            social_links: &[],
        },
        now,
    )
    .await
    .expect("import a synthetic Post");

    // Release 26.10.12's KIND gate once read an empty `<main>` here on all
    // three retries, with no server error logged. Poll with a reload rather
    // than trusting one read, and keep the page source when it never settles.
    let blog_url = format!("{}/blog", base_url());
    let started = std::time::Instant::now();
    let listing = loop {
        c.goto(&blog_url).await.unwrap();
        let text = match c.find(Locator::Css("main")).await {
            Ok(main) => main.text().await.unwrap_or_default(),
            Err(_) => String::new(),
        };
        if text.contains(&title) {
            break text;
        }
        if started.elapsed() > Duration::from_secs(20) {
            let source = c.source().await.unwrap_or_default();
            panic!("the projected Post is listed: main={text:?}\nsource={source}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    for legacy in ["Thanks, Apple", "Going All-In on Rust"] {
        assert_eq!(
            listing.matches(legacy).count(),
            1,
            "{legacy} stays listed exactly once beside new Posts: {listing}"
        );
    }

    login_as_owner(&c).await;
    c.goto(&format!("{}/app/admin/blog", base_url()))
        .await
        .unwrap();
    let catalog = c
        .find(Locator::Css("main#brand-blog-catalog"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        catalog.contains("Posts for the Brands you manage, as stored in the database."),
        "{catalog}"
    );
    assert!(
        catalog.contains(&title),
        "the Owner's catalog projects the Post: {catalog}"
    );
    assert!(
        catalog.contains("Video: declared vertical; measured vertical."),
        "{catalog}"
    );
    assert!(
        catalog.contains("Instagram: eligible when selected."),
        "{catalog}"
    );
    assert!(
        !catalog.contains("Create a Blog Post"),
        "the catalog does not author Posts: {catalog}"
    );
    c.close().await.unwrap();
}

#[tokio::test]
async fn admin_adds_a_person_through_the_people_form() {
    // Drives the full browser create path end-to-end: the Dioxus "Add person"
    // form (#641 Phase 3) is a native `POST /app/admin/people` carrying the session
    // cookie plus the hidden `_csrf` field, and the handler answers a 303 back to
    // the list. This exercises the whole credential-keyed CSRF path in a real
    // browser — the thing rendering tests can't prove.
    //
    // Admin, not Lawyer: ENG-304 deleted the `/app/lawyer/people` mirror, so this is
    // the only browser form that creates a Person.
    //
    // The people directory is firm-scoped for Admin (a viewer sees only
    // people bound to a firm or project they belong to), and a freshly
    // created person carries neither binding, so the new row does not
    // necessarily render in this session's list. The database read below is
    // the proof of the write, not the DOM.
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_admin(&c).await;

    // Unique email per run so re-runs don't trip the uniqueness guard.
    let email = format!("e2e-person-{}@example.com", std::process::id());

    c.goto(&format!("{}/app/admin/people/new", base_url()))
        .await
        .unwrap();
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("input[name='name']"))
        .await
        .unwrap();

    // Set values via JS (chromedriver send_keys is flaky under load) then
    // fire input/change so any listeners see the value.
    let set_input_script = "\
        const target = document.querySelector(arguments[0]); \
        target.value = arguments[1]; \
        target.dispatchEvent(new Event('input', {bubbles: true})); \
        target.dispatchEvent(new Event('change', {bubbles: true})); \
        return target.value;";
    c.execute(
        set_input_script,
        vec![
            serde_json::Value::String("input[name='name']".into()),
            serde_json::Value::String("E2E Person".into()),
        ],
    )
    .await
    .unwrap();
    c.execute(
        set_input_script,
        vec![
            serde_json::Value::String("input[name='email']".into()),
            serde_json::Value::String(email.clone()),
        ],
    )
    .await
    .unwrap();

    // Fire the submit button's click via JS to submit the native form (a
    // WebDriver click can be intercepted when the button sits below the fold).
    scroll_and_js_click(&c, "form.admin-form button[type=\"submit\"]").await;

    // The 303 lands us on the list — proof the cookie + hidden `_csrf` write
    // reached the native create handler and it accepted the command.
    wait_for_path(&c, "/app/admin/people", Duration::from_secs(10)).await;

    // The list may not show the row (firm scoping, see above), so read the
    // store directly for the proof that the POST actually wrote the person.
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let created = store::persons::find_by_email_ci(&surreal, &email)
        .await
        .expect("look up the created person")
        .expect("the native form POST wrote the person row");
    assert_eq!(created.name, "E2E Person");

    c.close().await.unwrap();
}

/// The avatar upload and the shared profile menu must work together in a real
/// browser: selecting a file submits the multipart form, the admin page shows
/// the stored image, and the authenticated application chrome points at the
/// viewer-only image route with the existing destinations behind the disclosure.
#[tokio::test]
async fn admin_uploads_an_avatar_and_sees_the_profile_menu() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let person = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "E2E Avatar Person",
            format!("e2e-avatar-{}@example.com", Uuid::now_v7()),
            store::persons::Role::Lawyer,
        ),
    )
    .await
    .expect("seed the synthetic avatar person");
    let unique = Uuid::now_v7();
    let path = std::env::temp_dir().join(format!("navigator-avatar-{unique}.png"));
    std::fs::write(
        &path,
        [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x62, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ],
    )
    .expect("write the synthetic avatar fixture");

    login_as_admin(&c).await;
    c.goto(&format!("{}/app/admin/people/{}", base_url(), person.id))
        .await
        .unwrap();
    let picker = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#person-avatar input[type='file']"))
        .await
        .expect("the person page renders its avatar picker");
    picker
        .send_keys(path.to_str().expect("avatar path is valid UTF-8"))
        .await
        .expect("select the synthetic avatar");
    scroll_and_js_click(&c, "#person-avatar form button[type='submit']").await;
    wait_for_path(
        &c,
        &format!("/app/admin/people/{}", person.id),
        Duration::from_secs(20),
    )
    .await;
    let person_source = c.source().await.unwrap();
    assert!(
        person_source.contains(&format!("/app/admin/people/{}/avatar", person.id)),
        "the admin page should render the uploaded avatar: {person_source}"
    );

    c.goto(&format!("{}/app/projects", base_url()))
        .await
        .unwrap();
    let avatar = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css(".lawyer-nav__avatar"))
        .await
        .expect("the shared app chrome renders the viewer avatar");
    assert_eq!(
        avatar.attr("src").await.unwrap().as_deref(),
        Some("/app/me/avatar")
    );
    let trigger = c
        .find(Locator::Css(".lawyer-nav__profile-trigger"))
        .await
        .expect("the avatar is a profile-menu trigger");
    trigger.click().await.expect("open the profile menu");
    let open_menu = c
        .find(Locator::Css(".lawyer-nav__profile[open]"))
        .await
        .expect("the native disclosure opens");
    assert!(
        open_menu
            .find(Locator::Css("a[href='/app/projects']"))
            .await
            .is_ok(),
        "Projects should be inside the opened profile menu"
    );
    assert!(
        open_menu
            .find(Locator::Css("a[href='/auth/logout']"))
            .await
            .is_ok(),
        "Sign out should be inside the opened profile menu"
    );

    std::fs::remove_file(path).ok();
    c.close().await.unwrap();
}

#[tokio::test]
async fn lawyer_creates_a_client_inline_on_the_project_form() {
    // Drives the inline "New client" form on /app/projects/new end to end in
    // a real browser. The form is a native <details> disclosure with its own
    // POST, answered post/redirect/get. What the browser has to prove: after
    // the create, the new client is the selected option in the matter form's
    // Client DRI picker. Rendering tests cannot prove the redirect round-trip
    // actually preselects it; this does.
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;

    let client_email = format!("inline-client-{}@example.com", std::process::id());

    c.goto(&format!("{}/app/projects/new", base_url()))
        .await
        .unwrap();
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("input[name='name']"))
        .await
        .unwrap();

    let set_input_script = "\
        const target = document.querySelector(arguments[0]); \
        target.value = arguments[1]; \
        target.dispatchEvent(new Event('input', {bubbles: true})); \
        target.dispatchEvent(new Event('change', {bubbles: true})); \
        return target.value;";

    // Open the "New client" disclosure and fill its native form.
    scroll_and_js_click(&c, "details.inline-create:last-of-type > summary").await;
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("input[name='client_name']"))
        .await
        .unwrap();
    c.execute(
        set_input_script,
        vec![
            serde_json::Value::String("input[name='client_name']".into()),
            serde_json::Value::String("Inline Libra".into()),
        ],
    )
    .await
    .unwrap();
    c.execute(
        set_input_script,
        vec![
            serde_json::Value::String("input[name='client_email']".into()),
            serde_json::Value::String(client_email.clone()),
        ],
    )
    .await
    .unwrap();
    c.execute(
        "document.querySelector('form[action=\"/app/projects/new/client\"]').submit(); \
         return true;",
        vec![],
    )
    .await
    .unwrap();

    // The redirect lands back on the form with `?client=<id>`, and the DRI
    // picker renders that client selected. Poll until it is chosen.
    let started = std::time::Instant::now();
    loop {
        let selected = c
            .execute(
                "const s = document.querySelector('#client_dri_person_id'); \
                 if (!s) return ''; \
                 const o = s.options[s.selectedIndex]; \
                 return o ? o.textContent : '';",
                vec![],
            )
            .await
            .unwrap();
        if selected
            .as_str()
            .is_some_and(|t| t.contains("Inline Libra"))
        {
            break;
        }
        assert!(
            started.elapsed() <= Duration::from_secs(10),
            "the new client never became the selected DRI; last was {selected:?}",
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // No HTMX, no Bootstrap modal, and no leftover dialog on the page.
    let modals = c
        .execute(
            "return document.querySelectorAll('.modal, [data-bs-toggle]').length;",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        modals.as_u64(),
        Some(0),
        "the inline create must not ship a Bootstrap modal any more",
    );

    c.close().await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn admin_edits_matter_participation_from_the_project_workbench() {
    // A real admin session uses the matter workbench to add and then edit a
    // participation row. The final database read proves the browser POSTs
    // reached the project-scoped ledger, rather than only changing the DOM.
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    // The store the running `web` reads.
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let lawyer = store::persons::find_by_email_ci(&surreal, "lawyer@neonlaw.com")
        .await
        .expect("look up lawyer person")
        .expect("the browser harness requires `navigator dev grant-lawyer`");
    let original_role = lawyer.role;
    let lawyer_id = lawyer.id;
    store::persons::set_role(&surreal, lawyer_id, store::persons::Role::Admin)
        .await
        .expect("promote the browser-harness lawyer for this admin-only flow");
    login_as_lawyer(&c).await;
    let unique = Uuid::now_v7();
    let candidate = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            format!("Participation E2E {unique}"),
            format!("participation-e2e-{unique}@example.com"),
            store::persons::Role::Lawyer,
        ),
    )
    .await
    .expect("seed participation candidate");
    let client = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            format!("Participation Client {unique}"),
            format!("participation-client-{unique}@example.com"),
            store::persons::Role::Client,
        ),
    )
    .await
    .expect("seed project client");
    // The edit step re-points the row at a second person to prove the write
    // re-derives participation from that person's tier — `clerk`, not `lawyer`.
    let replacement = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            format!("Participation Clerk {unique}"),
            format!("participation-clerk-{unique}@example.com"),
            store::persons::Role::Clerk,
        ),
    )
    .await
    .expect("seed participation replacement");
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("participation-e2e-{unique}"),
            name: format!("Participation E2E Matter {unique}"),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .expect("seed project");
    store::projects::add_participation(&surreal, project.id, lawyer.id, "lawyer")
        .await
        .expect("disclose logged-in lawyer to project");

    c.goto(&format!("{}/app/projects/{}", base_url(), project.code))
        .await
        .unwrap();
    store::projects::designate_dri_in_surreal(
        &surreal,
        project.id,
        client.id,
        store::projects::DriSide::Client,
    )
    .await
    .expect("designate client DRI");
    wait_for_text(&c, "Project people", Duration::from_secs(10)).await;
    // Drive the "Add person" link through the shared click-and-navigate
    // helper. A native WebDriver `.click()` here flaked in CI: the request
    // never reached `web` (empty pod logs, issue #512), meaning the element
    // was scrolled off-screen or briefly covered and the click landed without
    // triggering navigation. `click_and_reach` scrolls-then-JS-clicks to
    // bypass the interactability race and proves navigation actually happened
    // before we blame the form for a missing selector.
    click_and_reach(
        &c,
        &format!("a[href='/app/projects/{}/people/new']", project.code),
        &format!("/app/projects/{}/people/new", project.code),
        Duration::from_secs(10),
    )
    .await;
    // The page navigated to the form route; now prove it rendered the person
    // selector. A failure here is a genuine render fault rather than a lost
    // navigation, and the diagnostic reports the URL and source defensively so
    // a failed read can't mask the original selector error.
    if let Err(error) = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("select[name='person_id']"))
        .await
    {
        let url = c.current_url().await.map_or_else(
            |url_error| format!("<failed to read form page URL: {url_error}>"),
            |url| url.to_string(),
        );
        let source = c.source().await.unwrap_or_else(|source_error| {
            format!("<failed to read form page source: {source_error}>")
        });
        panic!(
            "project participation form did not render its person selector at {url}: {error}\n{source}"
        );
    }
    let set_input_script = "\
        const target = document.querySelector(arguments[0]); \
        target.value = arguments[1]; \
        target.dispatchEvent(new Event('input', {bubbles: true})); \
        target.dispatchEvent(new Event('change', {bubbles: true})); \
        return target.value;";
    c.execute(
        set_input_script,
        vec![
            serde_json::Value::String("select[name='person_id']".into()),
            serde_json::Value::String(candidate.id.to_string()),
        ],
    )
    .await
    .unwrap();
    // No participation control to fill: the person picker is the whole form.
    c.execute(
        "document.querySelector('form.admin-form').submit(); return true;",
        vec![],
    )
    .await
    .unwrap();
    wait_for_text(&c, &candidate.name, Duration::from_secs(10)).await;

    let participation =
        store::projects::participation_for_person(&surreal, candidate.id, project.id)
            .await
            .expect("load browser-created participation")
            .expect("browser add inserted participation");
    assert_eq!(
        participation.participation, "lawyer",
        "the add derived its participation from the candidate's tier"
    );
    click_and_reach(
        &c,
        &format!(
            "a[href='/app/projects/{}/people/{}/edit']",
            project.code, participation.id
        ),
        &format!(
            "/app/projects/{}/people/{}/edit",
            project.code, participation.id
        ),
        Duration::from_secs(10),
    )
    .await;
    c.wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("select[name='person_id']"))
        .await
        .unwrap();
    c.execute(
        set_input_script,
        vec![
            serde_json::Value::String("select[name='person_id']".into()),
            serde_json::Value::String(replacement.id.to_string()),
        ],
    )
    .await
    .unwrap();
    c.execute(
        "document.querySelector('form.admin-form').submit(); return true;",
        vec![],
    )
    .await
    .unwrap();
    wait_for_text(&c, &replacement.name, Duration::from_secs(10)).await;
    let persisted = store::projects::participation_by_id(&surreal, participation.id)
        .await
        .expect("load edited participation")
        .expect("edited participation remains");
    assert_eq!(persisted.person_id, replacement.id);
    assert_eq!(
        persisted.participation, "clerk",
        "the edit re-derived participation from the incoming person's tier"
    );

    store::persons::set_role(&surreal, lawyer_id, original_role)
        .await
        .expect("restore the browser-harness lawyer role");
    c.close().await.unwrap();
}

/// Seed a project the browser-harness lawyer account can open through
/// `/app/projects/:code`, and return its id.
///
/// `can_access_as_lawyer` scopes a non-admin lawyer to projects
/// carrying a non-client participation row for them, so the fixture adds
/// one for `lawyer@neonlaw.com` — the account `login_as_lawyer` drives and
/// `navigator dev grant-lawyer` seeds.
async fn seed_lawyer_upload_project(surreal: &store::surreal::SurrealDb) -> String {
    let lawyer = store::persons::find_by_email_ci(surreal, "lawyer@neonlaw.com")
        .await
        .expect("look up the browser-harness lawyer person")
        .expect("lawyer@neonlaw.com must exist — `navigator dev grant-lawyer`");

    let entity_id = store::test_support::seed_entity(surreal).await;
    let unique = Uuid::now_v7();
    let project = store::projects::create(
        surreal,
        &store::projects::NewProject {
            code: format!("batch-upload-{unique}"),
            name: format!("Batch Upload {unique}"),
            status: "open".into(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .expect("seed the upload project");
    store::projects::add_participation(surreal, project.id, lawyer.id, "lawyer")
        .await
        .expect("scope the harness lawyer person onto the project");

    project.code
}

#[tokio::test]
async fn lawyer_uploads_several_documents_at_once_from_the_project_page() {
    // The document picker is `multiple`, so one trip through the file
    // dialog can file a whole batch. This drives the real control: it
    // hands chromedriver three newline-separated paths (the WebDriver
    // idiom for a multi-select `input[type=file]`), submits the form,
    // and asserts all three land as separate documents on the page.
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let project_code = seed_lawyer_upload_project(&surreal).await;

    // Real files on disk — chromedriver uploads from the filesystem, so
    // these can't be synthesized in the page.
    let unique = Uuid::now_v7();
    let dir = std::env::temp_dir().join(format!("navigator-batch-upload-{unique}"));
    std::fs::create_dir_all(&dir).expect("create the upload scratch dir");
    let names = ["alpha.txt", "beta.txt", "gamma.txt"];
    let mut paths = Vec::new();
    for name in names {
        let path = dir.join(name);
        std::fs::write(&path, format!("contents of {name} for {unique}"))
            .expect("write an upload fixture file");
        paths.push(path.to_string_lossy().into_owned());
    }

    login_as_lawyer(&c).await;
    c.goto(&format!("{}/app/projects/{project_code}", base_url()))
        .await
        .unwrap();

    let picker = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("section.project-documents input[type='file']"))
        .await
        .expect("the documents section renders its upload picker");

    // The picker must be multi-select, or the batch below is meaningless.
    assert_eq!(
        picker.attr("multiple").await.unwrap().as_deref(),
        Some("true"),
        "the project document picker should accept more than one file"
    );

    // WebDriver's contract for a multi-select file input: one path per
    // line, sent to the element in a single send_keys.
    picker
        .send_keys(&paths.join("\n"))
        .await
        .expect("hand the batch of paths to the file input");

    scroll_and_js_click(
        &c,
        "section.project-documents form.admin-form button[type=\"submit\"]",
    )
    .await;

    // The handler redirects back to the project page; every file in the
    // batch should now be its own row in the documents table.
    for name in names {
        wait_for_text(&c, name, Duration::from_secs(20)).await;
    }

    let source = c.source().await.unwrap();
    for name in names {
        assert!(
            source.contains(name),
            "expected {name} to appear as its own document row"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
    c.close().await.unwrap();
}

/// Send a chord of keys to the focused element as one real keyboard action:
/// press each in order, then release in reverse. Proves the Catalog step
/// page's arrow-key navigation and its guards against a real browser, not a
/// synthesized DOM event.
async fn press_chord(c: &fantoccini::Client, keys: &[char]) {
    use fantoccini::actions::{InputSource, KeyAction, KeyActions};
    let mut act = KeyActions::new("catalog-kbd".to_string());
    for &k in keys {
        act = act.then(KeyAction::Down { value: k });
    }
    for &k in keys.iter().rev() {
        act = act.then(KeyAction::Up { value: k });
    }
    c.perform_actions(act).await.unwrap();
}

/// Poll until the current URL ends with `suffix` (5s budget). A passing
/// assertion here is the proof that a keystroke actually navigated.
async fn wait_url_ends(c: &fantoccini::Client, suffix: &str) {
    let started = std::time::Instant::now();
    loop {
        if c.current_url().await.unwrap().as_str().ends_with(suffix) {
            return;
        }
        assert!(
            started.elapsed() <= Duration::from_secs(5),
            "URL never reached {suffix}",
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// The site host, against the same KIND
/// dependency tier, and the talks are mounted there. It must be explicit:
/// falling back to `NAV_BASE_URL` would drive Neon, which answers `404` for
/// every `/presentations` path.
const SITE_BASE_URL_ENV: &str = "NAV_BASE_URL";

/// Return the site's public origin. A local developer without the second
/// host can still run the rest of the suite; CI always sets
/// `NAV_REQUIRE_HARNESS=1`, so a missing endpoint is a hard failure.
fn site_base_url_or_skip() -> Option<String> {
    match env::var(SITE_BASE_URL_ENV) {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ if require_harness() => panic!(
            "NAV_REQUIRE_HARNESS=1 but {SITE_BASE_URL_ENV} is unset: \
             refusing to pass the Catalog deck test without its live host"
        ),
        _ => {
            eprintln!(
                "skipping the Catalog deck test: set {SITE_BASE_URL_ENV} to the live host origin"
            );
            None
        }
    }
}

/// The Catalog classroom step page navigates on a bare ArrowLeft/ArrowRight,
/// but Shift+Arrow (a text selection) and an arrow aimed at the Sections
/// disclosure must be left alone — regressions Greptile flagged on the
/// per-browser navigation change.
///
/// This is also the only check anywhere that `catalog-display.js` actually
/// reaches the page: the step markup renders identically with or without it,
/// so a missing hoist fails here as a navigation timeout and nowhere else.
#[tokio::test]
async fn catalog_step_arrow_keys_navigate_but_shift_and_focused_controls_do_not() {
    let Some(site_base_url) = site_base_url_or_skip() else {
        return;
    };
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    let deck = format!("{site_base_url}/presentations/rust-in-peace");

    // Plain ArrowRight advances one slide; ArrowLeft comes back.
    c.goto(&format!("{deck}/step/10")).await.unwrap();
    press_chord(&c, &[Key::Right.into()]).await;
    wait_url_ends(&c, "/step/11").await;
    press_chord(&c, &[Key::Left.into()]).await;
    wait_url_ends(&c, "/step/10").await;

    // Shift+ArrowRight extends a text selection on the reading page — it must
    // never move the deck.
    press_chord(&c, &[Key::Shift.into(), Key::Right.into()]).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        c.current_url()
            .await
            .unwrap()
            .as_str()
            .ends_with("/step/10"),
        "Shift+ArrowRight must not navigate the deck",
    );

    // An arrow key while the Sections disclosure is focused belongs to that
    // control, not the deck.
    c.execute(
        "document.querySelector('.workshop-rail .workshop-sections__toggle').focus();",
        vec![],
    )
    .await
    .unwrap();
    press_chord(&c, &[Key::Right.into()]).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        c.current_url()
            .await
            .unwrap()
            .as_str()
            .ends_with("/step/10"),
        "an arrow with the Sections button focused must not navigate the deck",
    );

    c.close().await.unwrap();
}

/// The public marketing surface at a phone width — no page renders wider than
/// its own viewport.
///
/// `/navigator` shipped a CSS Grid blowout: `.public-shell` sets
/// `grid-template-rows` but left its column implicit, so the column's
/// automatic minimum size took the min-content width of the page's
/// `<pre>` Homebrew commands (unbreakable by `white-space: pre`) and grew the
/// whole shell — header and footer included — past a 375px viewport. Every
/// practice-skin page (`/disputes`, `/business`, `/personal`,
/// `/services`) carried a second, unrelated defect: their hero's decorative
/// glow bleeds `-25vw` past each edge on purpose, and the hero it bleeds from
/// carried no `overflow: hidden` to clip that bleed back to real layout
/// width. Both are one-line CSS fixes; this is the regression gate for
/// either recurring, on the pages that actually carried them plus a couple of
/// neighbors that share the same shell and hero.
#[tokio::test]
async fn public_marketing_pages_have_no_horizontal_overflow_on_mobile() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    c.set_window_size(375, 812).await.unwrap();

    for path in ["/", "/navigator", "/notations"] {
        c.goto(&format!("{}{path}", base_url())).await.unwrap();
        wait_for_text(&c, "Neon Law", Duration::from_secs(10)).await;
        let widths = c
            .execute(
                "return [document.documentElement.scrollWidth, \
                 document.documentElement.clientWidth];",
                vec![],
            )
            .await
            .unwrap();
        let widths = widths.as_array().expect("[scrollWidth, clientWidth]");
        let scroll_width = widths[0].as_u64().unwrap();
        let client_width = widths[1].as_u64().unwrap();
        assert!(
            scroll_width <= client_width,
            "{path} scrolls horizontally at a phone width: \
             scrollWidth={scroll_width} clientWidth={client_width}",
        );
    }

    c.close().await.unwrap();
}

/// The shared `QuestionStage` chrome (ENG-503) at every width the ENG-853
/// acceptance criteria name — 320, 375, 768, and 1280px. Both walkers
/// (`webapp::client_intake` and `webapp::walker_step`, plus the portal's
/// `retainer_walk`) render through this one component, so proving it on the
/// client intake walker covers all of them.
#[tokio::test]
async fn notation_questionnaire_has_no_horizontal_overflow_at_any_checked_width() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let client = store::persons::find_by_email_ci(&surreal, "client@neonlaw.com")
        .await
        .expect("look up the browser-harness client person")
        .expect("the browser harness requires the seeded client");
    let template = store::templates::resolve(&surreal, None, "onboarding__letter")
        .await
        .expect("look up the shipped onboarding template")
        .expect("the browser harness requires the shipped onboarding template");
    let unique = Uuid::now_v7();
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("overflow-check-{unique}"),
            name: format!("Overflow Check {unique}"),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .expect("seed the synthetic overflow-check matter");
    store::projects::add_participation(&surreal, project.id, client.id, "client")
        .await
        .expect("scope the seeded client onto the synthetic matter");
    store::notations::create(
        &surreal,
        &store::notations::NewNotation::new(
            template.id,
            client.id,
            project.id,
            workflows::StateName::BEGIN,
        ),
    )
    .await
    .expect("seed the pending client intake");

    login_as_client(&c).await;
    c.goto(&format!("{}/app/projects/{}", base_url(), project.code))
        .await
        .expect("open the synthetic matter page");
    wait_for_text(&c, "Continue intake", Duration::from_secs(20)).await;
    let link = c
        .find(Locator::Css("a[href*='/intake/']"))
        .await
        .expect("the matter page renders the intake continuation");
    let href = link
        .attr("href")
        .await
        .unwrap()
        .expect("intake continuation link carries an href");
    c.goto(&format!("{}{href}", base_url()))
        .await
        .expect("open the questionnaire's first question");
    wait_for_text(&c, "Step 1 of", Duration::from_secs(20)).await;

    for (width, height) in [(320, 780), (375, 812), (768, 1024), (1280, 900)] {
        c.set_window_size(width, height).await.unwrap();
        let widths = c
            .execute(
                "return [document.documentElement.scrollWidth, \
                 document.documentElement.clientWidth];",
                vec![],
            )
            .await
            .unwrap();
        let widths = widths.as_array().expect("[scrollWidth, clientWidth]");
        let scroll_width = widths[0].as_u64().unwrap();
        let client_width = widths[1].as_u64().unwrap();
        assert!(
            scroll_width <= client_width,
            "notation questionnaire scrolls horizontally at {width}px: \
             scrollWidth={scroll_width} clientWidth={client_width}",
        );
    }

    c.close().await.unwrap();
}

/// A second 1x1 PNG (red RGB) used by the fetch-POST path so a later
/// profile-form upload of [`SYNTHETIC_PNG`] is a visible overwrite.
const SYNTHETIC_PNG_RED: [u8; 69] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// A minimal valid 1x1 PNG, reused by every profile-form avatar upload.
const SYNTHETIC_PNG: [u8; 67] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x62, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

/// Write [`SYNTHETIC_PNG`] to a uniquely tagged temp path, for the file picker
/// to select.
fn write_synthetic_png(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("navigator-avatar-{tag}-{}.png", Uuid::now_v7()));
    std::fs::write(&path, SYNTHETIC_PNG).expect("write the synthetic avatar fixture");
    path
}

/// Read `url`'s real HTTP status through the authenticated browser session.
///
/// A direct `c.goto(url)` can't tell an authorized image response apart from
/// a denial: both the avatar-visibility routes' `404` and a real image
/// response "load" as a page with no browser-level navigation error, and
/// WebDriver's `goto` exposes no status code of its own. Running `fetch`
/// inside the page carries its cookies and reports the real status back
/// through the async-script callback.
async fn fetch_status(c: &Client, url: &str) -> u64 {
    let value = c
        .execute_async(
            "const [url, callback] = arguments; \
             fetch(url, {credentials: 'same-origin'}) \
                .then(r => callback(r.status)) \
                .catch(() => callback(0));",
            vec![serde_json::Value::String(url.to_string())],
        )
        .await
        .expect("fetch the avatar url from inside the authenticated page");
    value.as_u64().expect("fetch resolves a numeric status")
}

/// Content-Type of `url` through the authenticated session. `/app/me/avatar`
/// answers `200` with an SVG initials fallback when no photo is stored, so a
/// status-only assertion cannot tell a successful upload from a no-op.
async fn fetch_content_type(c: &Client, url: &str) -> String {
    let value = c
        .execute_async(
            "const [url, callback] = arguments; \
             fetch(url, {credentials: 'same-origin'}) \
                .then(r => callback(r.headers.get('content-type') || '')) \
                .catch(() => callback(''));",
            vec![serde_json::Value::String(url.to_string())],
        )
        .await
        .expect("read the avatar content-type from inside the authenticated page");
    value
        .as_str()
        .expect("fetch resolves a content-type string")
        .to_string()
}

/// Body bytes of `url` through the authenticated session.
async fn fetch_bytes(c: &Client, url: &str) -> Vec<u8> {
    let value = c
        .execute_async(
            "const [url, callback] = arguments; \
             fetch(url, {credentials: 'same-origin'}) \
                .then(r => r.arrayBuffer()) \
                .then(buf => callback(Array.from(new Uint8Array(buf)))) \
                .catch(() => callback([]));",
            vec![serde_json::Value::String(url.to_string())],
        )
        .await
        .expect("read the avatar bytes from inside the authenticated page");
    value
        .as_array()
        .expect("fetch resolves a byte array")
        .iter()
        .map(|n| {
            u8::try_from(n.as_u64().expect("byte")).expect("a Uint8Array element fits in a byte")
        })
        .collect()
}

/// Upload a real avatar for `person_id` through the admin editor, so a
/// visibility test's target actually has bytes in storage. Without this, a
/// denial and "authorized but no avatar set" would both 404 identically —
/// the deliberate non-disclosure `docs/access-model.md` calls for — and a
/// status-code assertion couldn't tell the two apart. `admin_browser` must
/// already be signed in as `admin@neonlaw.com`.
async fn seed_person_with_avatar(admin_browser: &Client, person_id: Uuid) {
    let path = write_synthetic_png(&person_id.to_string());
    admin_browser
        .goto(&format!("{}/app/admin/people/{}", base_url(), person_id))
        .await
        .expect("open the synthetic person's admin page");
    let picker = admin_browser
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#person-avatar input[type='file']"))
        .await
        .expect("the person page renders its avatar picker");
    picker
        .send_keys(path.to_str().expect("avatar path is valid UTF-8"))
        .await
        .expect("select the synthetic avatar");
    scroll_and_js_click(admin_browser, "#person-avatar form button[type='submit']").await;
    wait_for_path(
        admin_browser,
        &format!("/app/admin/people/{person_id}"),
        Duration::from_secs(20),
    )
    .await;
    std::fs::remove_file(path).ok();
}

/// Upload a synthetic avatar through the self-service `/app/profile` page for
/// whichever role `c` is already signed in as, and confirm the round trip:
/// the redirect back to `/app/profile`, the preview `<img>` reading the
/// caller's own avatar route, and that route actually serving image bytes
/// afterward.
async fn upload_own_avatar_via_profile_page(c: &Client, tag: &str) {
    let path = write_synthetic_png(tag);
    c.goto(&format!("{}/app/profile", base_url()))
        .await
        .expect("open the profile page");
    let picker = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#profile-avatar input[type='file']"))
        .await
        .expect("the profile page renders its avatar picker");
    picker
        .send_keys(path.to_str().expect("avatar path is valid UTF-8"))
        .await
        .expect("select the synthetic avatar");
    let form = c
        .find(Locator::Css("#profile-avatar form"))
        .await
        .expect("the profile page renders the avatar upload form");
    let action = form.attr("action").await.unwrap().unwrap_or_default();
    assert!(
        action == "/app/avatar"
            || action == "/app/profile/avatar"
            || action.ends_with("/app/avatar")
            || action.ends_with("/app/profile/avatar"),
        "the live form must post to a registered self-service avatar route, got {action:?}"
    );

    let before = fetch_content_type(c, &format!("{}/app/me/avatar", base_url())).await;
    assert!(
        before.starts_with("image/svg+xml") || before.starts_with("image/png"),
        "the preview must already be the initials SVG or a stored PNG, got {before}"
    );

    scroll_and_js_click(c, "#profile-avatar form button[type='submit']").await;
    wait_for_path(c, "/app/profile", Duration::from_secs(20)).await;
    wait_for_text(c, "Avatar updated.", Duration::from_secs(20)).await;

    let avatar = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#profile-avatar img"))
        .await
        .expect("the profile page renders the avatar preview");
    let preview_src = avatar.attr("src").await.unwrap().unwrap_or_default();
    assert!(
        preview_src.starts_with("/app/me/avatar?v="),
        "the preview must read the caller's own cache-busted avatar route, got {preview_src:?}"
    );

    let after = fetch_content_type(c, &format!("{}/app/me/avatar", base_url())).await;
    assert!(
        after.starts_with("image/png"),
        "the just-uploaded PNG must replace the initials fallback, got {after}"
    );
    let body = fetch_bytes(c, &format!("{}/app/me/avatar", base_url())).await;
    assert_eq!(
        body, SYNTHETIC_PNG,
        "the preview must serve the PNG the profile form just posted"
    );

    std::fs::remove_file(path).ok();
}

/// POST [`SYNTHETIC_PNG`] to a registered self-service avatar path through
/// the authenticated session, using the profile form's CSRF token. Proves
/// the handler itself, independent of which `action` the page currently
/// renders.
async fn post_synthetic_png_to_path(c: &Client, path: &str) {
    c.goto(&format!("{}/app/profile", base_url()))
        .await
        .expect("open the profile page for CSRF");
    let csrf = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#profile-avatar form input[name='_csrf']"))
        .await
        .expect("the profile avatar form carries a CSRF token")
        .attr("value")
        .await
        .unwrap()
        .expect("the CSRF field has a value");
    let bytes: Vec<serde_json::Value> = SYNTHETIC_PNG_RED
        .iter()
        .map(|b| serde_json::Value::from(*b))
        .collect();
    let status = c
        .execute_async(
            "const [path, bytes, csrf, callback] = arguments; \
             const u8 = Uint8Array.from(bytes); \
             const fd = new FormData(); \
             fd.append('_csrf', csrf); \
             fd.append('file', new Blob([u8], {type: 'image/png'}), 'me.png'); \
             fetch(path, {method: 'POST', body: fd, credentials: 'same-origin', redirect: 'follow'}) \
                .then(r => callback(r.status)) \
                .catch(() => callback(0));",
            vec![
                serde_json::Value::String(path.to_string()),
                serde_json::Value::Array(bytes),
                serde_json::Value::String(csrf),
            ],
        )
        .await
        .expect("POST the synthetic PNG through the authenticated session");
    let status = status.as_u64().expect("fetch resolves a numeric status");
    assert_eq!(
        status, 200,
        "POST {path} must follow the 303 back to the profile page, got {status}"
    );
    let after = fetch_content_type(c, &format!("{}/app/me/avatar", base_url())).await;
    assert!(
        after.starts_with("image/png"),
        "POST {path} must store a PNG the preview serves, got {after}"
    );
    let body = fetch_bytes(c, &format!("{}/app/me/avatar", base_url())).await;
    assert_eq!(
        body.as_slice(),
        SYNTHETIC_PNG_RED.as_slice(),
        "POST {path} must serve the PNG just posted"
    );
}

// ---------- self-service avatar upload, every tier ----------
//
// Reported in staging: uploading an avatar as a client failed, because there
// was no self-service path — only the admin-only editor at
// `/app/admin/people/{id}/avatar` could write `profile_image_url`. These five
// tests drive the real `/app/profile` upload for every role.

#[tokio::test]
async fn client_uploads_their_own_avatar_through_the_profile_page() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_client(&c).await;
    upload_own_avatar_via_profile_page(&c, "client").await;
    c.close().await.unwrap();
}

#[tokio::test]
async fn lawyer_uploads_their_own_avatar_through_the_profile_page() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&c).await;
    upload_own_avatar_via_profile_page(&c, "lawyer").await;
    c.close().await.unwrap();
}

#[tokio::test]
async fn admin_uploads_their_own_avatar_through_the_profile_page() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_admin(&c).await;
    upload_own_avatar_via_profile_page(&c, "admin").await;
    c.close().await.unwrap();
}

#[tokio::test]
async fn owner_uploads_their_own_avatar_through_the_profile_page() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_owner(&c).await;
    upload_own_avatar_via_profile_page(&c, "owner").await;
    c.close().await.unwrap();
}

#[tokio::test]
async fn clerk_uploads_their_own_avatar_through_the_profile_page() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_clerk(&c).await;
    upload_own_avatar_via_profile_page(&c, "clerk").await;
    c.close().await.unwrap();
}

/// Both `/app/avatar` and `/app/profile/avatar` must accept the same PNG
/// upload so a relative resolution and an absolute nested action both succeed.
#[tokio::test]
async fn client_posts_png_to_both_self_service_avatar_routes() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_client(&c).await;
    for path in ["/app/avatar", "/app/profile/avatar"] {
        post_synthetic_png_to_path(&c, path).await;
    }
    c.close().await.unwrap();
}

#[tokio::test]
async fn profile_page_shows_a_read_only_email_with_a_contact_us_note() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_client(&c).await;
    c.goto(&format!("{}/app/profile", base_url()))
        .await
        .unwrap();
    let email_field = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css("#profile-details input[name='email']"))
        .await
        .expect("the profile page renders the email field");
    assert_eq!(
        email_field.attr("value").await.unwrap().as_deref(),
        Some("client@neonlaw.com")
    );
    assert!(
        email_field.attr("disabled").await.unwrap().is_some(),
        "the email field must render disabled — greyed out, per the product ask"
    );
    let source = c.source().await.unwrap();
    assert!(
        source.contains("Contact us to update your email address."),
        "{source}"
    );
    c.close().await.unwrap();
}

// ---------- nav collapse: Firm(s)/Brands -> Firm, plus Profile everywhere ----------

#[tokio::test]
async fn owner_nav_collapses_firm_and_brands_into_one_link() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_owner(&c).await;
    let trigger = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css(".lawyer-nav__profile-trigger"))
        .await
        .expect("the shared app chrome renders the profile menu trigger");
    trigger.click().await.expect("open the profile menu");
    let open_menu = c
        .find(Locator::Css(".lawyer-nav__profile[open]"))
        .await
        .expect("the native disclosure opens");

    let firm_link = open_menu
        .find(Locator::Css("a[href='/app/owner']"))
        .await
        .expect("the collapsed Firm link is inside the opened menu");
    assert_eq!(firm_link.text().await.unwrap(), "Firm");

    assert!(
        open_menu
            .find(Locator::Css("a[href='/app/admin/brands']"))
            .await
            .is_err(),
        "a separate Brands link must not render in the row"
    );
    assert!(
        open_menu
            .find(Locator::Css("a[href='/app/profile']"))
            .await
            .is_ok(),
        "the row must also carry the new Profile link"
    );

    c.close().await.unwrap();
}

#[tokio::test]
async fn a_client_sees_profile_but_never_the_owner_only_firm_link() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_client(&c).await;
    let trigger = c
        .wait()
        .at_most(Duration::from_secs(10))
        .for_element(Locator::Css(".lawyer-nav__profile-trigger"))
        .await
        .expect("the shared app chrome renders the profile menu trigger");
    trigger.click().await.expect("open the profile menu");
    let open_menu = c
        .find(Locator::Css(".lawyer-nav__profile[open]"))
        .await
        .expect("the native disclosure opens");

    let profile_link = open_menu
        .find(Locator::Css("a[href='/app/profile']"))
        .await
        .expect("a client also sees Profile in the menu");
    assert_eq!(profile_link.text().await.unwrap(), "Profile");
    assert!(
        open_menu
            .find(Locator::Css("a[href='/app/owner']"))
            .await
            .is_err(),
        "a client must never see the Owner-only Firm link"
    );

    c.close().await.unwrap();
}

// ---------- avatar visibility ----------
//
// A person sees their own avatar, Owner/Admin sees any avatar, and every other
// viewer needs a shared Project participation (`store::access::avatar_visible_to`).

#[tokio::test]
async fn clients_sharing_a_project_can_see_each_others_avatar() {
    let Some(admin_browser) = new_client_or_skip().await else {
        return;
    };
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let client = store::persons::find_by_email_ci(&surreal, "client@neonlaw.com")
        .await
        .expect("look up the browser-harness client person")
        .expect("the browser harness requires the seeded client");
    let unique = Uuid::now_v7();
    let entity_id = store::test_support::seed_entity(&surreal).await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("shared-avatar-{unique}"),
            name: format!("Shared Avatar {unique}"),
            status: "open".into(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .expect("seed the synthetic shared matter");
    store::projects::add_participation(&surreal, project.id, client.id, "client")
        .await
        .expect("scope the seeded client onto the synthetic matter");
    let fellow = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "E2E Fellow Client",
            format!("e2e-fellow-{unique}@example.com"),
            store::persons::Role::Client,
        ),
    )
    .await
    .expect("seed the synthetic fellow client");
    store::projects::add_participation(&surreal, project.id, fellow.id, "client")
        .await
        .expect("scope the fellow client onto the same synthetic matter");

    login_as_admin(&admin_browser).await;
    seed_person_with_avatar(&admin_browser, fellow.id).await;
    admin_browser.close().await.unwrap();

    let Some(viewer) = new_client_or_skip().await else {
        return;
    };
    login_as_client(&viewer).await;
    let status = fetch_status(
        &viewer,
        &format!("{}/app/people/{}/avatar", base_url(), fellow.id),
    )
    .await;
    assert_eq!(
        status, 200,
        "a fellow client on the same Project must see the avatar"
    );
    viewer.close().await.unwrap();
}

#[tokio::test]
async fn clients_on_different_projects_cannot_see_each_others_avatar() {
    let Some(admin_browser) = new_client_or_skip().await else {
        return;
    };
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let unique = Uuid::now_v7();
    let entity_id = store::test_support::seed_entity(&surreal).await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("unshared-avatar-{unique}"),
            name: format!("Unshared Avatar {unique}"),
            status: "open".into(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .expect("seed the synthetic unshared matter");
    let stranger = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "E2E Stranger Client",
            format!("e2e-stranger-{unique}@example.com"),
            store::persons::Role::Client,
        ),
    )
    .await
    .expect("seed the synthetic stranger client");
    store::projects::add_participation(&surreal, project.id, stranger.id, "client")
        .await
        .expect("scope the stranger client onto a matter client@neonlaw.com is not on");

    login_as_admin(&admin_browser).await;
    seed_person_with_avatar(&admin_browser, stranger.id).await;
    admin_browser.close().await.unwrap();

    let Some(viewer) = new_client_or_skip().await else {
        return;
    };
    login_as_client(&viewer).await;
    let status = fetch_status(
        &viewer,
        &format!("{}/app/people/{}/avatar", base_url(), stranger.id),
    )
    .await;
    assert_eq!(
        status, 404,
        "a client on no shared Project must not see the avatar"
    );
    viewer.close().await.unwrap();
}

#[tokio::test]
async fn an_unassigned_firm_tier_and_client_cannot_see_an_avatar() {
    let Some(admin_browser) = new_client_or_skip().await else {
        return;
    };
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let unique = Uuid::now_v7();
    let synthetic_lawyer = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "E2E Firm Avatar Person",
            format!("e2e-firm-avatar-{unique}@example.com"),
            store::persons::Role::Lawyer,
        ),
    )
    .await
    .expect("seed the synthetic lawyer");
    let synthetic_client = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "E2E Client Avatar Person",
            format!("e2e-client-avatar-{unique}@example.com"),
            store::persons::Role::Client,
        ),
    )
    .await
    .expect("seed the synthetic client");

    login_as_admin(&admin_browser).await;
    seed_person_with_avatar(&admin_browser, synthetic_lawyer.id).await;
    seed_person_with_avatar(&admin_browser, synthetic_client.id).await;
    admin_browser.close().await.unwrap();

    let Some(lawyer_browser) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&lawyer_browser).await;
    let status = fetch_status(
        &lawyer_browser,
        &format!("{}/app/people/{}/avatar", base_url(), synthetic_client.id),
    )
    .await;
    assert_eq!(
        status, 404,
        "a firm tier without shared participation must not see the avatar"
    );
    lawyer_browser.close().await.unwrap();

    let Some(client_browser) = new_client_or_skip().await else {
        return;
    };
    login_as_client(&client_browser).await;
    let status = fetch_status(
        &client_browser,
        &format!("{}/app/people/{}/avatar", base_url(), synthetic_lawyer.id),
    )
    .await;
    assert_eq!(status, 404, "an unrelated client must not see the avatar");
    client_browser.close().await.unwrap();
}

/// Published house branding remains visible before a Firm uploads replacements.
#[tokio::test]
async fn brand_editor_previews_the_registered_logo_and_typeface() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_owner(&c).await;
    c.goto(&format!("{}/app/admin/brands/vesta/edit", base_url()))
        .await
        .expect("Brand edit");
    wait_for_text(&c, "EB Garamond", Duration::from_secs(10)).await;
    let font_status = c.find(Locator::Css("#brand-font p")).await.unwrap();
    assert_eq!(
        font_status.text().await.unwrap(),
        "Current typeface: EB Garamond"
    );
    let image = c.find(Locator::Css("#brand-logo img")).await.unwrap();
    assert_eq!(
        image.attr("src").await.unwrap().as_deref(),
        Some("/public/brand/vesta.svg")
    );
    let loaded = c.execute(
        "const image = document.querySelector('#brand-logo img'); return image.complete && image.naturalWidth > 0;",
        vec![],
    ).await.unwrap();
    assert_eq!(loaded, serde_json::json!(true));
    let stylesheet = c.find(Locator::Css("#brand-font a")).await.unwrap();
    assert_eq!(
        stylesheet.attr("href").await.unwrap().as_deref(),
        Some("/public/css/brand-vesta-tokens.css")
    );
    assert_eq!(
        fetch_status(
            &c,
            &format!("{}/public/css/brand-vesta-tokens.css", base_url())
        )
        .await,
        200
    );
    let capture_dir = std::path::Path::new("/tmp/navigator-screenshots/brand-admin-social");
    std::fs::create_dir_all(capture_dir).unwrap();
    set_practice_viewport(&c, 1280, 1200).await;
    c.execute(
        "document.querySelector('[name=primary_color]').scrollIntoView({block:'center'})",
        vec![],
    )
    .await
    .unwrap();
    std::fs::write(
        capture_dir.join("picker-before.png"),
        c.screenshot().await.unwrap(),
    )
    .unwrap();
    assert!(c.find(Locator::Css("input[type='file']")).await.is_err());
    let synced = c.execute(
        "const picker = document.querySelector('[data-color-picker] input[type=color]'); picker.value = '#123456'; picker.dispatchEvent(new Event('input', {bubbles:true})); return document.querySelector('[name=primary_color]').value;",
        vec![],
    ).await.unwrap();
    assert_eq!(synced, serde_json::json!("#123456"));
    let synced = c.execute(
        "const text = document.querySelector('[name=primary_color]'); text.value = '#654321'; text.dispatchEvent(new Event('input', {bubbles:true})); return document.querySelector('[data-color-picker] input[type=color]').value;",
        vec![],
    ).await.unwrap();
    assert_eq!(synced, serde_json::json!("#654321"));
    std::fs::write(
        capture_dir.join("picker-after.png"),
        c.screenshot().await.unwrap(),
    )
    .unwrap();
    c.goto(&format!("{}/app/admin", base_url())).await.unwrap();
    wait_for_text(&c, "Open calendar", Duration::from_secs(10)).await;
    for href in ["/app/social", "/app/social/calendar"] {
        assert!(c
            .find(Locator::Css(&format!("main a[href='{href}']")))
            .await
            .is_ok());
        assert_eq!(
            fetch_status(&c, &format!("{}{href}", base_url())).await,
            200
        );
    }
    c.goto(&format!("{}/app/admin/brands/vesta/edit", base_url()))
        .await
        .unwrap();
    wait_for_text(&c, "EB Garamond", Duration::from_secs(10)).await;
    set_practice_viewport(&c, 1280, 1200).await;
    c.execute(
        "document.getElementById('brand-logo').scrollIntoView()",
        vec![],
    )
    .await
    .unwrap();
    let directory = std::path::Path::new("/tmp/navigator-screenshots/brand-assets");
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(
        directory.join("current-brand-assets.png"),
        c.screenshot().await.unwrap(),
    )
    .unwrap();
    c.close().await.unwrap();
}

#[tokio::test]
async fn admin_hub_links_to_social_and_calendar_without_owner_keys() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_admin(&c).await;
    c.goto(&format!("{}/app/admin", base_url())).await.unwrap();
    wait_for_text(&c, "Open calendar", Duration::from_secs(10)).await;
    for href in ["/app/social", "/app/social/calendar"] {
        assert!(c
            .find(Locator::Css(&format!("main a[href='{href}']")))
            .await
            .is_ok());
        assert_eq!(
            fetch_status(&c, &format!("{}{href}", base_url())).await,
            200
        );
    }
    assert_eq!(
        fetch_status(&c, &format!("{}/app/owner/keys", base_url())).await,
        403
    );
    c.close().await.unwrap();
}

/// Fill one Brand social destination and POST its selection in a single script.
///
/// `send_keys` and a native WebDriver click drop the value on this Dioxus form.
/// Waiting for "Social accounts" cannot prove the save landed — that heading is
/// already on the page before the POST.
async fn save_brand_social_selection(
    c: &Client,
    provider: &str,
    identity: &str,
    update_label: &str,
) {
    let form = format!("form[action='/app/admin/brands/neon/social/{provider}/connect']");
    let landed = c
        .execute(
            "const form = document.querySelector(arguments[0]); \
             if (!form) { return null; } \
             const input = form.querySelector(\"input[name='provider_account_id']\"); \
             if (!input) { return null; } \
             const update = form.querySelector('[data-account-update]'); \
             if (input.readOnly && update) { update.click(); } \
             const proto = Object.getPrototypeOf(input); \
             const desc = Object.getOwnPropertyDescriptor(proto, 'value'); \
             if (desc && desc.set) { desc.set.call(input, arguments[1]); } \
             else { input.value = arguments[1]; } \
             input.dispatchEvent(new Event('input', {bubbles: true})); \
             input.dispatchEvent(new Event('change', {bubbles: true})); \
             const save = form.querySelector('button[formaction$=\"/selection\"]'); \
             if (!save) { return null; } \
             save.click(); \
             return input.value;",
            vec![
                serde_json::Value::String(form),
                serde_json::Value::String(identity.to_string()),
            ],
        )
        .await
        .expect("set and save the Brand social destination");
    assert_eq!(
        landed.as_str(),
        Some(identity),
        "destination did not land before save: {landed:?}"
    );
    wait_for_text(c, update_label, Duration::from_secs(10)).await;
}

/// Provider identities are chosen within one Brand's own connection form.
#[tokio::test]
async fn brand_social_connection_form_requires_a_company_page_and_preserves_csrf() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_owner(&c).await;
    c.goto(&format!("{}/app/admin/brands/neon/edit", base_url()))
        .await
        .expect("Brand edit");
    wait_for_text(&c, "Social accounts", Duration::from_secs(10)).await;
    let form = "form[action='/app/admin/brands/neon/social/linkedin/connect']";
    let account = c
        .find(Locator::Css(&format!(
            "{form} input[name='provider_account_id']"
        )))
        .await
        .expect("Company Page ID input");
    assert!(account.attr("required").await.unwrap().is_some());
    let csrf = c
        .find(Locator::Css(&format!("{form} input[name='_csrf']")))
        .await
        .expect("native CSRF field");
    assert!(!csrf
        .attr("value")
        .await
        .unwrap()
        .unwrap_or_default()
        .is_empty());
    set_practice_viewport(&c, 1280, 1200).await;
    c.execute(
        "document.getElementById('brand-social-accounts').scrollIntoView()",
        vec![],
    )
    .await
    .expect("show social account forms");
    let capture_dir =
        std::path::Path::new("/tmp/navigator-screenshots/social-production-onboarding");
    std::fs::create_dir_all(capture_dir).unwrap();
    std::fs::write(
        capture_dir.join("brand-social-accounts.png"),
        c.screenshot().await.unwrap(),
    )
    .unwrap();
    save_brand_social_selection(&c, "linkedin", "123456789", "Update LinkedIn account").await;
    let saved = c
        .find(Locator::Css("#provider_account_id-linkedin"))
        .await
        .unwrap();
    assert!(saved.attr("readonly").await.unwrap().is_some());
    let update = "button[data-account-update='provider_account_id-linkedin']";
    scroll_and_js_click(&c, update).await;
    assert!(saved.attr("readonly").await.unwrap().is_none());
    saved.clear().await.unwrap();
    saved.send_keys("987654321").await.unwrap();
    scroll_and_js_click(&c, update).await;
    assert!(saved.attr("readonly").await.unwrap().is_some());
    assert_eq!(
        saved.prop("value").await.unwrap().as_deref(),
        Some("123456789")
    );
    let x_form = "form[action='/app/admin/brands/neon/social/x/connect']";
    let username = c
        .find(Locator::Css(&format!(
            "{x_form} input[name='provider_account_id']"
        )))
        .await
        .expect("Brand X username");
    assert!(username.attr("required").await.unwrap().is_some());
    assert!(c
        .find(Locator::Css(&format!("{x_form} input[type='checkbox']")))
        .await
        .is_err());
    c.close().await.unwrap();
}

// Exercise the production transport without exposing a test API from portal.
#[path = "../../portal/src/social_oauth/navigation.rs"]
mod social_navigation;

/// The actual Connect form can leave its same-origin form policy by mouse or keyboard.
#[tokio::test]
async fn brand_social_native_handoff_reaches_a_fake_provider_with_click_and_enter() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_owner(&c).await;
    c.goto(&format!("{}/app/admin/brands/neon/edit", base_url()))
        .await
        .unwrap();
    wait_for_text(&c, "Social accounts", Duration::from_secs(10)).await;
    let html = c
        .execute("return document.documentElement.outerHTML", vec![])
        .await
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let provider = url::Url::parse(&format!("http://localhost:{port}/consent")).unwrap();
    let app = axum::Router::new()
        .route(
            "/",
            axum::routing::get(move || {
                let html = html.clone();
                async move {
                    (
                        [(
                            "content-security-policy",
                            "default-src 'self'; form-action 'self'",
                        )],
                        axum::response::Html(html),
                    )
                }
            }),
        )
        .route(
            "/app/admin/brands/neon/social/facebook/connect",
            axum::routing::post(move || {
                let provider = provider.clone();
                async move { social_navigation::authorization_navigation(&provider, "/") }
            }),
        )
        .route(
            "/consent",
            axum::routing::get(|| async {
                axum::response::Html("<h1>Synthetic provider consent</h1>")
            }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for keyboard in [false, true] {
        c.goto(&format!("http://127.0.0.1:{port}/")).await.unwrap();
        let input = c
            .find(Locator::Css("#provider_account_id-facebook"))
            .await
            .unwrap();
        if input.attr("readonly").await.unwrap().is_none() {
            input.clear().await.unwrap();
            input.send_keys("synthetic-browser-page").await.unwrap();
        }
        let connect = c.find(Locator::Css("form[action='/app/admin/brands/neon/social/facebook/connect'] button[data-account-connect]")).await.unwrap();
        if keyboard {
            connect.send_keys("\u{e007}").await.unwrap();
        } else {
            connect.click().await.unwrap();
        }
        wait_for_text(&c, "Synthetic provider consent", Duration::from_secs(10)).await;
        assert_eq!(c.current_url().await.unwrap().host_str(), Some("localhost"));
    }
    c.close().await.unwrap();
    server.abort();
    let _ = server.await;
}

/// Saving a destination survives reload and an unavailable authorization provider.
#[tokio::test]
async fn brand_social_selection_survives_reload_and_connect_failure() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_owner(&c).await;
    c.goto(&format!("{}/app/admin/brands/neon/edit", base_url()))
        .await
        .unwrap();
    wait_for_text(&c, "Social accounts", Duration::from_secs(10)).await;
    let capture_dir =
        std::path::Path::new("/tmp/navigator-screenshots/social-selection-walkthrough");
    std::fs::create_dir_all(capture_dir).unwrap();
    let recording_client = c.clone();
    let recording = tokio::spawn(async move {
        for index in 0..300 {
            if let Ok(pixels) = recording_client.screenshot().await {
                std::fs::write(capture_dir.join(format!("frame-{index:04}.png")), pixels).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
    let identity = format!("synthetic-page-{}", Uuid::now_v7());
    let selector = "#provider_account_id-facebook";
    save_brand_social_selection(&c, "facebook", &identity, "Update Facebook account").await;
    c.refresh().await.unwrap();
    wait_for_text(&c, "Update Facebook account", Duration::from_secs(10)).await;
    assert_eq!(
        c.find(Locator::Css(selector))
            .await
            .unwrap()
            .attr("value")
            .await
            .unwrap()
            .as_deref(),
        Some(identity.as_str())
    );
    // A keypress sent while the reloaded page hydrates is dropped, as `send_keys` is in
    // `save_brand_social_selection`; the keyboard path is proven against a static page in
    // `brand_social_native_handoff_reaches_a_fake_provider_with_click_and_enter`.
    scroll_and_js_click(
        &c,
        "form[action='/app/admin/brands/neon/social/facebook/connect'] button[data-account-connect]",
    )
    .await;
    wait_for_text(
        &c,
        "Social account connection is not configured",
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(
        c.find(Locator::Css(selector))
            .await
            .unwrap()
            .attr("value")
            .await
            .unwrap()
            .as_deref(),
        Some(identity.as_str())
    );
    assert!(c
        .find(Locator::Css("section[aria-label='Meta']"))
        .await
        .is_ok());
    assert!(c
        .find(Locator::Css(
            "form[action='/app/admin/brands/neon/social/threads/connect']"
        ))
        .await
        .is_ok());
    recording.abort();
    let _ = recording.await;
    c.close().await.unwrap();
}

/// Owner key metadata stays distinct from deployment and account authorization.
#[tokio::test]
async fn owner_social_keys_are_blank_and_configuration_check_is_read_only() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_owner(&c).await;
    c.goto(&format!("{}/app/owner/keys", base_url()))
        .await
        .unwrap();
    let page = c.find(Locator::Css("#deployment-keys")).await.unwrap();
    let text = page.text().await.unwrap();
    assert!(text.contains("Social app keys"), "{text}");
    assert!(text.contains("dedicated runtime encryption key"), "{text}");
    let fields = c
        .find_all(Locator::Css("#deployment-keys input[type='password']"))
        .await
        .unwrap();
    assert_eq!(fields.len(), 8);
    for field in fields {
        assert_eq!(field.prop("value").await.unwrap().as_deref(), Some(""));
    }
    scroll_and_js_click(&c, "form[action='/app/owner/keys/test'] button").await;
    wait_for_text(&c, "Configuration checked", Duration::from_secs(10)).await;
    let text = c
        .find(Locator::Css("#deployment-keys"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(text.contains("not provider approval or Brand authorization"));
    c.close().await.unwrap();
    let c = new_client_or_skip()
        .await
        .expect("browser harness remains available");
    login_as_admin(&c).await;
    c.goto(&format!("{}/app/owner/keys", base_url()))
        .await
        .unwrap();
    assert!(c.find(Locator::Css("#deployment-keys")).await.is_err());
    c.close().await.unwrap();
}

/// Business publishing requires a chosen Brand identity; OAuth grants consent.
#[tokio::test]
async fn brand_tiktok_connection_requires_username_without_redundant_checkbox() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    login_as_owner(&c).await;
    c.goto(&format!("{}/app/admin/brands/neon/edit", base_url()))
        .await
        .unwrap();
    wait_for_text(&c, "Social accounts", Duration::from_secs(10)).await;
    let form = "form[action='/app/admin/brands/neon/social/tiktok/connect']";
    let username = c
        .find(Locator::Css(&format!(
            "{form} input[name='provider_account_id']"
        )))
        .await
        .unwrap();
    assert!(username.attr("required").await.unwrap().is_some());
    assert!(c
        .find(Locator::Css(&format!("{form} input[type='checkbox']")))
        .await
        .is_err());
    let valid = c
        .execute(
            "const form = document.querySelector(arguments[0]); \
             if (!form) { return false; } \
             const input = form.querySelector(\"input[name='provider_account_id']\"); \
             if (!input) { return false; } \
             const proto = Object.getPrototypeOf(input); \
             const desc = Object.getOwnPropertyDescriptor(proto, 'value'); \
             if (desc && desc.set) { desc.set.call(input, 'synthetic_business'); } \
             else { input.value = 'synthetic_business'; } \
             input.dispatchEvent(new Event('input', {bubbles: true})); \
             return form.checkValidity();",
            vec![serde_json::Value::String(form.to_string())],
        )
        .await
        .unwrap();
    assert_eq!(valid.as_bool(), Some(true));
    set_practice_viewport(&c, 1280, 1000).await;
    c.execute(
        &format!("document.querySelector(\"{form}\").scrollIntoView({{block:'center'}})"),
        vec![],
    )
    .await
    .unwrap();
    let directory = std::path::Path::new("/tmp/navigator-screenshots/tiktok-business");
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(
        directory.join("brand-tiktok-connection.png"),
        c.screenshot().await.unwrap(),
    )
    .unwrap();
    c.close().await.unwrap();
}

/// The `pages:` blocks the three sample repositories carry, shared with the
/// router-level suite in `portal/tests/litigation_records`.
const SAMPLE_PAGES: &[(&str, &str)] = &[
    (
        store::seed::SAMPLE_LITIGATION_CODE,
        include_str!("../../portal/tests/litigation_records/fixtures/sample-litigation.pages.yaml"),
    ),
    (
        store::seed::SAMPLE_ESTATE_CODE,
        include_str!("../../portal/tests/litigation_records/fixtures/sample-estate.pages.yaml"),
    ),
    (
        store::seed::SAMPLE_TRANSACTIONAL_CODE,
        include_str!(
            "../../portal/tests/litigation_records/fixtures/sample-transactional.pages.yaml"
        ),
    ),
];

/// Store each sample matter's declared pages, as `project sync` would, and
/// return every `(code, slug)` pair.
async fn store_sample_pages(surreal: &store::surreal::SurrealDb) -> Vec<(String, String)> {
    let mut declared = Vec::new();
    for (code, yaml) in SAMPLE_PAGES {
        let document: serde_yaml::Value = serde_yaml::from_str(yaml).expect("pages fixture");
        let pages = rules::pages::entries(document.as_mapping().expect("a mapping"))
            .expect("pages entries");
        let project = store::projects::find_by_code(surreal, code)
            .await
            .expect("look up the sample matter")
            .unwrap_or_else(|| panic!("the development seed opens `{code}`"));
        store::matter_pages::replace_for_project(surreal, project.id, &pages)
            .await
            .expect("store the sample pages");
        declared.extend(pages.into_iter().map(|page| (code.to_string(), page.slug)));
    }
    declared
}

/// Every declared page of the three sample matters renders in a browser for
/// the fixture client and the fixture lawyer, each in their own lens, from
/// the records the development seed writes.
#[tokio::test]
async fn every_sample_matter_page_renders_in_the_client_and_lawyer_lens() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    let surreal = store::surreal::connect_from_env()
        .await
        .expect("connect to the port-forwarded SurrealDB");
    let declared = store_sample_pages(&surreal).await;

    for (lens, login) in [("client", true), ("lawyer", false)] {
        if login {
            login_as_client(&c).await;
        } else {
            c.delete_all_cookies().await.unwrap();
            login_as_lawyer(&c).await;
        }
        for (code, slug) in &declared {
            c.goto(&format!("{}/app/projects/{code}/{slug}", base_url()))
                .await
                .unwrap();
            c.wait()
                .at_most(Duration::from_secs(10))
                .for_element(Locator::Css(&format!("[data-lens='{lens}']")))
                .await
                .unwrap_or_else(|_| panic!("/app/projects/{code}/{slug} renders the {lens} lens"));
            let boundary = c
                .find_all(Locator::Css("[data-section='boundary_note']"))
                .await
                .unwrap();
            assert!(
                !boundary.is_empty(),
                "/app/projects/{code}/{slug} carries its boundary note for the {lens}"
            );
        }
    }
    c.close().await.unwrap();
}

/// A real ten-second invitation can be dismissed without taking the visitor away.
#[tokio::test]
async fn homepage_signup_prompt_waits_and_stays_dismissed() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    c.set_window_size(1280, 900).await.unwrap();
    c.goto(&format!("{}/", base_url())).await.unwrap();
    c.execute(
        "sessionStorage.removeItem('navigator.signup.offered');",
        vec![],
    )
    .await
    .unwrap();
    c.refresh().await.unwrap();
    c.wait()
        .at_most(Duration::from_secs(15))
        .for_element(Locator::Css("#signup-prompt"))
        .await
        .expect("signup dialog markup");
    assert_eq!(
        c.execute(
            "return document.querySelector('#signup-prompt').open;",
            vec![]
        )
        .await
        .unwrap(),
        serde_json::json!(false)
    );
    let frames = std::path::Path::new("/tmp/navigator-screenshots/signup-walkthrough");
    std::fs::create_dir_all(frames).unwrap();
    for frame in 0..12 {
        std::fs::write(
            frames.join(format!("{frame:03}.png")),
            c.screenshot().await.unwrap(),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(
        c.execute(
            "return document.querySelector('#signup-prompt').open;",
            vec![]
        )
        .await
        .unwrap(),
        serde_json::json!(true)
    );
    assert_eq!(
        c.find(Locator::Css("#signup-prompt a"))
            .await
            .unwrap()
            .attr("href")
            .await
            .unwrap()
            .as_deref(),
        Some("/auth/signup")
    );
    c.find(Locator::Css("[data-signup-dismiss]"))
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    std::fs::write(frames.join("012.png"), c.screenshot().await.unwrap()).unwrap();
    c.refresh().await.unwrap();
    tokio::time::sleep(Duration::from_secs(11)).await;
    assert_eq!(
        c.execute(
            "return document.querySelector('#signup-prompt').open;",
            vec![]
        )
        .await
        .unwrap(),
        serde_json::json!(false)
    );
    c.execute(
        "document.querySelector('.site-footer__provider').scrollIntoView({block:'center',behavior:'instant'});",
        vec![],
    )
    .await
    .unwrap();
    std::fs::write(
        "/tmp/navigator-screenshots/website-footer.png",
        c.screenshot().await.unwrap(),
    )
    .unwrap();
    c.close().await.unwrap();
}
