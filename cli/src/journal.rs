//! Authenticated operator commands for the Firm-owned journal.
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::{path::PathBuf, process::ExitCode};
use uuid::Uuid;
#[derive(Subcommand)]
pub(crate) enum JournalCmd {
    /// Read immutable entries and their receipt/source links.
    List,
    /// Review legacy rows that need timestamp evidence before migration.
    MigrationTriage,
    /// Read durable reconciliation results and unresolved exceptions.
    Audits {
        #[arg(long)]
        job: Option<Uuid>,
    },
    /// Fail when this job has no recent successful reconciliation.
    Status {
        #[arg(long)]
        job: Uuid,
        #[arg(long, default_value_t = 600, value_parser=clap::value_parser!(u64).range(60..))]
        stale_after_seconds: u64,
    },
    /// Account and asset balances, optionally restricted to one Project's allocations.
    Statement {
        #[arg(long)]
        project: Option<Uuid>,
    },
    /// Apply every pending PostgreSQL migration through the explicitly configured
    /// operator connection, as `navigator-migrate` does.
    Migrate,
    /// Submit one balanced immutable entry; retries use its original key.
    #[command(alias = "import")]
    Post { file: PathBuf },
    /// Validate the complete posting command without persisting it.
    Preview { file: PathBuf },
    /// Preview or execute the exact approvals and scope carried in the file.
    Reconcile {
        file: PathBuf,
        /// Refresh only explicitly linked provider sources into this Project's private Documents.
        #[arg(long)]
        refresh_project: Option<Uuid>,
        /// Poll linked records at this interval. No account enumeration occurs.
        #[arg(long,value_parser=clap::value_parser!(u64).range(60..))]
        interval_seconds: Option<u64>,
        /// Print counts and health only, suitable for scheduled job logs.
        #[arg(long)]
        summary: bool,
    },
    /// Read exactly one Mercury transaction and file its private evidence.
    Mercury {
        #[arg(long)]
        project: Uuid,
        #[arg(long)]
        account: Uuid,
        #[arg(long)]
        transaction: Uuid,
    },
}
pub(crate) async fn run(firm: Uuid, host: Option<String>, action: JournalCmd) -> ExitCode {
    match execute(firm, host, action).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("navigator: journal: {e:#}");
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
                return response.json().await.context("decode journal response");
            }
            Ok(response)
                if response.status().is_server_error()
                    || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS =>
            {
                if attempt == 2 {
                    bail!(
                        "journal service unavailable after bounded retries ({})",
                        response.status()
                    );
                }
            }
            Ok(response) => bail!("journal command refused ({})", response.status()),
            Err(_) if attempt == 2 => {
                bail!("journal command transport unavailable after bounded retries")
            }
            Err(_) => {}
        }
        tokio::time::sleep(std::time::Duration::from_secs(1u64 << attempt)).await;
    }
    bail!("journal retry limit reached")
}
fn file(path: &PathBuf) -> Result<serde_json::Value> {
    if std::fs::metadata(path)?.len() > 10 * 1024 * 1024 {
        bail!("journal input exceeds 10 MiB");
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
async fn execute(firm: Uuid, host: Option<String>, action: JournalCmd) -> Result<()> {
    if matches!(&action, JournalCmd::Migrate) {
        store::postgres::PostgresDb::from_env()
            .await?
            .migrate_embedded(None)
            .await?;
        return Ok(());
    }
    let (base, token) = crate::remote::resolve(host.as_deref())?;
    execute_at(firm, &base, &token, action).await
}

#[expect(
    clippy::too_many_lines,
    reason = "command dispatch retains each operator command and polling lifecycle together"
)]
async fn execute_at(firm: Uuid, base: &str, token: &str, action: JournalCmd) -> Result<()> {
    let route = format!("/app/api/firms/{firm}/journal");
    match action {
        JournalCmd::Migrate => {}
        JournalCmd::Status {
            job,
            stale_after_seconds,
        } => {
            let audits: Vec<store::firm_journal::ReconciliationAudit> = serde_json::from_value(
                call(
                    base,
                    token,
                    reqwest::Method::GET,
                    &format!("{route}/audits?job_id={job}"),
                    None,
                )
                .await?,
            )?;
            let healthy = audit_healthy(audits.first(), chrono::Utc::now(), stale_after_seconds);
            println!("{}", serde_json::json!({"healthy":healthy,"job_id":job}));
            if !healthy {
                bail!("reconciliation is stale or needs review");
            }
        }
        JournalCmd::Audits { job } => {
            let suffix = job.map(|id| format!("?job_id={id}")).unwrap_or_default();
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &call(
                        base,
                        token,
                        reqwest::Method::GET,
                        &format!("{route}/audits{suffix}"),
                        None
                    )
                    .await?
                )?
            );
        }
        JournalCmd::MigrationTriage => println!(
            "{}",
            serde_json::to_string_pretty(
                &call(
                    base,
                    token,
                    reqwest::Method::GET,
                    &format!("{route}/migration-triage"),
                    None
                )
                .await?
            )?
        ),
        JournalCmd::Statement { project } => {
            let suffix = project
                .map(|id| format!("?project_id={id}"))
                .unwrap_or_default();
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &call(
                        base,
                        token,
                        reqwest::Method::GET,
                        &format!("{route}/statement{suffix}"),
                        None
                    )
                    .await?
                )?
            );
        }
        JournalCmd::List => println!(
            "{}",
            serde_json::to_string_pretty(
                &call(base, token, reqwest::Method::GET, &route, None).await?
            )?
        ),
        JournalCmd::Preview { file: path } => {
            let body = file(&path)?;
            let _: store::firm_journal::JournalEntry = serde_json::from_value(body.clone())?;
            println!(
                "{}",
                call(
                    base,
                    token,
                    reqwest::Method::POST,
                    &format!("{route}/preview"),
                    Some(&body)
                )
                .await?
            );
        }
        JournalCmd::Post { file: path } => {
            let body = file(&path)?;
            let _: store::firm_journal::JournalEntry = serde_json::from_value(body.clone())?;
            println!(
                "{}",
                call(base, token, reqwest::Method::POST, &route, Some(&body)).await?
            );
        }
        JournalCmd::Mercury {
            project,
            account,
            transaction,
        } => {
            let body = serde_json::json!({"project_id":project,"account_id":account,"transaction_id":transaction});
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &call(
                        base,
                        token,
                        reqwest::Method::POST,
                        &format!("{route}/mercury"),
                        Some(&body)
                    )
                    .await?
                )?
            );
        }
        JournalCmd::Reconcile {
            file: path,
            refresh_project,
            interval_seconds,
            summary,
        } => {
            if interval_seconds.is_some() && refresh_project.is_none() {
                bail!("scheduled reconciliation requires --refresh-project for fresh provider evidence");
            }
            let mut cursor = 0;
            loop {
                let mut request: store::firm_journal::ReconciliationRequest =
                    serde_json::from_value(file(&path)?)?;
                if request.scope.firm_id != firm {
                    bail!("request Firm differs from command Firm");
                }
                if interval_seconds.is_some() {
                    let audits: Vec<store::firm_journal::ReconciliationAudit> =
                        serde_json::from_value(
                            call(
                                base,
                                token,
                                reqwest::Method::GET,
                                &format!("{route}/audits?job_id={}", request.id),
                                None,
                            )
                            .await?,
                        )?;
                    cursor = cursor
                        .max(request.signal_cursor)
                        .max(audits.first().map_or(0, |a| a.signal_cursor));
                    let signals: Vec<store::firm_journal::ReconciliationSignal> =
                        serde_json::from_value(
                            call(
                                base,
                                token,
                                reqwest::Method::GET,
                                &format!("{route}/signals?after={cursor}"),
                                None,
                            )
                            .await?,
                        )?;
                    advance_signals(&signals, &request.scope.sources, &mut cursor);
                    request.signal_cursor = cursor;
                }
                if let Some(project) = refresh_project {
                    refresh_sources(base, token, &route, project, &mut request).await?;
                }
                let value = call(
                    base,
                    token,
                    reqwest::Method::POST,
                    &format!("{route}/reconcile"),
                    Some(&serde_json::to_value(&request)?),
                )
                .await?;
                let revalidation = if value
                    .get("posted_entries")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|entries| !entries.is_empty())
                {
                    if let Some(project) = refresh_project {
                        request.mode = store::firm_journal::RepairMode::Preview;
                        request.proposed_entries.clear();
                        request.approvals.clear();
                        refresh_sources(base, token, &route, project, &mut request).await?;
                        Some(
                            call(
                                base,
                                token,
                                reqwest::Method::POST,
                                &format!("{route}/reconcile"),
                                Some(&serde_json::to_value(&request)?),
                            )
                            .await?,
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };
                if summary {
                    let audit: store::firm_journal::ReconciliationAudit = serde_json::from_value(
                        revalidation.clone().unwrap_or_else(|| value.clone()),
                    )?;
                    println!(
                        "{}",
                        serde_json::json!({"job_id":audit.job_id,"at":audit.at,"reconciled":audit.reconciled(),"checks":audit.checks.len(),"exceptions":audit.exceptions.len(),"posted_entries":value.get("posted_entries").and_then(serde_json::Value::as_array).map_or(0,Vec::len)})
                    );
                } else {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(
                            &serde_json::json!({"audit":value,"revalidation":revalidation})
                        )?
                    );
                }
                let Some(interval) = interval_seconds else {
                    break;
                };
                let deadline =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(interval);
                loop {
                    tokio::select! {
                        ()=tokio::time::sleep_until(deadline)=>break,
                        ()=portal::hosting::shutdown_signal()=>return Ok(()),
                        ()=tokio::time::sleep(std::time::Duration::from_secs(5))=>{}
                    }
                    let signals: Vec<store::firm_journal::ReconciliationSignal> =
                        serde_json::from_value(
                            call(
                                base,
                                token,
                                reqwest::Method::GET,
                                &format!("{route}/signals?after={cursor}"),
                                None,
                            )
                            .await?,
                        )?;
                    if advance_signals(&signals, &request.scope.sources, &mut cursor) {
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

async fn refresh_sources(
    base: &str,
    token: &str,
    route: &str,
    project: Uuid,
    request: &mut store::firm_journal::ReconciliationRequest,
) -> Result<()> {
    let reviewed_accounts: std::collections::BTreeMap<_, _> = request
        .snapshots
        .iter()
        .map(|snapshot| (snapshot.source.clone(), snapshot.journal_account.clone()))
        .collect();
    request.snapshots.clear();
    for source in &request.scope.sources {
        if source.locator.is_some() && source.provider != "solana" {
            bail!("unsupported source locator; use reviewed evidence");
        }
        let (adapter, body) = match (source.provider.as_str(), source.object_type.as_str()) {
            ("solana", "transfer" | "network_fee") => (
                "solana",
                serde_json::json!({"project_id":project,"network":source.scope,
                    "wallet":source.account,"signature":source.external_id,"locator":source.locator}),
            ),
            ("mercury", "transaction") => (
                "mercury",
                serde_json::json!({
                    "project_id":project,"account_id":source.account.parse::<Uuid>()?,
                    "transaction_id":source.external_id.parse::<Uuid>()?
                }),
            ),
            (
                "xero",
                kind @ ("invoice" | "credit_note" | "payment" | "bank_transaction"
                | "bank_transfer"),
            ) => {
                let account = if matches!(kind, "invoice" | "credit_note") {
                    None
                } else {
                    Some(source.account.parse::<Uuid>()?)
                };
                (
                    "xero",
                    serde_json::json!({"project_id":project,"account_id":account,
                "object_id":source.external_id.parse::<Uuid>()?,"object_type":kind}),
                )
            }
            _ => bail!("no automatic adapter for this linked source; use reviewed evidence"),
        };
        let snapshot = match call(
            base,
            token,
            reqwest::Method::POST,
            &format!("{route}/{adapter}"),
            Some(&body),
        )
        .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                eprintln!("navigator: linked source unavailable: {error}; recording incomplete reconciliation");
                continue;
            }
        };
        let mut snapshot: store::firm_journal::SourceSnapshot = serde_json::from_value(snapshot)?;
        snapshot.journal_account = reviewed_accounts.get(source).cloned().flatten();
        if snapshot.source != *source {
            bail!("refreshed source identity differs from approved scope");
        }
        request.snapshots.push(snapshot);
    }
    Ok(())
}

fn advance_signals(
    signals: &[store::firm_journal::ReconciliationSignal],
    scope: &[store::firm_journal::SourceReference],
    cursor: &mut i64,
) -> bool {
    let mut wake = false;
    for signal in signals {
        if signal.cursor > *cursor {
            wake |= scope.contains(&signal.source);
            *cursor = signal.cursor;
        }
    }
    wake
}
fn audit_healthy(
    audit: Option<&store::firm_journal::ReconciliationAudit>,
    now: chrono::DateTime<chrono::Utc>,
    age: u64,
) -> bool {
    audit.is_some_and(|a| {
        a.reconciled()
            && chrono::DateTime::parse_from_rfc3339(&a.at).is_ok_and(|at| {
                let elapsed = now.signed_duration_since(at).num_seconds();
                elapsed >= 0 && u64::try_from(elapsed).is_ok_and(|elapsed| elapsed <= age)
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{body_json, header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn read_commands_preserve_firm_and_optional_project_or_job_scope() {
        let server = MockServer::start().await;
        let firm = Uuid::now_v7();
        let project = Uuid::now_v7();
        let job = Uuid::now_v7();
        let route = format!("/app/api/firms/{firm}/journal");
        for (suffix, action, query) in [
            ("", JournalCmd::List, None),
            ("/migration-triage", JournalCmd::MigrationTriage, None),
            ("/audits", JournalCmd::Audits { job: None }, None),
            (
                "/audits",
                JournalCmd::Audits { job: Some(job) },
                Some(("job_id", job)),
            ),
            ("/statement", JournalCmd::Statement { project: None }, None),
            (
                "/statement",
                JournalCmd::Statement {
                    project: Some(project),
                },
                Some(("project_id", project)),
            ),
        ] {
            server.reset().await;
            let mut mock = Mock::given(method("GET"))
                .and(path(format!("{route}{suffix}")))
                .and(header("authorization", "Bearer synthetic-token"));
            if let Some((key, id)) = query {
                mock = mock.and(query_param(key, id.to_string()));
            }
            mock.respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
                .expect(1)
                .mount(&server)
                .await;
            execute_at(firm, &server.uri(), "synthetic-token", action)
                .await
                .unwrap();
            server.verify().await;
        }
    }

    #[tokio::test]
    async fn posting_and_preview_preserve_original_command_and_idempotency_key() {
        let server = MockServer::start().await;
        let firm = Uuid::now_v7();
        let body = serde_json::json!({
            "id":Uuid::now_v7(),"firm_id":firm,"key":"synthetic-original-key",
            "actor_id":Uuid::now_v7(),"occurred_at":"2026-10-11T00:00:00Z",
            "memo":"Synthetic command","postings":[],"sources":[],"receipts":[],
            "links":[],"reversal_of":null
        });
        let input = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(input.path(), body.to_string()).unwrap();
        for preview in [true, false] {
            server.reset().await;
            let suffix = if preview { "/preview" } else { "" };
            Mock::given(method("POST"))
                .and(path(format!("/app/api/firms/{firm}/journal{suffix}")))
                .and(header("authorization", "Bearer synthetic-token"))
                .and(body_json(&body))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({"accepted":true})),
                )
                .expect(1)
                .mount(&server)
                .await;
            let action = if preview {
                JournalCmd::Preview {
                    file: input.path().into(),
                }
            } else {
                JournalCmd::Post {
                    file: input.path().into(),
                }
            };
            execute_at(firm, &server.uri(), "synthetic-token", action)
                .await
                .unwrap();
            server.verify().await;
        }
    }

    #[tokio::test]
    async fn mercury_command_selects_one_account_and_transaction() {
        let server = MockServer::start().await;
        let firm = Uuid::now_v7();
        let project = Uuid::now_v7();
        let account = Uuid::now_v7();
        let transaction = Uuid::now_v7();
        Mock::given(method("POST")).and(path(format!("/app/api/firms/{firm}/journal/mercury")))
            .and(body_json(serde_json::json!({"project_id":project,"account_id":account,"transaction_id":transaction})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1).mount(&server).await;
        execute_at(
            firm,
            &server.uri(),
            "synthetic-token",
            JournalCmd::Mercury {
                project,
                account,
                transaction,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn permission_failure_is_not_retried_and_does_not_expose_response_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/journal"))
            .respond_with(ResponseTemplate::new(403).set_body_string("private provider response"))
            .expect(1)
            .mount(&server)
            .await;
        let error = call(
            &server.uri(),
            "synthetic-token",
            reqwest::Method::POST,
            "/journal",
            None,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("403"));
        assert!(!error.contains("private"));
    }

    #[tokio::test]
    async fn unavailable_service_has_bounded_retries_with_unchanged_body() {
        let server = MockServer::start().await;
        let body = serde_json::json!({"key":"original-key","quantity":"100.00"});
        Mock::given(method("POST"))
            .and(path("/journal"))
            .and(body_json(&body))
            .respond_with(ResponseTemplate::new(503))
            .expect(3)
            .mount(&server)
            .await;
        let error = call(
            &server.uri(),
            "synthetic-token",
            reqwest::Method::POST,
            "/journal",
            Some(&body),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("bounded retries"));
    }

    #[test]
    fn input_rejects_invalid_json_and_oversized_evidence() {
        let input = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(input.path(), "invalid JSON").unwrap();
        assert!(file(&input.path().into()).is_err());
        input.as_file().set_len(10 * 1024 * 1024 + 1).unwrap();
        assert!(file(&input.path().into())
            .unwrap_err()
            .to_string()
            .contains("10 MiB"));
    }

    fn reconciliation(firm: Uuid) -> store::firm_journal::ReconciliationRequest {
        store::firm_journal::ReconciliationRequest {
            signal_cursor: 0,
            id: Uuid::now_v7(),
            mode: store::firm_journal::RepairMode::Preview,
            scope: store::firm_journal::ReconciliationScope {
                firm_id: firm,
                period_start: "2026-10-01T00:00:00Z".into(),
                period_end: "2026-10-31T00:00:00Z".into(),
                sources: vec![],
                complete_statement_document_id: None,
            },
            snapshots: vec![],
            proposed_entries: vec![],
            approvals: vec![],
        }
    }

    #[tokio::test]
    async fn refresh_reads_only_selected_sources_and_retains_reviewed_account_mapping() {
        let server = MockServer::start().await;
        let project = Uuid::now_v7();
        for (provider, kind) in [
            ("mercury", "transaction"),
            ("xero", "invoice"),
            ("xero", "credit_note"),
            ("xero", "payment"),
            ("xero", "bank_transaction"),
            ("xero", "bank_transfer"),
            ("solana", "transfer"),
            ("solana", "network_fee"),
        ] {
            server.reset().await;
            let mut snapshot = store::firm_journal::SourceSnapshot::synthetic("USD", 2, "10000");
            snapshot.source.provider = provider.into();
            snapshot.source.object_type = kind.into();
            snapshot.source.account = Uuid::now_v7().to_string();
            snapshot.source.external_id = Uuid::now_v7().to_string();
            if provider == "solana" {
                snapshot.source.locator = Some("instruction:0".into());
            }
            let mut request = reconciliation(Uuid::now_v7());
            request.scope.sources.push(snapshot.source.clone());
            let mut reviewed = snapshot.clone();
            reviewed.journal_account = Some("firm-operating".into());
            request.snapshots.push(reviewed);
            Mock::given(method("POST"))
                .and(path(format!("/journal/{provider}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(&snapshot))
                .expect(1)
                .mount(&server)
                .await;
            refresh_sources(
                &server.uri(),
                "synthetic-token",
                "/journal",
                project,
                &mut request,
            )
            .await
            .unwrap();
            assert_eq!(request.snapshots.len(), 1);
            assert_eq!(
                request.snapshots[0].journal_account.as_deref(),
                Some("firm-operating")
            );
            let requests = server.received_requests().await.unwrap();
            let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert_eq!(sent["project_id"], project.to_string());
            let key = if provider == "mercury" {
                "transaction_id"
            } else if provider == "solana" {
                "signature"
            } else {
                "object_id"
            };
            assert_eq!(sent[key], snapshot.source.external_id);
            if provider == "xero" && matches!(kind, "invoice" | "credit_note") {
                assert!(sent["account_id"].is_null());
            }
            server.verify().await;
        }
    }

    #[tokio::test]
    async fn refresh_missing_source_records_incomplete_evidence_but_wrong_identity_refuses() {
        let server = MockServer::start().await;
        let mut snapshot = store::firm_journal::SourceSnapshot::synthetic("USD", 2, "10000");
        snapshot.source.provider = "mercury".into();
        snapshot.source.account = Uuid::now_v7().to_string();
        snapshot.source.external_id = Uuid::now_v7().to_string();
        let mut request = reconciliation(Uuid::now_v7());
        request.scope.sources.push(snapshot.source.clone());
        request.snapshots.push(snapshot.clone());
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        refresh_sources(
            &server.uri(),
            "synthetic-token",
            "/journal",
            Uuid::now_v7(),
            &mut request,
        )
        .await
        .unwrap();
        assert!(request.snapshots.is_empty());
        assert_eq!(request.scope.sources.len(), 1);
        server.reset().await;
        snapshot.source.external_id = Uuid::now_v7().to_string();
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(snapshot))
            .mount(&server)
            .await;
        assert!(refresh_sources(
            &server.uri(),
            "synthetic-token",
            "/journal",
            Uuid::now_v7(),
            &mut request
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("identity differs"));
    }

    #[tokio::test]
    async fn reconcile_revalidates_posted_repairs_with_fresh_preview_and_refuses_wrong_firm() {
        let server = MockServer::start().await;
        let firm = Uuid::now_v7();
        let mut request = reconciliation(firm);
        request.mode = store::firm_journal::RepairMode::ApprovedSafe;
        let input = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(input.path(), serde_json::to_vec(&request).unwrap()).unwrap();
        let route = format!("/app/api/firms/{firm}/journal/reconcile");
        Mock::given(method("POST"))
            .and(path(&route))
            .and(body_json(serde_json::to_value(&request).unwrap()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"posted_entries":[Uuid::now_v7()]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut preview = request.clone();
        preview.mode = store::firm_journal::RepairMode::Preview;
        Mock::given(method("POST"))
            .and(path(&route))
            .and(body_json(serde_json::to_value(preview).unwrap()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"posted_entries":[]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        execute_at(
            firm,
            &server.uri(),
            "synthetic-token",
            JournalCmd::Reconcile {
                file: input.path().into(),
                refresh_project: Some(Uuid::now_v7()),
                interval_seconds: None,
                summary: false,
            },
        )
        .await
        .unwrap();
        server.verify().await;
        assert!(execute_at(
            Uuid::now_v7(),
            &server.uri(),
            "synthetic-token",
            JournalCmd::Reconcile {
                file: input.path().into(),
                refresh_project: None,
                interval_seconds: None,
                summary: false,
            }
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("Firm differs"));
        assert!(execute_at(
            firm,
            &server.uri(),
            "synthetic-token",
            JournalCmd::Reconcile {
                file: input.path().into(),
                refresh_project: None,
                interval_seconds: Some(60),
                summary: false,
            }
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("requires --refresh-project"));
    }

    fn healthy_audit(firm: Uuid) -> store::firm_journal::ReconciliationAudit {
        let snapshot = store::firm_journal::SourceSnapshot::synthetic("USD", 2, "10000");
        store::firm_journal::ReconciliationAudit {
            signal_cursor: 0,
            job_id: Uuid::now_v7(),
            scope: reconciliation(firm).scope,
            approvals: vec![],
            complete_account_reconciliation: false,
            firm_id: firm,
            at: chrono::Utc::now().to_rfc3339(),
            mode: store::firm_journal::RepairMode::Preview,
            source_digests: vec![],
            posted_entries: vec![],
            exceptions: vec![],
            checks: vec![store::firm_journal::Reconciliation {
                source: snapshot.source,
                expected_quantity: "10000".into(),
                actual_quantity: Some("10000".into()),
                unit: snapshot.unit,
                checks: std::collections::BTreeMap::from([("balanced".into(), true)]),
                exceptions: vec![],
            }],
        }
    }

    #[tokio::test]
    async fn status_and_summary_fail_stale_jobs_without_exposing_evidence() {
        let server = MockServer::start().await;
        let firm = Uuid::now_v7();
        let mut audit = healthy_audit(firm);
        let job = audit.job_id;
        let now = chrono::Utc::now();
        assert!(audit_healthy(Some(&audit), now, 600));
        for at in [
            "invalid".to_owned(),
            (now - chrono::Duration::seconds(601)).to_rfc3339(),
            (now + chrono::Duration::seconds(60)).to_rfc3339(),
        ] {
            audit.at = at;
            assert!(!audit_healthy(Some(&audit), now, 600));
        }
        audit.at = now.to_rfc3339();
        audit.exceptions.push("synthetic-review".into());
        assert!(!audit_healthy(Some(&audit), now, 600));
        audit.exceptions.clear();
        Mock::given(method("GET"))
            .and(path(format!("/app/api/firms/{firm}/journal/audits")))
            .and(query_param("job_id", job.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![audit.clone()]))
            .expect(1)
            .mount(&server)
            .await;
        execute_at(
            firm,
            &server.uri(),
            "synthetic-token",
            JournalCmd::Status {
                job,
                stale_after_seconds: 600,
            },
        )
        .await
        .unwrap();
        server.verify().await;
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;
        assert!(execute_at(
            firm,
            &server.uri(),
            "synthetic-token",
            JournalCmd::Status {
                job,
                stale_after_seconds: 600
            }
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("stale or needs review"));
        server.verify().await;
        server.reset().await;
        let input = tempfile::NamedTempFile::new().unwrap();
        let request = reconciliation(firm);
        std::fs::write(input.path(), serde_json::to_vec(&request).unwrap()).unwrap();
        Mock::given(method("POST"))
            .and(path(format!("/app/api/firms/{firm}/journal/reconcile")))
            .and(body_json(serde_json::to_value(&request).unwrap()))
            .respond_with(ResponseTemplate::new(200).set_body_json(audit))
            .expect(1)
            .mount(&server)
            .await;
        execute_at(
            firm,
            &server.uri(),
            "synthetic-token",
            JournalCmd::Reconcile {
                file: input.path().into(),
                refresh_project: None,
                interval_seconds: None,
                summary: true,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn scheduled_reconciliation_resumes_cursor_and_wakes_only_for_a_linked_signal() {
        let server = MockServer::start().await;
        let firm = Uuid::now_v7();
        let mut snapshot = store::firm_journal::SourceSnapshot::synthetic("USD", 2, "10000");
        snapshot.source.provider = "mercury".into();
        snapshot.source.account = Uuid::now_v7().to_string();
        snapshot.source.external_id = Uuid::now_v7().to_string();
        let mut request = reconciliation(firm);
        request.scope.sources.push(snapshot.source.clone());
        let input = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(input.path(), serde_json::to_vec(&request).unwrap()).unwrap();
        let mut audit = healthy_audit(firm);
        audit.job_id = request.id;
        audit.signal_cursor = 9;
        let route = format!("/app/api/firms/{firm}/journal");
        Mock::given(method("GET"))
            .and(path(format!("{route}/audits")))
            .and(query_param("job_id", request.id.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![audit]))
            .expect(2)
            .mount(&server)
            .await;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let source = snapshot.source.clone();
        Mock::given(method("GET"))
            .and(path(format!("{route}/signals")))
            .and(query_param("after", "9"))
            .respond_with(move |_: &wiremock::Request| {
                let signals = if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    vec![]
                } else {
                    vec![store::firm_journal::ReconciliationSignal {
                        cursor: 10,
                        source: source.clone(),
                    }]
                };
                ResponseTemplate::new(200).set_body_json(signals)
            })
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{route}/signals")))
            .and(query_param("after", "10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{route}/mercury")))
            .respond_with(ResponseTemplate::new(200).set_body_json(&snapshot))
            .expect(2)
            .mount(&server)
            .await;
        let mut first = request.clone();
        first.signal_cursor = 9;
        first.snapshots.push(snapshot);
        Mock::given(method("POST"))
            .and(path(format!("{route}/reconcile")))
            .and(body_json(serde_json::to_value(&first).unwrap()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"posted_entries":[]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        first.signal_cursor = 10;
        Mock::given(method("POST"))
            .and(path(format!("{route}/reconcile")))
            .and(body_json(serde_json::to_value(&first).unwrap()))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            execute_at(
                firm,
                &server.uri(),
                "synthetic-token",
                JournalCmd::Reconcile {
                    file: input.path().into(),
                    refresh_project: Some(Uuid::now_v7()),
                    interval_seconds: Some(60),
                    summary: false,
                },
            ),
        )
        .await
        .unwrap();
        assert!(result.unwrap_err().to_string().contains("403"));
        server.verify().await;
    }

    #[test]
    fn signals_only_wake_approved_sources_and_never_expand_scope() {
        let source = store::firm_journal::SourceReference {
            provider: "xero".into(),
            scope: "tenant".into(),
            account: "account".into(),
            object_type: "invoice".into(),
            external_id: "one".into(),
            locator: None,
        };
        let mut other = source.clone();
        other.external_id = "two".into();
        let scope = vec![source.clone()];
        let mut cursor = 0;
        assert!(!advance_signals(
            &[store::firm_journal::ReconciliationSignal {
                cursor: 1,
                source: other
            }],
            &scope,
            &mut cursor
        ));
        assert_eq!(cursor, 1);
        assert!(advance_signals(
            &[store::firm_journal::ReconciliationSignal {
                cursor: 2,
                source: source.clone()
            }],
            &scope,
            &mut cursor
        ));
        assert!(!advance_signals(
            &[store::firm_journal::ReconciliationSignal { cursor: 2, source }],
            &scope,
            &mut cursor
        ));
        assert_eq!(scope.len(), 1);
        assert!(!audit_healthy(None, chrono::Utc::now(), 600));
    }
}
