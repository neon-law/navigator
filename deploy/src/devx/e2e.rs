//! Verification of the task-owned PostgreSQL and DBOS KIND application.

use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::{require_tools, use_kind_context, KindConfig};

/// Minimum seed-row counts the deployed stack must show. Matches the
/// canonical seed in `store/seeds/`; a smaller count means seeding
/// silently failed.
///
/// Both witnesses read PostgreSQL through the same accessors as the application.
const MIN_QUESTIONS: i64 = 8;
/// The bundled template catalog carries 20+ current rows.
const MIN_TEMPLATES: i64 = 8;

/// Whether the seed counts clear the minimums.
fn seed_counts_ok(questions: i64, templates: i64) -> bool {
    questions >= MIN_QUESTIONS && templates >= MIN_TEMPLATES
}

/// The local Rauthy fixture's Lawyer login, seeded so the browser gate's
/// admin-gated walk can run.
const LAWYER_EMAIL: &str = "lawyer@neonlaw.com";
const LAWYER_NAME: &str = "Lawrence Lawyer";

/// Find or promote the synthetic Lawyer through the authoritative Person accessor.
async fn grant_lawyer_in(surreal: &store::surreal::SurrealDb) -> Result<()> {
    match store::persons::find_by_email_ci(surreal, LAWYER_EMAIL)
        .await
        .context("look up the lawyer person")?
    {
        Some(existing) => {
            // A Rauthy sign-in without a name claim can leave the mailbox as
            // the display name. The retainer fixture needs a named signer.
            let name = existing
                .name
                .trim()
                .eq_ignore_ascii_case(&existing.email)
                .then(|| LAWYER_NAME.to_owned());
            store::persons::edit(
                surreal,
                existing.id,
                &store::persons::PersonEdit {
                    name,
                    role: Some(store::persons::Role::Lawyer),
                    ..Default::default()
                },
            )
            .await
            .context("prepare the lawyer person")?;
        }
        None => {
            store::persons::create(
                surreal,
                &store::persons::NewPerson::with_role(
                    LAWYER_NAME,
                    LAWYER_EMAIL,
                    store::persons::Role::Lawyer,
                ),
            )
            .await
            .context("seed the lawyer person")?;
        }
    }
    Ok(())
}

// ---------- orchestration (shell-out; not unit-tested) ----------

/// `devx e2e`: the full deployed-stack smoke check.
pub fn run_e2e(cfg: &KindConfig) -> Result<()> {
    require_tools(&["kubectl", "curl"])?;
    use_kind_context(cfg)?;
    let root = super::orchestrate::workspace_root()?;
    super::orchestrate::verified_native_database(&root, cfg)?;
    wait_for_clamd_ping(cfg.clamav_port)?;
    check_health_at(&format!(
        "localhost:{}",
        super::worktree_env::web_port(&root)?
    ))?;
    check_seed()?;
    eprintln!("=== native checks passed ===");
    Ok(())
}

fn wait_for_clamd_ping(port: u16) -> Result<()> {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};

    let addr: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .context("parse ClamAV host address")?;
    let deadline = Instant::now() + Duration::from_mins(1);
    loop {
        let ready = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
            .and_then(|mut stream| {
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.write_all(b"zPING\0")?;
                let mut reply = [0_u8; 64];
                let read = stream.read(&mut reply)?;
                Ok(reply[..read].windows(4).any(|part| part == b"PONG"))
            })
            .unwrap_or(false);
        if ready {
            eprintln!("    ClamAV PING/PONG ready");
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for ClamAV PING/PONG on 127.0.0.1:{port}");
        }
        sleep(Duration::from_millis(500));
    }
}

/// `devx grant-lawyer`: pre-seed the Lawyer demo user with the `lawyer`
/// role so the browser e2e's admin-gated walk can run.
pub fn grant_lawyer(_cfg: &KindConfig) -> Result<()> {
    // The grant connects to the endpoint the sourced `.devx/env` names —
    // the same coordinates the running `web` reads — through
    // `NAVIGATOR_SURREAL_*`.
    grant_lawyer_from_env()
}

/// Grant Lawyer the `lawyer` role in the `SurrealDB` the environment names.
///
/// The browser gate ([`super::browser_e2e`]) grants into whichever store
/// its host `web` reads, which is the same env-resolved endpoint — so it
/// calls straight through here.
pub fn grant_lawyer_at() -> Result<()> {
    grant_lawyer_from_env()
}

fn grant_lawyer_from_env() -> Result<()> {
    eprintln!("=== granting lawyer the lawyer role in PostgreSQL ===");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create tokio runtime")?;
    runtime.block_on(async {
        let surreal = crate::application_store()
            .await
            .context("verify PostgreSQL application authority")?;
        grant_lawyer_in(&surreal).await?;
        eprintln!("{LAWYER_EMAIL} role=lawyer");
        Ok(())
    })
}

/// Restate readiness depends on the broker. Restate Cloud has no
/// in-cluster `StatefulSet` — probe the tenant via the CLI; otherwise
/// wait on the Operator's `restate` `StatefulSet`. Either way the
/// worker Deployment must roll out.
fn check_health_at(host: &str) -> Result<()> {
    let url = format!("http://{host}/app/health");
    let deadline = Instant::now() + Duration::from_mins(1);
    let mut attempt = 0;
    loop {
        attempt += 1;
        let out = Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                "10",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
            ])
            .arg("--resolve")
            .arg("localhost:8080:127.0.0.1")
            .arg(&url)
            .output()
            .context("curl /app/health")?;
        let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if status == "200" {
            eprintln!("    health OK ({status}) after {attempt} attempt(s)");
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "expected HTTP 200 from {url} within 60s; last status {status:?} after {attempt} attempt(s)"
            );
        }
        eprintln!(
            "    /app/health not ready (status {status:?}); retrying in 2s [attempt {attempt}]"
        );
        sleep(Duration::from_secs(2));
    }
}

/// Confirm the canonical Question and current Template catalogs were seeded.
fn check_seed() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create tokio runtime")?;
    let (q, e) = runtime.block_on(count_seeded_rows())?;
    if !seed_counts_ok(q, e) {
        bail!("expected at least {MIN_QUESTIONS} questions and {MIN_TEMPLATES} templates; got q={q} e={e}");
    }
    eprintln!("seed OK (q={q} e={e})");
    Ok(())
}

async fn count_seeded_rows() -> Result<(i64, i64)> {
    let db = crate::application_store().await?;
    let questions = store::questions::list_all(&db)
        .await
        .context("count the seeded questions")?;
    let templates = store::templates::list_current(&db)
        .await
        .context("count the seeded templates")?;
    Ok((witnessed(questions.len()), witnessed(templates.len())))
}

/// Narrow a row count to the `i64` the minimums compare against. A count
/// past `i64::MAX` clears every floor, so saturating is exact enough.
fn witnessed(rows: usize) -> i64 {
    i64::try_from(rows).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn witnessed_narrows_a_row_count() {
        assert_eq!(witnessed(0), 0);
        assert_eq!(witnessed(38), 38);
    }

    #[tokio::test]
    async fn seed_witnesses_read_native_rows_and_count_only_current_templates() {
        store::test_support::with_postgres(async {
            assert_eq!(count_seeded_rows().await.unwrap(), (0, 0));
            let db = store::surreal::SurrealDb::uninitialized();
            store::questions::create(
                &db,
                &store::questions::NewQuestion::new("synthetic_question", "Synthetic?", "string"),
            )
            .await
            .unwrap();
            for title in ["First", "Second"] {
                store::templates::save_version(
                    &db,
                    None,
                    "synthetic_template",
                    store::templates::Version {
                        title: title.into(),
                        respondent_type: "person".into(),
                        asset_id: None,
                        form_code: None,
                        kind: None,
                        source_commit_sha: None,
                    },
                )
                .await
                .unwrap();
            }
            assert_eq!(count_seeded_rows().await.unwrap(), (1, 1));
        })
        .await;
    }

    /// Two witnesses, so both must clear: a stack where only one table
    /// populated has to fail rather than pass on the other's count.
    #[test]
    fn one_witness_alone_is_not_enough() {
        assert!(!seed_counts_ok(0, 38), "templates alone must not pass");
        assert!(!seed_counts_ok(38, 0), "questions alone must not pass");
    }

    #[test]
    fn seed_counts_ok_enforces_the_minimums() {
        assert!(seed_counts_ok(8, 8));
        assert!(seed_counts_ok(20, 10));
        assert!(!seed_counts_ok(7, 8));
        assert!(!seed_counts_ok(8, 7));
    }

    /// The grant seeds the row when the mailbox is absent.
    #[tokio::test]
    async fn grant_lawyer_seeds_the_lawyer_person() {
        store::test_support::with_postgres(async {
            let surreal = store::surreal::SurrealDb::uninitialized();

            grant_lawyer_in(&surreal).await.unwrap();

            let row = store::persons::find_by_email_ci(&surreal, LAWYER_EMAIL)
                .await
                .unwrap()
                .expect("the grant seeds the mailbox");
            assert_eq!(row.role, store::persons::Role::Lawyer);
            assert_eq!(row.email, LAWYER_EMAIL);
        })
        .await;
    }

    #[tokio::test]
    async fn grant_lawyer_replaces_the_fixture_mailbox_display_name() {
        store::test_support::with_postgres(async {
            let surreal = store::surreal::SurrealDb::uninitialized();
            let fixture = store::persons::create(
                &surreal,
                &store::persons::NewPerson::with_role(
                    LAWYER_EMAIL,
                    LAWYER_EMAIL,
                    store::persons::Role::Client,
                ),
            )
            .await
            .unwrap();

            grant_lawyer_in(&surreal).await.unwrap();

            let row = store::persons::find_by_email_ci(&surreal, LAWYER_EMAIL)
                .await
                .unwrap()
                .expect("the existing fixture remains");
            assert_eq!(row.id, fixture.id);
            assert_eq!(row.role, store::persons::Role::Lawyer);
            assert_eq!(row.name, "Lawrence Lawyer");
        })
        .await;
    }

    /// …and promotes it when it is already there, at whatever casing.
    ///
    /// `person_email_lower` holds one row per mailbox, so a Lawyer row stored
    /// as `Lawyer@NeonLaw.com` is the *same* mailbox as the seeded
    /// `lawyer@neonlaw.com`. The grant must promote that row rather than
    /// attempt a second create the unique index would reject.
    #[tokio::test]
    async fn grant_lawyer_folds_a_case_variant_row_instead_of_colliding() {
        store::test_support::with_postgres(async {
            let surreal = store::surreal::SurrealDb::uninitialized();
            store::persons::create(
                &surreal,
                &store::persons::NewPerson::with_role(
                    "Fixture Counsel",
                    "Lawyer@NeonLaw.com",
                    store::persons::Role::Client,
                ),
            )
            .await
            .unwrap();

            grant_lawyer_in(&surreal).await.unwrap();

            let rows = store::persons::list_directory(&surreal).await.unwrap();
            assert_eq!(
                rows.len(),
                1,
                "the grant must fold the case-variant mailbox, not add a row"
            );
            assert_eq!(
                rows[0].email, "Lawyer@NeonLaw.com",
                "the stored casing is preserved"
            );
            assert_eq!(rows[0].role, store::persons::Role::Lawyer);
            assert_eq!(rows[0].name, "Fixture Counsel");
        })
        .await;
    }
}
