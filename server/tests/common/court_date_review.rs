//! Mount the production court-date workbench and its browser assets with synthetic Project responses.
use std::time::Duration;

use base64::Engine as _;
use dioxus::prelude::VirtualDom;
use fantoccini::{Client, Locator};
use features::webdriver::{new_client_for_fixture_or_skip, wait_for_text};
use serde_json::{json, Value};
use webapp::court_date_review::{CourtDateReview, CourtDateReviewProps};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const AUTHORITY: &str = "2030 TEST 34, ¶ 12";
const API: &str = "/app/api/projects/sample/court-date-proposals";

async fn fixture(supervised: bool, proposals: Value) -> MockServer {
    let dom = VirtualDom::new_with_props(
        CourtDateReview,
        CourtDateReviewProps {
            project_id: "sample".into(),
            project_code: "sample-litigation".into(),
            csrf: "synthetic-token".into(),
            supervised,
        },
    );
    let server = mounted(dom, "Sample litigation").await;
    Mock::given(method("GET"))
        .and(path(API))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"can_confirm":!supervised,"proposals":proposals})),
        )
        .mount(&server)
        .await;
    server
}

async fn mounted(mut dom: VirtualDom, heading: &str) -> MockServer {
    let server = MockServer::start().await;
    dom.rebuild_in_place();
    let component = dioxus_ssr::render(&dom);
    let html = format!(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
    <link rel="stylesheet" href="/public/css/tokens.css"><link rel="stylesheet" href="/public/css/theme.css">
    <link rel="stylesheet" href="/public/css/court-date-review.css"><link rel="stylesheet" href="/public/css/citation-highlight.css"><title>{heading} · Navigator</title>
    <main class="nav-theme lawyer-detail" style="max-width:70rem;padding:1rem 2rem;margin:auto">
    <h1 class="project-heading">{heading}</h1>{component}</main>
    <script src="/public/js/keyboard-shortcuts.js"></script><script src="/public/js/citation-highlight.js"></script><script src="/public/js/court-date-review.js"></script></html>"#
    );
    Mock::given(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(html, "text/html"))
        .mount(&server)
        .await;
    for (name, kind, body) in [
        (
            "js/citation-highlight.js",
            "application/javascript",
            include_str!("../../public/js/citation-highlight.js"),
        ),
        (
            "css/citation-highlight.css",
            "text/css",
            include_str!("../../public/css/citation-highlight.css"),
        ),
        (
            "js/court-date-review.js",
            "application/javascript",
            include_str!("../../public/js/court-date-review.js"),
        ),
        (
            "js/keyboard-shortcuts.js",
            "application/javascript",
            include_str!("../../public/js/keyboard-shortcuts.js"),
        ),
        (
            "css/court-date-review.css",
            "text/css",
            include_str!("../../public/css/court-date-review.css"),
        ),
        (
            "css/theme.css",
            "text/css",
            include_str!("../../public/css/theme.css"),
        ),
        (
            "css/tokens.css",
            "text/css",
            include_str!("../../public/css/tokens.css"),
        ),
    ] {
        Mock::given(path(format!("/public/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, kind))
            .mount(&server)
            .await;
    }
    server
}

async fn click(client: &Client, selector: &str) {
    client
        .find(Locator::Css(selector))
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
}

async fn assert_authority_repository_link(client: &Client) {
    let provenance = client
        .find(Locator::Css("dialog a[href*='/forgejo?revision=3']"))
        .await
        .unwrap();
    assert_eq!(provenance.text().await.unwrap(), "Open in Forgejo");
    assert_eq!(
        provenance.attr("title").await.unwrap().as_deref(),
        Some("Firm Library · rules/2030-34.pdf · 7d431edc")
    );
}

async fn assert_confirm_explains_calendar_sync(client: &Client) {
    let note = client
        .find(Locator::Id("court-date-calendar-order"))
        .await
        .unwrap();
    assert_eq!(
        note.text().await.unwrap(),
        "Confirmed dates appear on the matter’s shared calendar after the next sync."
    );
    assert_eq!(
        client
            .find(Locator::Css("[data-detail] .nav-btn--primary"))
            .await
            .unwrap()
            .attr("aria-describedby")
            .await
            .unwrap()
            .as_deref(),
        Some("court-date-calendar-order")
    );
}

async fn capture(client: &Client, name: &str) {
    let dir = std::path::Path::new("/tmp/navigator-screenshots/court-date-review");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), client.screenshot().await.unwrap()).unwrap();
}

fn page(role: &str) -> Value {
    let label = match role {
        "authority" => AUTHORITY,
        "trigger" => "Certificate of service.pdf",
        _ => "Signed scheduling order.pdf",
    };
    let mut source = json!({"role":role,"label":label,"page":1,"rects":[{"x":0.074,"y":0.23,"width":0.85,"height":0.045}]});
    match role {
        "authority" => {
            source["forgejo"] = json!({"repository":"firm_library","path":"rules/2030-34.pdf","commit":"7d431edce4c3c21e45cdc8753633c97c9cd2c845"});
            source["forgejo_url"] = json!(format!("{API}/response/evidence/1/forgejo?revision=3"));
        }
        "trigger" => {
            source["forgejo_url"] = json!(format!("{API}/response/evidence/0/forgejo?revision=2"));
        }
        _ => {}
    }
    source
}

// A synthetic one-page filing with no external font or document dependency.
async fn source(
    client: &Client,
    server: &MockServer,
    proposal: &str,
    index: u8,
    title: &str,
    passage: &str,
) {
    let png = client.execute(r"const c = document.createElement('canvas'); c.width=900; c.height=1060;
        const g=c.getContext('2d'); g.fillStyle='#ffffff'; g.fillRect(0,0,c.width,c.height);
        g.fillStyle='#2d322f'; g.font='bold 32px Georgia'; g.fillText(arguments[0],70,100);
        g.font='17px Georgia'; g.fillText('Sample litigation · Court record',70,139);
        g.fillRect(70,166,760,1); g.font='22px Georgia'; g.fillText(arguments[1],70,274);
        if (arguments[0] === 'Signed scheduling order') {
          g.font='italic 21px Georgia'; g.fillText('/s/ Sample Judge',70,420);
        }
        g.font='16px Georgia'; g.fillText('Page 1',420,997); return c.toDataURL('image/png').split(',')[1];",
        vec![json!(title),json!(passage)]).await.unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png.as_str().unwrap())
        .unwrap();
    Mock::given(path(format!("{API}/{proposal}/evidence/{index}")))
        .and(wiremock::matchers::query_param("revision", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(bytes, "image/png"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn returned_clerk_proposal_exposes_its_revision_without_confirm_or_authority_fetch() {
    let server = fixture(true,json!([{
        "id":"synthetic-proposal","status":"returned","kind":"calculated","revision":3,
        "title":"Response deadline","reason":"Verify the date on the certificate of service.","timezone":"America/New_York",
        "due_on":"2030-07-16","evidence":[page("trigger"),page("authority")]
    }])).await;
    let Some(client) = new_client_for_fixture_or_skip(&server.uri()).await else {
        return;
    };
    source(
        &client,
        &server,
        "synthetic-proposal",
        0,
        "Certificate of service",
        "The motion was served by hand on July 2, 2030.",
    )
    .await;
    client.goto(&server.uri()).await.unwrap();
    wait_for_text(
        &client,
        "Verify the date on the certificate of service.",
        Duration::from_secs(5),
    )
    .await;
    client
        .wait()
        .for_element(Locator::Css("[data-detail] canvas"))
        .await
        .unwrap();
    let text = client
        .find(Locator::Css("[data-detail]"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(text.contains("review correct synthetic-proposal --revision 3"));
    assert!(!text.contains(AUTHORITY) && !text.contains("Forgejo"));
    assert!(!text.contains("Confirm date") && !text.contains("Request correction"));
    assert!(!text.contains("after the next sync"));
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| !r.url.path().ends_with("/evidence/1")));
    capture(&client, "clerk-correction.png").await;
    client.close().await.unwrap();
}

#[tokio::test]
async fn lawyer_checks_highlighted_record_and_authority_before_confirming_one_revision() {
    let server=fixture(false,json!([
        {"id":"order","status":"pending","revision":3,"kind":"signed_order","title":"Status hearing","timezone":"America/New_York",
         "due_at":"2030-07-16T10:00:00-04:00","category":"hearing","hearing":{"duration_minutes":60,"location":"Courtroom 2"},"evidence":[page("signed_order")]},
        {"id":"response","status":"pending","revision":3,"kind":"calculated","title":"Response deadline","timezone":"America/New_York","due_on":"2030-07-16",
         "calculation":{"trigger":"Service","trigger_on":"2030-07-02","offset_days":14,"counting_basis":"calendar_days"},"evidence":[page("trigger"),page("authority")]}
    ])).await;
    let Some(client) = new_client_for_fixture_or_skip(&server.uri()).await else {
        return;
    };
    for (id, index, title, text) in [
        (
            "order",
            0,
            "Signed scheduling order",
            "The status hearing is set for July 16, 2030 at 10:00 a.m. Eastern.",
        ),
        (
            "response",
            0,
            "Certificate of service",
            "The motion was served by hand on July 2, 2030.",
        ),
        (
            "response",
            1,
            AUTHORITY,
            "A response is due 14 calendar days after service of the motion.",
        ),
    ] {
        source(&client, &server, id, index, title, text).await;
    }
    Mock::given(method("POST"))
        .and(path(format!("{API}/response/confirm")))
        .and(wiremock::matchers::body_json(json!({"revision":3})))
        .and(wiremock::matchers::header(
            "x-csrf-token",
            "synthetic-token",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id":"response","revision":3,"status":"approved"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    client.goto(&server.uri()).await.unwrap();
    client
        .wait()
        .for_element(Locator::Css("[data-detail] canvas"))
        .await
        .unwrap();
    wait_for_text(
        &client,
        "Confirm signed order and date",
        Duration::from_secs(5),
    )
    .await;
    capture(&client, "lawyer-signed-order.png").await;
    assert_confirm_explains_calendar_sync(&client).await;
    click(&client, ".court-review__row:nth-child(2)").await;
    client
        .wait()
        .for_element(Locator::Css("[data-detail] section:nth-of-type(2) canvas"))
        .await
        .unwrap();
    let citation = client
        .find(Locator::Css("[data-detail] section:nth-of-type(2) h4"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(
        citation,
        format!("Governing authority · {AUTHORITY}, page 1")
    );
    assert!(client
        .find_all(Locator::Css("a[href*='revision=2']"))
        .await
        .unwrap()
        .is_empty());
    capture(&client, "lawyer-calculated-date.png").await;
    click(&client, "button[aria-label*='Governing authority']").await;
    client
        .wait()
        .for_element(Locator::Css("dialog[open] canvas"))
        .await
        .unwrap();
    assert_authority_repository_link(&client).await;
    click(&client, "dialog button").await;
    click(&client, "[data-detail] .nav-btn--primary").await;
    wait_for_text(&client, "Date confirmed.", Duration::from_secs(5)).await;
    client.close().await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn design_gallery_citation_opens_the_original_without_a_repository_link() {
    let server = mounted(
        VirtualDom::new(webapp::design::CiteCheckExample),
        "Navigator design system",
    )
    .await;
    Mock::given(path("/public/design/sample-complaint-page.png"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            include_bytes!("../../public/design/sample-complaint-page.png").as_slice(),
            "image/png",
        ))
        .mount(&server)
        .await;
    let Some(client) = new_client_for_fixture_or_skip(&server.uri()).await else {
        return;
    };
    client.goto(&server.uri()).await.unwrap();
    let crop = client
        .wait()
        .for_element(Locator::Css(
            "#design-cite-check nav-citation-highlight canvas",
        ))
        .await
        .unwrap();
    assert!(
        crop.attr("height")
            .await
            .unwrap()
            .unwrap()
            .parse::<u32>()
            .unwrap()
            < 1010
    );
    let button = "#design-cite-check button[aria-haspopup='dialog']";
    click(&client, button).await;
    let full = client
        .wait()
        .for_element(Locator::Css("dialog[open] canvas"))
        .await
        .unwrap();
    assert_eq!(full.attr("height").await.unwrap().as_deref(), Some("1010"));
    assert!(client
        .find_all(Locator::Css("a[href*='/forgejo']"))
        .await
        .unwrap()
        .is_empty());
    click(&client, "dialog button").await;
    assert_eq!(
        client
            .execute(
                "return document.activeElement.matches(arguments[0])",
                vec![json!(button)]
            )
            .await
            .unwrap(),
        json!(true)
    );
    capture(&client, "design-citation-highlight.png").await;
    client.close().await.unwrap();
}
