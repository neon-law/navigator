//! Stage a single timekeeper's explicit billing trailer from a Project checkout.
use std::{path::Path, process::Command, time::Duration};

use anyhow::{ensure, Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{manifest, surfaces};

fn trailer_lines(description: &str) -> impl Iterator<Item = &str> {
    description
        .lines()
        .rev()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.strip_prefix("Billing-Time:"))
}

/// Format: Billing-Time: <hours in tenths> <work date> <entry UUID>.
/// Only machine fields leave the checkout. The lawyer supplies the client narrative at review.
fn trailer(description: &str) -> Result<(i32, NaiveDate, Uuid)> {
    let mut lines = trailer_lines(description);
    let fields: Vec<_> = lines
        .next()
        .context("missing Billing-Time trailer; ask the author for actual hours and the work date, then add a signed change trailer: Billing-Time: <hours in tenths> <YYYY-MM-DD> <entry UUID>")?
        .strip_prefix(' ')
        .context("leave a space after Billing-Time:")?
        .split_whitespace()
        .collect();
    ensure!(
        lines.next().is_none() && fields.len() == 3,
        "expected one Billing-Time trailer with hours, date, and UUID"
    );
    let (whole, fraction) = fields[0]
        .split_once('.')
        .context("use hours in exact tenths, such as 0.2")?;
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && fraction.len() == 1
            && fraction.as_bytes()[0].is_ascii_digit(),
        "use hours in exact tenths"
    );
    let tenths = whole
        .parse::<i32>()?
        .checked_mul(10)
        .and_then(|v| v.checked_add(i32::from(fraction.as_bytes()[0] - b'0')))
        .context("invalid hours")?;
    ensure!(
        (1..=240).contains(&tenths),
        "hours must be between 0.1 and 24.0"
    );
    Ok((tenths, fields[1].parse()?, fields[2].parse()?))
}

/// Snapshot change metadata without passing description text through a shell.
#[derive(Deserialize)]
pub(super) struct JujutsuChange {
    pub commit_id: String,
    pub change_id: String,
    pub description: String,
    author: String,
    pub signed: bool,
    pub conflict: bool,
    pub divergent: bool,
}

pub(super) fn changes(dir: &Path, revision: &str) -> Result<Vec<JujutsuChange>> {
    ensure!(dir.join(".jj").is_dir(), "run from a Jujutsu checkout root");
    let template = r#"json({"commit_id" => commit_id, "change_id" => change_id, "description" => description, "author" => author.email(), "signed" => if(signature, true, false), "conflict" => conflict, "divergent" => divergent}) ++ "\n""#;
    let output = Command::new("jj")
        .current_dir(dir)
        .args([
            "log",
            "--no-graph",
            "--no-pager",
            "-r",
            revision,
            "-T",
            template,
        ])
        .output()
        .context("read Jujutsu changes")?;
    ensure!(output.status.success(), "could not read Jujutsu revisions");
    String::from_utf8(output.stdout)?
        .lines()
        .map(|line| serde_json::from_str(line).context("decode Jujutsu change"))
        .collect()
}

pub(super) fn change(dir: &Path, revision: &str) -> Result<JujutsuChange> {
    let mut selected = changes(dir, revision)?;
    ensure!(selected.len() == 1, "select exactly one Jujutsu change");
    Ok(selected.remove(0))
}

#[derive(Serialize)]
struct Annotation<'a> {
    id: Uuid,
    change_id: &'a str,
    commit_id: &'a str,
    tenths: i32,
    performed_on: NaiveDate,
}

fn annotations<'a>(
    changes: &'a [JujutsuChange],
    email: &str,
    explicit: bool,
) -> Result<Vec<Annotation<'a>>> {
    let mut annotations = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for change in changes {
        if !change.author.eq_ignore_ascii_case(email) {
            ensure!(
                !explicit,
                "the signed-in timekeeper must match the change author"
            );
            eprintln!(
                "billing: the author must stage time for change {}",
                change.change_id
            );
            continue;
        }
        ensure!(
            !change.conflict && !change.divergent,
            "resolve the change before staging time"
        );
        let (tenths, performed_on, id) = trailer(&change.description)
            .with_context(|| format!("read billing trailer on change {}", change.change_id))?;
        ensure!(
            ids.insert(id),
            "billing annotation UUID is reused on multiple changes"
        );
        annotations.push(Annotation {
            id,
            change_id: &change.change_id,
            commit_id: &change.commit_id,
            tenths,
            performed_on,
        });
    }
    Ok(annotations)
}

pub async fn stage_in(dir: &Path, revision: &str) -> Result<()> {
    let change = change(dir, revision)?;
    stage_changes(dir, &[change], true).await
}

pub(super) async fn stage_outgoing(dir: &Path, mut changes: Vec<JujutsuChange>) -> Result<()> {
    changes.retain(|change| trailer_lines(&change.description).next().is_some());
    stage_changes(dir, &changes, false).await
}

async fn stage_changes(dir: &Path, changes: &[JujutsuChange], explicit: bool) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let manifest = manifest::read(dir).context("missing Project manifest")?;
    let host = manifest.host.context("missing Project host")?;
    let project = manifest.project.context("missing Project code")?;
    let crate::remote::MatchingSession::Valid(base, token) =
        crate::remote::matching_stored_session(&host)?
    else {
        anyhow::bail!("sign in to the host in navigator.yaml before staging time")
    };
    let email = crate::login::fetch_whoami(&base, &token)
        .await?
        .email
        .context("the signed-in timekeeper needs a session email")?;
    let annotations = annotations(changes, &email, explicit)?;
    if annotations.is_empty() {
        return Ok(());
    }
    let project_id = surfaces::resolve_project_id(&base, &token, &project).await?;
    let http = crate::remote::authenticated_client_builder(&base)?
        .timeout(Duration::from_secs(30))
        .build()?;
    let endpoint = format!("{base}/app/api/projects/{project_id}/billing-annotations");
    for annotation in annotations {
        let response = http
            .post(&endpoint)
            .bearer_auth(&token)
            .json(&annotation)
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "staging change {} refused ({})",
            annotation.change_id,
            response.status()
        );
        if !explicit {
            println!("billing: staged change {} for review", annotation.change_id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::test_login::{answer, site};
    use serde_json::json;

    const ROW: &str = "Billing-Time: 0.2 2026-11-20 00000000-0000-4000-8000-000000000001";

    fn authored(author: &str, description: &str) -> JujutsuChange {
        JujutsuChange {
            commit_id: "commit-one".into(),
            change_id: "change-one".into(),
            description: description.into(),
            author: author.into(),
            signed: true,
            conflict: false,
            divergent: false,
        }
    }

    #[tokio::test]
    async fn staging_posts_the_signed_in_authors_trailer_to_project_billing() {
        use wiremock::matchers::{body_json, method, path};
        let id = Uuid::new_v4();
        let (server, _login) = site(json!([{"id": id, "code": "sample-matter"}])).await;
        let host = server.uri();
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("navigator.yaml");
        let mine = || vec![authored("lawyer@example.com", &format!("Subject\n\n{ROW}"))];
        let root = dir.path();
        let stage = |changes| async move {
            stage_outgoing(root, changes)
                .await
                .map_err(|error| error.to_string())
        };

        assert_eq!(stage(Vec::new()).await, Ok(()));
        assert_eq!(
            stage(vec![authored("lawyer@example.com", "No trailer")]).await,
            Ok(())
        );
        assert_eq!(
            stage(mine()).await,
            Err("missing Project manifest".to_string())
        );
        let elsewhere = "project:\n  name: sample-matter\n  host: signed-out.example.test\n";
        std::fs::write(&manifest, elsewhere).unwrap();
        assert_eq!(
            stage(mine()).await,
            Err("sign in to the host in navigator.yaml before staging time".to_string())
        );

        std::fs::write(
            &manifest,
            elsewhere.replace("signed-out.example.test", &host),
        )
        .unwrap();
        let whoami = "/auth/cli/whoami";
        {
            let anonymous = json!({"role": "lawyer", "exp": 4_102_444_800_i64});
            let _whoami = wiremock::Mock::given(method("GET"))
                .and(path(whoami))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(anonymous))
                .mount_as_scoped(&server)
                .await;
            assert_eq!(
                stage(mine()).await,
                Err("the signed-in timekeeper needs a session email".to_string())
            );
        }
        let lawyer =
            json!({"email": "Lawyer@example.com", "role": "lawyer", "exp": 4_102_444_800_i64});
        answer(&server, "GET", whoami, 200, lawyer).await;
        assert_eq!(
            stage(vec![authored(
                "other@example.com",
                &format!("Subject\n\n{ROW}")
            )])
            .await,
            Ok(())
        );
        assert_eq!(
            stage(mine()).await,
            Err("staging change change-one refused (404 Not Found)".to_string())
        );
        wiremock::Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{id}/billing-annotations")))
            .and(body_json(json!({
                "id": "00000000-0000-4000-8000-000000000001",
                "change_id": "change-one",
                "commit_id": "commit-one",
                "tenths": 2,
                "performed_on": "2026-11-20",
            })))
            .respond_with(wiremock::ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(stage(mine()).await, Ok(()));
    }

    #[tokio::test]
    async fn changes_are_read_only_from_a_jujutsu_checkout_root() {
        let _jj = crate::projects::test_login::jj_stand_in();
        let dir = tempfile::tempdir().unwrap();
        let error = stage_in(dir.path(), "@").await.unwrap_err();
        assert_eq!(error.to_string(), "run from a Jujutsu checkout root");

        let store = dir.path().join(".jj");
        std::fs::create_dir(&store).unwrap();
        let record = json!({
            "commit_id": "commit-one", "change_id": "change-one", "description": "Subject\n",
            "author": "lawyer@example.com", "signed": true, "conflict": false, "divergent": false,
        });
        std::fs::write(store.join("log"), format!("{record}\n")).unwrap();
        let selected = change(dir.path(), "@").unwrap();
        assert_eq!(selected.change_id, "change-one");
        assert!(selected.signed && !selected.conflict && !selected.divergent);

        std::fs::write(store.join("log"), format!("{record}\n{record}\n")).unwrap();
        let error = change(dir.path(), "@-::@").err().unwrap();
        assert_eq!(error.to_string(), "select exactly one Jujutsu change");
        std::fs::write(store.join("log"), "not a record\n").unwrap();
        let error = change(dir.path(), "@").err().unwrap();
        assert_eq!(error.to_string(), "decode Jujutsu change");
    }

    #[test]
    fn a_conflicted_change_or_a_reused_annotation_is_refused() {
        let description = format!("Subject\n\n{ROW}");
        let conflicted = JujutsuChange {
            conflict: true,
            ..authored("lawyer@example.com", &description)
        };
        let error = annotations(&[conflicted], "lawyer@example.com", true)
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "resolve the change before staging time");
        let twice = [
            authored("lawyer@example.com", &description),
            authored("lawyer@example.com", &description),
        ];
        let error = annotations(&twice, "lawyer@example.com", true)
            .err()
            .unwrap();
        assert!(error.to_string().contains("reused on multiple changes"));
        let untagged = [authored("lawyer@example.com", "Subject")];
        let error = annotations(&untagged, "lawyer@example.com", true)
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("read billing trailer on change change-one"));
    }

    #[test]
    fn automatic_staging_attributes_only_the_authenticated_authors_changes() {
        let changes = [
            JujutsuChange {
                commit_id: "commit-one".into(),
                change_id: "change-one".into(),
                description: "Change one\n\nBilling-Time: 0.2 2026-11-20 00000000-0000-4000-8000-000000000001".into(),
                author: "Lawyer@example.com".into(),
                signed: true,
                conflict: false,
                divergent: false,
            },
            JujutsuChange {
                commit_id: "commit-two".into(),
                change_id: "change-two".into(),
                description: "Change two\n\nBilling-Time: 0.3 2026-11-20 00000000-0000-4000-8000-000000000002".into(),
                author: "other@example.com".into(),
                signed: true,
                conflict: false,
                divergent: false,
            },
        ];
        let rows = annotations(&changes, "lawyer@example.com", false).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            serde_json::to_value(&rows[0]).unwrap(),
            serde_json::json!({
                "id": "00000000-0000-4000-8000-000000000001",
                "change_id": "change-one",
                "commit_id": "commit-one",
                "tenths": 2,
                "performed_on": "2026-11-20"
            })
        );
        assert!(annotations(&changes[1..], "lawyer@example.com", true).is_err());
    }

    #[test]
    fn only_one_explicit_final_trailer_can_stage_exact_tenths() {
        let row = "Billing-Time: 0.2 2026-11-20 00000000-0000-4000-8000-000000000001";
        let description = format!("Private subject\n\n{row}\n");
        assert_eq!(trailer_lines(&description).count(), 1);
        assert_eq!(
            trailer_lines(&format!("{row}\n\nUnrelated body")).count(),
            0
        );
        assert_eq!(trailer_lines("Private subject\n\nBilling-Time:").count(), 1);
        assert_eq!(trailer(&description).unwrap().0, 2);
        assert_eq!(trailer(&description.replace('\n', "\r\n")).unwrap().0, 2);
        for invalid in [
            row.replace("0.2", "0.01"),
            row.replace("0.2", "24.1"),
            row.replace("0.2", "-0.2"),
            format!("{row}\n{row}"),
            format!("{row}\nBilling-Time:"),
            format!("{row}\n\nUnrelated body"),
            format!("{row}\r\n\r\nUnrelated body"),
        ] {
            assert!(trailer(&invalid).is_err());
        }
    }
}
