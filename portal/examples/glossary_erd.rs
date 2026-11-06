//! Export the full glossary diagram with the repository's pinned Mermaid.
//! Run against a local `ChromeDriver`: `cargo run -p portal --example glossary_erd`.

use anyhow::{Context, Result};
use serde_json::{json, Value};

const SOURCE: &str = include_str!("../../store/postgres/postgres-erd.mmd");
const MERMAID: &str = include_str!("../../server/public/vendor/mermaid/mermaid.min.js");

#[tokio::main]
async fn main() -> Result<()> {
    let driver = std::env::var("WEBDRIVER_URL").unwrap_or_else(|_| "http://127.0.0.1:9515".into());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let mut options = json!({"args": ["--headless=new", "--disable-gpu", "--no-sandbox"]});
    if let Ok(binary) = std::env::var("CHROME_BINARY") {
        options["binary"] = binary.into();
    }
    let response: Value = client.post(format!("{driver}/session"))
        .json(&json!({"capabilities": {"alwaysMatch": {"browserName": "chrome", "goog:chromeOptions": options}}}))
        .send().await?.error_for_status()?.json().await?;
    let id = response["value"]["sessionId"]
        .as_str()
        .context("ChromeDriver session id")?;
    let session = format!("{driver}/session/{id}");
    let rendered = render(&client, &session).await;
    // Always release the task-owned browser, including a rendering failure.
    client.delete(&session).send().await?.error_for_status()?;
    let svg = rendered?;
    let digest = store::assets::sha256_hex(SOURCE.as_bytes());
    let svg = svg.replacen(
        "<svg ",
        &format!("<svg data-source-sha256=\"{digest}\" "),
        1,
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../server/public/glossary/postgres/ERD.svg");
    std::fs::create_dir_all(path.parent().context("SVG output directory")?)?;
    std::fs::write(&path, svg)?;
    println!("{}", path.display());
    Ok(())
}

async fn render(client: &reqwest::Client, session: &str) -> Result<String> {
    for (path, body) in [
        ("timeouts", json!({"script": 120_000})),
        ("url", json!({"url": "about:blank"})),
        (
            "execute/sync",
            json!({"script": "const script = document.createElement('script'); script.textContent = arguments[0]; document.head.append(script);", "args": [MERMAID]}),
        ),
    ] {
        client
            .post(format!("{session}/{path}"))
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
    }
    let response: Value = client.post(format!("{session}/execute/async"))
        .json(&json!({
            "script": "const source = arguments[0], done = arguments[arguments.length - 1]; mermaid.initialize({startOnLoad:false,securityLevel:'strict',maxTextSize:200000,maxEdges:2000,htmlLabels:false,deterministicIds:true,deterministicIDSeed:'navigator-postgres',er:{useMaxWidth:false},theme:'neutral'}); mermaid.render('navigatorPostgres', source).then(result => done({svg: result.svg})).catch(() => done({error: 'Mermaid rendering failed'}));",
            "args": [SOURCE]
        }))
        .send().await?.error_for_status()?.json().await?;
    response["value"]["svg"]
        .as_str()
        .map(str::to_owned)
        .context("Mermaid did not return an SVG")
}
