//! Authenticated operator commands for a Firm's assumptions: the stated
//! premises the CEO Digest checks actuals against. Keys come from the
//! repository's public catalog; the values are Firm data in PostgreSQL.
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::process::ExitCode;
use uuid::Uuid;

#[derive(Subcommand)]
pub(crate) enum AssumptionsCmd {
    /// The Firm's current assumptions: one row per known key it has set.
    List,
    /// The newest version of one key, or a blank result when never set.
    Show {
        /// The dotted-path key, for example `plan.target_minutes.document_review`.
        key: String,
    },
    /// Record a new version of one key; history is the table.
    Set {
        /// The dotted-path key, for example `plan.target_minutes.document_review`.
        key: String,
        /// The JSON value, for example `45` or `40.5` — checked against the
        /// key's catalog type.
        #[arg(long)]
        value: String,
        /// Why the value changed; recorded with the version.
        #[arg(long)]
        reason: String,
    },
    /// Every version of one key, newest first.
    History {
        /// The dotted-path key, for example `plan.target_minutes.document_review`.
        key: String,
    },
    /// The public catalog of known keys with their types and null defaults.
    Catalog,
}

pub(crate) async fn run(firm: Uuid, host: Option<String>, action: AssumptionsCmd) -> ExitCode {
    match execute(firm, host, action).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("navigator: assumptions: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn call(
    base: &str,
    token: &str,
    method: reqwest::Method,
    route: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    for attempt in 0..3u32 {
        let mut request = client
            .request(method.clone(), format!("{base}{route}"))
            .bearer_auth(token);
        if let Some(body) = body {
            request = request.json(body);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => {
                return response.json().await.context("decode assumptions response");
            }
            Ok(response)
                if response.status().is_server_error()
                    || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS =>
            {
                if attempt == 2 {
                    bail!(
                        "assumptions service unavailable after bounded retries ({})",
                        response.status()
                    );
                }
            }
            Ok(response) => bail!("assumptions command refused ({})", response.status()),
            Err(_) if attempt == 2 => {
                bail!("assumptions command transport unavailable after bounded retries")
            }
            Err(_) => {}
        }
        tokio::time::sleep(std::time::Duration::from_secs(1u64 << attempt)).await;
    }
    bail!("assumptions retry limit reached")
}

async fn execute(firm: Uuid, host: Option<String>, action: AssumptionsCmd) -> Result<()> {
    let (base, token) = crate::remote::resolve(host.as_deref())?;
    execute_at(firm, &base, &token, action).await
}

async fn execute_at(firm: Uuid, base: &str, token: &str, action: AssumptionsCmd) -> Result<()> {
    let route = format!("/app/api/firms/{firm}/assumptions");
    match action {
        AssumptionsCmd::List => println!(
            "{}",
            serde_json::to_string_pretty(
                &call(base, token, reqwest::Method::GET, &route, None).await?
            )?
        ),
        AssumptionsCmd::Show { key } => println!(
            "{}",
            serde_json::to_string_pretty(
                &call(
                    base,
                    token,
                    reqwest::Method::GET,
                    &format!("{route}/{key}"),
                    None
                )
                .await?
            )?
        ),
        AssumptionsCmd::Set { key, value, reason } => {
            let parsed: serde_json::Value =
                serde_json::from_str(&value).context("the --value must be one JSON value")?;
            let body = serde_json::json!({"value": parsed, "reason": reason});
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &call(
                        base,
                        token,
                        reqwest::Method::POST,
                        &format!("{route}/{key}"),
                        Some(&body)
                    )
                    .await?
                )?
            );
        }
        AssumptionsCmd::History { key } => println!(
            "{}",
            serde_json::to_string_pretty(
                &call(
                    base,
                    token,
                    reqwest::Method::GET,
                    &format!("{route}/{key}/history"),
                    None
                )
                .await?
            )?
        ),
        AssumptionsCmd::Catalog => println!(
            "{}",
            serde_json::to_string_pretty(
                &call(
                    base,
                    token,
                    reqwest::Method::GET,
                    "/app/api/assumptions",
                    None
                )
                .await?
            )?
        ),
    }
    Ok(())
}
