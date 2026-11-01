//! SSH public keys for a Firm user's Forgejo account (ENG-1219).
//!
//! `navigator site person ssh-key add|list|remove|sync` manage the logged-in
//! user's own keys through the deployment's `/app/api/ssh-keys` doors; the
//! server resolves the Person from the session and reconciles their Forgejo
//! account after each change. A key is checked locally before it leaves this
//! machine, so a private key pasted by mistake is refused here and never
//! sent.
//!
//! `navigator ops forgejo ssh-keys sync` is the operator sweep: it reads the
//! stores directly and reconciles every Person holding a key row (live, or
//! revoked and awaiting removal from the forge), reporting drift by Person id
//! and fingerprint. Drift — a key on the account Navigator neither holds nor
//! manages — is reported and never removed, and the sweep exits nonzero
//! while any remains or any account failed.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{anyhow, bail, Context, Result};
use cloud::forgejo_user_keys::ForgejoUserKeys;
use serde_json::Value;
use store::ssh_public_keys::{self as keys, SyncOutcome};

/// Read a public key from a `.pub` file, or stdin for `-`.
fn read_key(source: &Path) -> Result<String> {
    if source.as_os_str() == "-" {
        return std::io::read_to_string(std::io::stdin()).context("read the key from stdin");
    }
    std::fs::read_to_string(source).with_context(|| format!("read {}", source.display()))
}

/// The door's `message`, else the first line of the body.
fn refusal_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            body.lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("(empty response)")
                .chars()
                .take(200)
                .collect()
        })
}

async fn send(request: reqwest::RequestBuilder, what: &str) -> Result<Value> {
    let response = request.send().await.with_context(|| what.to_string())?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("{what} refused: {status}: {}", refusal_message(&body));
    }
    serde_json::from_str(&body).with_context(|| format!("read the reply to {what}"))
}

/// One line describing a reconciliation outcome.
fn describe_sync(sync: &Value) -> String {
    match sync["status"].as_str() {
        Some("synced") => {
            let count = |field: &str| sync[field].as_array().map_or(0, Vec::len);
            let drift = match count("drift") {
                0 => String::new(),
                n => format!("; {n} key(s) on the account are not managed by Navigator"),
            };
            format!(
                "Forgejo account {}: {} added, {} removed{drift}",
                sync["forgejo_login"].as_str().unwrap_or("?"),
                count("added"),
                count("removed"),
            )
        }
        Some("no_forgejo_account") => {
            "no Forgejo account yet; the key is added after your first Forgejo sign-in \
             (then run `navigator site person ssh-key sync`)"
                .to_string()
        }
        Some("not_configured") => "this deployment does not sync keys to Forgejo".to_string(),
        _ => format!(
            "the Forgejo sync failed ({}); the key change stands and is retried by the operator sweep",
            sync["error"].as_str().unwrap_or("unknown error")
        ),
    }
}

fn print_key(key: &Value) {
    println!(
        "{}  {}  {}  {}",
        key["id"].as_str().unwrap_or("?"),
        key["fingerprint"].as_str().unwrap_or("?"),
        key["algorithm"].as_str().unwrap_or("?"),
        key["label"].as_str().unwrap_or(""),
    );
}

async fn add_at(base: &str, token: &str, key: &str, label: Option<&str>) -> Result<()> {
    // Refuse a private key, or anything else malformed, before it is sent.
    let parsed = keys::parse(key).map_err(|error| anyhow!("{error}"))?;
    let mut body = serde_json::json!({ "public_key": parsed.public_key });
    if let Some(label) = label.or(parsed.comment.as_deref()) {
        body["label"] = Value::from(label);
    }
    let reply = send(
        reqwest::Client::new()
            .post(format!("{base}/app/api/ssh-keys"))
            .bearer_auth(token)
            .json(&body),
        "POST /app/api/ssh-keys",
    )
    .await?;
    print_key(&reply["key"]);
    println!("{}", describe_sync(&reply["sync"]));
    Ok(())
}

async fn list_at(base: &str, token: &str, person: Option<uuid::Uuid>) -> Result<()> {
    let mut request = reqwest::Client::new()
        .get(format!("{base}/app/api/ssh-keys"))
        .bearer_auth(token);
    if let Some(person) = person {
        request = request.query(&[("person", person.to_string())]);
    }
    let reply = send(request, "GET /app/api/ssh-keys").await?;
    let listed = reply["keys"].as_array().cloned().unwrap_or_default();
    if listed.is_empty() {
        println!("no SSH keys");
    }
    for key in &listed {
        print_key(key);
    }
    Ok(())
}

async fn remove_at(base: &str, token: &str, id: uuid::Uuid) -> Result<()> {
    let reply = send(
        reqwest::Client::new()
            .delete(format!("{base}/app/api/ssh-keys/{id}"))
            .bearer_auth(token),
        "DELETE /app/api/ssh-keys",
    )
    .await?;
    println!(
        "removed {}",
        reply["key"]["fingerprint"].as_str().unwrap_or("the key")
    );
    println!("{}", describe_sync(&reply["sync"]));
    Ok(())
}

async fn sync_at(base: &str, token: &str) -> Result<()> {
    let reply = send(
        reqwest::Client::new()
            .post(format!("{base}/app/api/ssh-keys/sync"))
            .bearer_auth(token),
        "POST /app/api/ssh-keys/sync",
    )
    .await?;
    println!("{}", describe_sync(&reply["sync"]));
    Ok(())
}

fn finish(result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::from(2)
        }
    }
}

/// `navigator site person ssh-key add`.
pub async fn add(host: Option<String>, source: &Path, label: Option<&str>) -> ExitCode {
    finish(
        async {
            let key = read_key(source)?;
            let (base, token) = crate::remote::resolve(host.as_deref())?;
            add_at(&base, &token, &key, label).await
        }
        .await,
    )
}

/// `navigator site person ssh-key list`.
pub async fn list(host: Option<String>, person: Option<uuid::Uuid>) -> ExitCode {
    finish(
        async {
            let (base, token) = crate::remote::resolve(host.as_deref())?;
            list_at(&base, &token, person).await
        }
        .await,
    )
}

/// `navigator site person ssh-key remove`.
pub async fn remove(host: Option<String>, id: uuid::Uuid) -> ExitCode {
    finish(
        async {
            let (base, token) = crate::remote::resolve(host.as_deref())?;
            remove_at(&base, &token, id).await
        }
        .await,
    )
}

/// `navigator site person ssh-key sync`.
pub async fn sync(host: Option<String>) -> ExitCode {
    finish(
        async {
            let (base, token) = crate::remote::resolve(host.as_deref())?;
            sync_at(&base, &token).await
        }
        .await,
    )
}

/// `navigator ops forgejo ssh-keys sync`: reconcile every Person holding a
/// key row, or just `person`.
///
/// # Errors
/// Missing configuration, store failures, and — after every account has been
/// tried — any account that failed or holds drift.
pub fn sweep(person: Option<&str>, dry_run: bool) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("start the async runtime")?;
    runtime.block_on(async {
        let client = ForgejoUserKeys::from_env()?.ok_or_else(|| {
            anyhow!(
                "set {}, {} and {} to run the key sync",
                cloud::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV,
                cloud::forgejo_user_keys::NAVIGATOR_FORGEJO_KEY_SYNC_LOGIN_ENV,
                cloud::forgejo_user_keys::NAVIGATOR_FORGEJO_KEY_SYNC_TOKEN_ENV,
            )
        })?;
        let surreal = store::surreal::connect_from_env()
            .await
            .map_err(|error| anyhow!("surreal: {error}"))?;
        let pg = store::postgres::PostgresDb::from_env()
            .await
            .map_err(|error| anyhow!("{error}"))?;
        let people = match person {
            Some(spec) => vec![resolve_person(&surreal, spec).await?],
            None => keys::person_ids_with_keys(&pg)
                .await
                .map_err(|error| anyhow!("{error}"))?,
        };
        let mut drifted = 0usize;
        let mut failed = 0usize;
        for person_id in &people {
            match keys::sync_person(&surreal, &pg, Some(&client), *person_id, dry_run).await {
                Ok(SyncOutcome::Synced(report)) => {
                    let verb = if dry_run { "would add" } else { "added" };
                    println!(
                        "{person_id}: {verb} {}, {} {}, drift {}",
                        report.added.len(),
                        if dry_run { "would remove" } else { "removed" },
                        report.removed.len(),
                        report.drift.len()
                    );
                    for fingerprint in &report.drift {
                        println!("{person_id}: drift {fingerprint} (not managed by Navigator; left in place)");
                    }
                    if !report.drift.is_empty() {
                        drifted += 1;
                    }
                }
                Ok(SyncOutcome::NoForgejoAccount) => {
                    println!("{person_id}: no Forgejo account yet");
                }
                Ok(SyncOutcome::NotConfigured) => unreachable!("the client is configured"),
                Err(error) => {
                    failed += 1;
                    eprintln!("{person_id}: {error}");
                }
            }
        }
        println!(
            "{} Person(s) checked; {drifted} with drift; {failed} failed",
            people.len()
        );
        if drifted > 0 || failed > 0 {
            bail!("{drifted} account(s) hold drift and {failed} failed; inspect and resolve by hand");
        }
        Ok(())
    })
}

async fn resolve_person(surreal: &store::surreal::SurrealDb, spec: &str) -> Result<uuid::Uuid> {
    if let Ok(id) = spec.parse::<uuid::Uuid>() {
        return Ok(id);
    }
    store::persons::find_by_any_email(surreal, spec)
        .await
        .map_err(|error| anyhow!("{error}"))?
        .map(|person| person.id)
        .ok_or_else(|| anyhow!("no Person answers to that email"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ED25519: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO+kywI2x0e1JcY7eJ50pKflZgKO0UKaS+cV8ssit45e synthetic@example.test";

    #[tokio::test]
    async fn add_sends_only_the_canonical_public_line_with_the_comment_as_label() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/api/ssh-keys"))
            .and(header("authorization", "Bearer synthetic-token"))
            .and(body_json(json!({
                "public_key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO+kywI2x0e1JcY7eJ50pKflZgKO0UKaS+cV8ssit45e",
                "label": "synthetic@example.test",
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "key": {"id": "k1", "fingerprint": "SHA256:x", "algorithm": "ssh-ed25519", "label": "l"},
                "sync": {"status": "not_configured"},
            })))
            .expect(1)
            .mount(&server)
            .await;
        add_at(&server.uri(), "synthetic-token", ED25519, None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_private_key_is_refused_before_any_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&server)
            .await;
        let error = add_at(
            &server.uri(),
            "synthetic-token",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n",
            None,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("private key"), "{error}");
    }

    #[tokio::test]
    async fn list_for_another_person_names_them_in_the_query() {
        let server = MockServer::start().await;
        let person = uuid::Uuid::nil();
        Mock::given(method("GET"))
            .and(path("/app/api/ssh-keys"))
            .and(query_param("person", person.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"keys": []})))
            .expect(1)
            .mount(&server)
            .await;
        list_at(&server.uri(), "synthetic-token", Some(person))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_refusal_surfaces_the_doors_message() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "error": "not_found", "message": "the SSH public key was not found"
            })))
            .mount(&server)
            .await;
        let error = remove_at(&server.uri(), "synthetic-token", uuid::Uuid::nil())
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("the SSH public key was not found"),
            "{error}"
        );
    }

    #[test]
    fn sync_outcomes_read_plainly() {
        assert!(describe_sync(&json!({
            "status": "synced", "forgejo_login": "synthetic-lawyer",
            "added": ["a"], "removed": [], "drift": ["b"], "dry_run": false
        }))
        .contains("1 added, 0 removed; 1 key(s) on the account are not managed"));
        assert!(describe_sync(&json!({"status": "no_forgejo_account"}))
            .contains("first Forgejo sign-in"));
        assert!(describe_sync(&json!({"status": "failed", "error": "boom"})).contains("boom"));
    }
}
