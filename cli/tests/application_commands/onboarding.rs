//! `navigator project status` flags an official onboarding whose signed PDF
//! or Certificate of Completion is not filed, end to end (LAW-586).
use serde_json::Value;

use crate::live::LiveSite;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_flags_an_official_onboarding_without_its_signed_record() {
    store::test_support::with_postgres(async {
        let site = LiveSite::start(None, None, true).await;
        let db = store::application_store::ApplicationStore::uninitialized();
        let status = || {
            let output = assert_cmd::Command::cargo_bin("navigator")
                .unwrap()
                .current_dir(site.root.path())
                .env("NAVIGATOR_CREDENTIALS_FILE", site.root.path().join("credentials.json"))
                .args(["project", "status", &site.project.code, "--host", &site.host, "--json"])
                .assert()
                .success();
            serde_json::from_slice::<Value>(&output.get_output().stdout).unwrap()
        };
        let missing = |value: &Value| {
            value["missing"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item.as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        let pitch = status();
        assert!(
            !missing(&pitch).iter().any(|item| item.contains("signed engagement letter PDF")),
            "an unsigned onboarding needs no signed copy yet"
        );

        store::test_support::seed_signed_onboarding(&db, site.project.id, site.lawyer).await;
        let signed = status();
        assert_eq!(signed["onboarding"]["execution_verified"], true);
        let flagged = missing(&signed);
        assert!(flagged.iter().any(|item| item.contains("no signed engagement letter PDF")), "{flagged:?}");
        assert!(flagged.iter().any(|item| item.contains("Certificate of Completion")), "{flagged:?}");

        for (name, signing) in [
            ("2026.11.02_engagement-letter_signed.pdf", Some(r#"{"method":"docusign","signed_on":"2026-11-02"}"#)),
            ("2026.11.02_engagement-letter_certificate.pdf", None),
        ] {
            sqlx::query(
                "INSERT INTO asset (storage_key,content_type,byte_size,sha256_hex,project_id,filename,kind,source,slug,signing)
                 VALUES ($1,'application/pdf',4,$2,$3,$1,'onboarding','docusign',$1,$4::jsonb)",
            )
            .bind(name)
            .bind(store::assets::sha256_hex(name.as_bytes()))
            .bind(site.project.id)
            .bind(signing)
            .execute(site.pg.pool())
            .await
            .unwrap();
        }
        let filed = status();
        assert_eq!(
            filed["onboarding"]["signed_pdf"],
            "2026.11.02_engagement-letter_signed.pdf"
        );
        assert!(!missing(&filed).iter().any(|item| item.contains("engagement letter")), "{filed:#}");
        let text = assert_cmd::Command::cargo_bin("navigator")
            .unwrap()
            .current_dir(site.root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", site.root.path().join("credentials.json"))
            .args(["project", "status", &site.project.code, "--host", &site.host])
            .assert()
            .success();
        let text = String::from_utf8_lossy(&text.get_output().stdout).into_owned();
        assert!(text.contains("onboarding certificate: 2026.11.02_engagement-letter_certificate.pdf"), "{text}");
    })
    .await;
}
