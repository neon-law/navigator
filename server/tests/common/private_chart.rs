//! Synthetic, request-local private chart browser behavior.

use std::time::{Duration, Instant};

use fantoccini::{Client, Locator};
use features::webdriver::{base_url, login_as_client, new_client_or_skip};
use uuid::Uuid;

async fn fixture() -> String {
    let db = store::surreal::connect_from_env()
        .await
        .expect("task store");
    let client = store::persons::find_by_email_ci(&db, "client@neonlaw.com")
        .await
        .expect("fixture client lookup")
        .expect("fixture client");
    let project = store::projects::create(
        &db,
        &store::projects::NewProject {
            code: format!("chart-browser-{}", Uuid::now_v7()),
            name: "Synthetic reflection".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&db).await,
            ..Default::default()
        },
    )
    .await
    .expect("synthetic Project");
    store::projects::add_participation(&db, project.id, client.id, "client")
        .await
        .expect("canonical client participation");
    let pages: Vec<rules::pages::PageEntry> = serde_json::from_value(serde_json::json!([
        {"slug":"reflection","kind":"estate_plan_console","title":"Personal reflection","lenses":{
            "client":["instrument_set","astrology_chart","boundary_note","provenance_statement"],
            "lawyer":["instrument_set","astrology_chart","boundary_note","provenance_statement"]}},
        {"slug":"instruments","kind":"estate_plan_console","title":"Instruments","lenses":{
            "client":["instrument_set","boundary_note","provenance_statement"]}}
    ]))
    .expect("declared pages");
    store::matter_pages::replace_for_project(&db, project.id, &pages)
        .await
        .expect("validated pages");
    project.code
}

async fn hydrated(c: &Client) {
    let start = Instant::now();
    loop {
        if c.find(Locator::Css(
            "main[data-matter-page-root][data-hydrated='true']",
        ))
        .await
        .is_ok()
        {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the private SSR root must hydrate using its matching MatterPage entry point"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn private_chart_hydrates_its_authorized_matter_page() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    let cleanup = c.clone();
    let outcome = tokio::spawn(async move { check_hydration(&c).await }).await;
    cleanup
        .close()
        .await
        .expect("close own browser even on failure");
    outcome.expect("private Matter Page browser assertions");
}

async fn check_hydration(c: &Client) {
    login_as_client(c).await;
    let code = fixture().await;
    c.goto(&format!("{}/app/projects/{code}/reflection", base_url()))
        .await
        .expect("private page");
    let html = c.source().await.expect("authorized SSR document");
    assert!(
        html.contains("data-private-chart=\"true\""),
        "the fixture supplies the opted-in authorized form"
    );
    assert!(html.contains("method=\"post\""));
    let start = Instant::now();
    while !c.execute("return performance.getEntriesByType('resource').some(entry => entry.name.includes('.wasm') && entry.responseEnd > 0)", vec![]).await.expect("WASM resource timing").as_bool().unwrap_or(false) {
        assert!(start.elapsed() < Duration::from_secs(30), "the current WASM bundle must load before diagnosing hydration");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    hydrated(c).await;
    assert!(c
        .find(Locator::Css(
            "[data-private-chart] input[name='birth-date']"
        ))
        .await
        .expect("birth fields")
        .is_enabled()
        .await
        .unwrap());
    for (slug, expected) in [
        ("instruments", "Instruments"),
        ("not-declared", "Not found"),
    ] {
        c.goto(&format!("{}/app/projects/{code}/{slug}", base_url()))
            .await
            .expect("other private page");
        hydrated(c).await;
        assert!(c
            .find(Locator::Css("main[data-matter-page-root] h1"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
            .contains(expected));
        assert!(
            c.find(Locator::Css("[data-private-chart]")).await.is_err(),
            "non-chart and absent pages never inherit personal controls"
        );
    }
    c.goto(&format!("{}/glossary", base_url()))
        .await
        .expect("public glossary");
    let start = Instant::now();
    loop {
        if c.find(Locator::Css("#matter-page")).await.is_ok() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "glossary contains the Matter Page term"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        c.find(Locator::Css("main[data-matter-page-root]"))
            .await
            .is_err(),
        "a public glossary term ID is never the private hydration root"
    );
}

async fn set_input(c: &Client, name: &str, value: &str) {
    let css = format!("[data-private-chart] [name='{name}']");
    c.wait()
        .at_most(Duration::from_secs(5))
        .for_element(Locator::Css(&css))
        .await
        .expect("private control");
    c.execute("const input=document.querySelector(arguments[0]); const prototype=input.tagName==='SELECT'?HTMLSelectElement.prototype:HTMLInputElement.prototype; Object.getOwnPropertyDescriptor(prototype,'value').set.call(input,arguments[1]); input.dispatchEvent(new Event('input',{bubbles:true}));", vec![css.into(), value.into()]).await.expect("edit synthetic input");
}

async fn fill(c: &Client, precision: &str) {
    set_input(c, "precision", precision).await;
    for (name, value) in [
        ("birth-date", "2000-01-01"),
        ("birth-zone", "UTC"),
        ("birth-longitude", "0"),
        ("birth-latitude", "0"),
    ] {
        set_input(c, name, value).await;
    }
    if precision != "unknown" {
        set_input(c, "birth-time", "12:00:00").await;
    }
    if precision == "approximate" {
        set_input(c, "birth-uncertainty", "60").await;
    }
    if precision == "exact" {
        set_input(c, "houses", "equal").await;
    }
    features::webdriver::scroll_and_js_click(c, "[data-private-chart] [name=consent]").await;
}

async fn submit(c: &Client) {
    features::webdriver::scroll_and_js_click(c, "[data-private-chart] button[type=submit]").await;
}

async fn wait_flag(c: &Client, script: &str) {
    let start = Instant::now();
    while !c
        .execute(script, vec![])
        .await
        .expect("private lifecycle checkpoint")
        .as_bool()
        .unwrap_or(false)
    {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "private lifecycle checkpoint did not complete: {script}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn calculated(c: &Client) -> bool {
    wait_flag(c, "return !!document.querySelector('[data-private-result], [data-private-chart] [role=alert]')").await;
    let success = c.find(Locator::Css("[data-private-result]")).await.is_ok();
    if !success {
        assert_eq!(
            c.execute("return window.__chartHttpStatus", vec![])
                .await
                .unwrap(),
            503
        );
        assert!(c
            .find(Locator::Css("[data-private-chart] [role=alert]"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
            .contains("Verified calculation sources are unavailable"));
        assert!(
            !std::env::var("NAV_REQUIRE_PRIVATE_CHART_SOURCES").is_ok_and(|value| value == "1"),
            "strict local browser lane requires actual verified JPL calculation success"
        );
    }
    success
}

async fn clear(c: &Client) {
    features::webdriver::scroll_and_js_click(c, "[data-private-clear]").await;
    wait_flag(c, "return !document.querySelector('[data-private-result]') && document.querySelector('[name=birth-date]').value==='' && !document.querySelector('[name=consent]').checked").await;
}

async fn track_requests(c: &Client) {
    c.execute(
        r"
        const nativeFetch=window.fetch.bind(window);
        const nativeSetItem=Storage.prototype.setItem;
        window.__chartStorageWrites=0;
        window.__chartStorageLengths=[localStorage.length,sessionStorage.length];
        Storage.prototype.setItem=function(...args){window.__chartStorageWrites++;return nativeSetItem.apply(this,args)};
        window.__chartRequests=0; window.__chartNoQuery=true;
        window.fetch=(...args)=>{
            const request=args[0];
            const url=new URL(typeof request==='string'?request:request.url,location.href);
            if(!url.pathname.endsWith('/astrology-chart')) return nativeFetch(...args);
            window.__chartRequests++; window.__chartNoQuery=window.__chartNoQuery&&!url.search;
            const signal=request.signal||args[1]?.signal;
            signal?.addEventListener('abort',()=>window.__chartAborted=true,{once:true});
            return nativeFetch(...args).then(response=>{
                window.__chartHttpStatus=response.status;
                if(!window.__chartDelay) return response;
                return new Promise(resolve=>{
                    window.__chartWaiting=true;
                    window.__chartRelease=()=>resolve(response);
                });
            });
        };
    ",
        vec![],
    )
    .await
    .expect("test-only genuine response timing");
}

#[tokio::test]
async fn private_chart_calculation_and_page_lifetime_are_request_local() {
    let Some(c) = new_client_or_skip().await else {
        return;
    };
    let cleanup = c.clone();
    let outcome = tokio::spawn(async move { check_lifetime(&c).await }).await;
    cleanup
        .close()
        .await
        .expect("close own browser even on failure");
    outcome.expect("private chart lifetime browser assertions");
}

async fn check_lifetime(c: &Client) {
    login_as_client(c).await;
    let code = fixture().await;
    c.goto(&format!("{}/app/projects/{code}/reflection", base_url()))
        .await
        .unwrap();
    hydrated(c).await;
    if std::env::var_os("NAV_PRIVATE_CHART_CAPTURE_DIR").is_some() {
        super::set_practice_viewport(c, 1280, 1800).await;
    }
    capture(c, "01-blank", "[data-private-chart]").await;
    track_requests(c).await;
    fill(c, "exact").await;
    capture(c, "02-inputs", "[data-private-chart]").await;
    submit(c).await;
    if calculated(c).await {
        assert!(c
            .find(Locator::Css("[data-private-result] svg"))
            .await
            .is_ok());
        // Independent retained JPL reference: exact UTC noon at J2000, geocentric tropical output.
        for (body, longitude, rate) in [
            ("Sun", "280.4° longitude", "1.0194"),
            ("Moon", "223.3° longitude", "12.0213"),
        ] {
            let row = c
                .find(Locator::XPath(&format!(
                    "//div[@data-private-result]//tr[th[contains(.,'{body}')]]"
                )))
                .await
                .expect("selected body row")
                .text()
                .await
                .unwrap();
            assert!(
                row.contains(longitude) && row.contains(rate),
                "actual calculated table agrees with pinned source reference"
            );
        }
        assert_eq!(
            c.find_all(Locator::Css("[data-private-result] svg g[data-house]"))
                .await
                .unwrap()
                .len(),
            12
        );
        capture(c, "03-wheel", "#astrology-chart svg").await;
        capture(c, "04-table", "#astrology-chart table").await;
        select_moon_control(c).await;
        set_input(c, "birth-zone", "Etc/UTC").await;
        wait_flag(c, "return !document.querySelector('[data-private-result]')").await;
    }
    clear(c).await;
    capture(c, "05-cleared", "[data-private-chart]").await;
    for precision in ["approximate", "unknown"] {
        fill(c, precision).await;
        submit(c).await;
        if calculated(c).await {
            assert!(c
                .find_all(Locator::Css("[data-private-result] svg g[data-house]"))
                .await
                .unwrap()
                .is_empty());
            assert_eq!(
                c.find(Locator::Css("[data-private-result] svg"))
                    .await
                    .is_ok(),
                precision != "unknown"
            );
        }
        clear(c).await;
    }
    delayed_clear(c).await;
    pagehide_and_restore(c, &code).await;
}

async fn select_moon_control(c: &Client) {
    let moon = c
        .find(Locator::XPath(
            "//section[@id='astrology-chart']//button[contains(.,'Moon')]",
        ))
        .await
        .expect("calculated Moon control");
    assert_eq!(
        moon.attr("aria-pressed").await.unwrap().as_deref(),
        Some("false")
    );
    assert!(moon.is_enabled().await.unwrap());
    c.execute("[...document.querySelectorAll('#astrology-chart button')].find(button=>button.textContent.includes('Moon')).focus();",vec![]).await.unwrap();
    assert_eq!(
        c.execute(
            "return document.activeElement.textContent.includes('Moon')",
            vec![]
        )
        .await
        .unwrap(),
        true
    );
    assert_eq!(moon.tag_name().await.unwrap(), "button");
    assert_eq!(moon.attr("type").await.unwrap().as_deref(), Some("button"));
    features::webdriver::scroll_and_js_click(
        c,
        "#astrology-chart .astrology-chart__controls button:nth-child(2)",
    )
    .await;
    wait_flag(c,"return [...document.querySelectorAll('#astrology-chart button')].some(button=>button.textContent.includes('Moon')&&button.getAttribute('aria-pressed')==='true')").await;
    assert!(c
        .find(Locator::Css("#astrology-chart [aria-live] strong"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
        .contains("Moon"));
}

async fn delayed_clear(c: &Client) {
    fill(c, "exact").await;
    c.execute(
        "window.__chartDelay=true; window.__chartWaiting=false; window.__chartAborted=false;",
        vec![],
    )
    .await
    .unwrap();
    submit(c).await;
    wait_flag(c, "return window.__chartWaiting===true").await;
    clear(c).await;
    assert_eq!(
        c.execute("return window.__chartAborted", vec![])
            .await
            .unwrap(),
        true
    );
    c.execute(
        "window.__chartRelease(); window.__chartDelay=false;",
        vec![],
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        c.find(Locator::Css(
            "[data-private-result], [data-private-chart] [role=alert]"
        ))
        .await
        .is_err(),
        "late genuine response cannot restore cleared result or error"
    );
    assert_eq!(
        c.execute("return window.__chartNoQuery", vec![])
            .await
            .unwrap(),
        true
    );
    assert_eq!(c.execute("return window.__chartStorageWrites===0 && localStorage.length===window.__chartStorageLengths[0] && sessionStorage.length===window.__chartStorageLengths[1]",vec![]).await.unwrap(),true,"private inputs, calculated results and clears do not write browser storage");
}

async fn pagehide_and_restore(c: &Client, code: &str) {
    fill(c, "exact").await;
    submit(c).await;
    let _ = calculated(c).await;
    let immediate=c.execute("window.dispatchEvent(new PageTransitionEvent('pagehide',{persisted:true})); const root=document.querySelector('[data-private-chart]'); return root.hidden && getComputedStyle(root).display==='none' && [...root.querySelectorAll('input[data-private-input]')].every(input=>input.type==='checkbox'?!input.checked:input.value==='');",vec![]).await.unwrap();
    assert_eq!(
        immediate, true,
        "pagehide hides and clears personal DOM synchronously before a reactive render"
    );
    c.execute(
        "window.dispatchEvent(new PageTransitionEvent('pageshow',{persisted:true}));",
        vec![],
    )
    .await
    .unwrap();
    hydrated(c).await;
    wait_flag(c,"return document.querySelector('[data-private-chart]')?.hidden===false && document.querySelector('[name=birth-date]').value===''").await;
    let other = fixture().await;
    track_requests(c).await;
    fill(c, "exact").await;
    c.execute(
        "window.__chartDelay=true; window.__chartWaiting=false;",
        vec![],
    )
    .await
    .unwrap();
    submit(c).await;
    wait_flag(c, "return window.__chartWaiting===true").await;
    c.execute("setTimeout(()=>window.__chartRelease(),2000)", vec![])
        .await
        .unwrap();
    c.goto(&format!("{}/app/projects/{other}/reflection", base_url()))
        .await
        .unwrap();
    hydrated(c).await;
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(
        c.find(Locator::Css("[data-private-result]")).await.is_err(),
        "a late response from the prior Project never populates the new Project"
    );
    assert_eq!(
        c.find(Locator::Css("[name='birth-date']"))
            .await
            .unwrap()
            .prop("value")
            .await
            .unwrap()
            .as_deref(),
        Some("")
    );
    c.back().await.unwrap();
    hydrated(c).await;
    wait_flag(c,"return document.querySelector('[data-private-chart]')?.hidden===false && document.querySelector('[name=birth-date]').value===''").await;
    assert!(c.current_url().await.unwrap().path().contains(code));
    assert!(c.find(Locator::Css("[data-private-result]")).await.is_err());
}

async fn capture(c: &Client, name: &str, selector: &str) {
    let Some(directory) = std::env::var_os("NAV_PRIVATE_CHART_CAPTURE_DIR") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    assert!(
        directory.starts_with("/tmp") || directory.starts_with("/private/tmp"),
        "captures belong only in task temporary output"
    );
    std::fs::create_dir_all(&directory).unwrap();
    c.execute(
        "document.querySelector(arguments[0]).scrollIntoView({block:'start'})",
        vec![selector.into()],
    )
    .await
    .unwrap();
    let png = c
        .find(Locator::Css(selector))
        .await
        .expect("synthetic private section")
        .screenshot()
        .await
        .expect("actual browser screenshot");
    std::fs::write(directory.join(format!("{name}.png")), png).unwrap();
}

async fn audit_private_section(c: &Client) {
    c.execute(include_str!("../assets/axe.min.js"), vec![])
        .await
        .expect("inject pinned axe");
    let report=c.execute_async(r"
        const done=arguments[arguments.length-1];
        axe.run(document.querySelector('[data-private-chart]'),{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21a','wcag21aa']}}).then(report=>{
            const undecidable=report.incomplete.filter(result=>result.id==='color-contrast').flatMap(result=>result.nodes).filter(node=>node.any.some(check=>check.id==='color-contrast'&&check.data?.bgColor==null&&check.data?.messageKey==null));
            done({violations:report.violations.map(result=>result.id),undecidable:undecidable.length});
        }).catch(()=>done({violations:['axe-failed'],undecidable:0}));
    ",vec![]).await.expect("private section axe audit");
    assert_eq!(
        report["violations"],
        serde_json::json!([]),
        "private section meets WCAG A/AA"
    );
    assert_eq!(report["undecidable"], 0, "text has a measurable background");
    assert_eq!(c.execute("return [...document.querySelectorAll('.astrology-chart__table tbody td:nth-child(3)')].every(cell=>{const range=document.createRange();range.selectNodeContents(cell);return range.getClientRects().length===1})",vec![]).await.unwrap(),true,"motion labels remain whole readable words at the actual viewport");
    assert_eq!(
        c.execute(
            "return document.documentElement.scrollWidth<=innerWidth",
            vec![]
        )
        .await
        .unwrap(),
        true,
        "private page fits the actual viewport"
    );
}

#[tokio::test]
async fn private_chart_is_accessible_at_mobile_and_desktop_in_both_schemes() {
    use features::webdriver::{new_client_or_skip_in_scheme, ColorScheme};
    for scheme in ColorScheme::both() {
        let Some(c) = new_client_or_skip_in_scheme(scheme).await else {
            return;
        };
        let cleanup = c.clone();
        let outcome = tokio::spawn(async move { check_accessibility(&c, scheme).await }).await;
        cleanup
            .close()
            .await
            .expect("close own audit browser even on failure");
        outcome.expect("private chart accessibility assertions");
    }
}

async fn check_accessibility(c: &Client, scheme: features::webdriver::ColorScheme) {
    login_as_client(c).await;
    let code = fixture().await;
    c.goto(&format!("{}/app/projects/{code}/reflection", base_url()))
        .await
        .unwrap();
    hydrated(c).await;
    assert_eq!(
        c.execute(
            "return matchMedia('(prefers-color-scheme: dark)').matches",
            vec![]
        )
        .await
        .unwrap(),
        scheme.prefers_dark()
    );
    for (width, height) in [(1280, 900), (375, 812)] {
        super::set_practice_viewport(c, width, height).await;
        audit_private_section(c).await;
    }
    track_requests(c).await;
    fill(c, "exact").await;
    submit(c).await;
    let _ = calculated(c).await;
    for (width, height) in [(375, 812), (1280, 900)] {
        super::set_practice_viewport(c, width, height).await;
        audit_private_section(c).await;
    }
}
