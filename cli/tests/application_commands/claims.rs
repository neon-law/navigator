//! A Legal Plan claim packet and an email-filed claim, end to end: the CLI
//! binary, the portal's real handlers, object storage, and PostgreSQL
//! (LAW-585).
use chrono::{Duration, Utc};
use serde_json::Value;
use store::{
    firms::{self, FirmMembership, NewFirm, NewPersonFirmRole},
    legal_plans::{self, NewLegalPlan},
    persons::{self, NewPerson, Role},
    projects::{self, PlanColumns},
};
use uuid::Uuid;

use crate::live::LiveSite;

async fn plan_matter() -> (LiveSite, Uuid) {
    let pg = store::postgres::PostgresDb::application().await.unwrap();
    let db = store::application_store::ApplicationStore::uninitialized();
    let entity: Uuid = sqlx::query_scalar(
        "WITH kind AS (INSERT INTO entity_type(name) VALUES ('Synthetic Packet Entity Type') RETURNING id),
          place AS (INSERT INTO jurisdiction(name) VALUES ('Synthetic Packet Jurisdiction') RETURNING id)
          INSERT INTO entity(name,entity_type_id,jurisdiction_id)
          SELECT 'Synthetic Billing Entity LLC',kind.id,place.id FROM kind, place RETURNING id",
    )
    .fetch_one(pg.pool())
    .await
    .unwrap();
    let person = |name: &str, role| {
        NewPerson::with_role(
            name,
            format!("{}@example.com", Uuid::now_v7().simple()),
            role,
        )
    };
    let admin = persons::create(&db, &person("Synthetic Admin", Role::Admin))
        .await
        .unwrap()
        .id;
    let lawyer = persons::create(&db, &person("Synthetic Packet Lawyer", Role::Lawyer))
        .await
        .unwrap()
        .id;
    let firm = firms::create(
        &db,
        &NewFirm {
            name: format!("Synthetic Packet Firm {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id: entity,
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap()
    .id;
    firms::ensure_membership(
        &db,
        &NewPersonFirmRole {
            person_id: lawyer,
            firm_id: firm,
            membership: FirmMembership::Admin,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    // The responsible lawyer sees the claim's next action in `navigator billing`.
    let site = LiveSite::start(Some(firm), Some(lawyer), true).await;

    let plan = legal_plans::create(
        &pg,
        firm,
        &NewLegalPlan {
            name: "LegalEASE".into(),
            contact: None,
            portal: None,
            acceptance_sla_hours: Some(24),
            turnaround_sla_days: None,
            reporting_requirements: None,
        },
    )
    .await
    .unwrap();
    projects::set_plan_columns(
        &db,
        &pg,
        site.project.id,
        &PlanColumns {
            legal_plan_id: Some(plan.id),
            plan_case_number: Some("SYN-CASE-7".into()),
            plan_member_id: Some("SYN-MEMBER-3".into()),
            benefit_code: Some("LELG".into()),
            ..PlanColumns::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    (site, firm)
}

/// File a synthetic signed engagement letter: the proof the plan asks for.
async fn file_signed_letter(site: &LiveSite) {
    let letter = pdf::render("Synthetic signed engagement letter").unwrap();
    let key = format!("projects/{}/synthetic/letter_signed.pdf", site.project.id);
    site.storage
        .put(&key, &letter, "application/pdf")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO asset (storage_key,content_type,byte_size,sha256_hex,project_id,filename,kind,source,slug,signing)
         VALUES ($2,'application/pdf',$3,$4,$1,'2026.11.02_engagement-letter_signed.pdf','onboarding','docusign',
         '2026.11.02_engagement-letter_signed.pdf','{\"method\":\"docusign\",\"signed_on\":\"2026-11-02\"}'::jsonb)",
    )
    .bind(site.project.id)
    .bind(&key)
    .bind(i64::try_from(letter.len()).unwrap())
    .bind(pdf_sha(&letter))
    .execute(site.pg.pool())
    .await
    .unwrap();
}

fn pdf_sha(bytes: &[u8]) -> String {
    store::assets::sha256_hex(bytes)
}

fn worklist(site: &LiveSite) -> String {
    let output = site.navigator(&["billing"]).assert().success();
    String::from_utf8_lossy(&output.get_output().stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // The acceptance walk: rules, work, packet, filing, reference.
async fn a_prepared_claim_files_its_packet_and_is_filed_by_its_sent_message() {
    store::test_support::with_postgres(async {
        let (site, firm) = plan_matter().await;
        file_signed_letter(&site).await;
        let rules = assert_cmd::Command::cargo_bin("navigator")
            .unwrap()
            .current_dir(site.root.path())
            .env(
                "NAVIGATOR_CREDENTIALS_FILE",
                site.root.path().join("credentials.json"),
            )
            .args([
                "legal-plans",
                "--firm",
                &firm.to_string(),
                "--host",
                &site.host,
                "claim-filing",
                "--plan",
                "legalease",
                "--file-with",
                "claims@example.com",
                "--subject",
                "Claim Submission {case_number}",
                "--invoice-to",
                "invoices@example.com",
                "--form-title",
                "Synthetic LegalGuard claim form",
                "--proof",
                "engagement-letter",
            ])
            .assert()
            .success();
        let rules: Value = serde_json::from_slice(&rules.get_output().stdout).unwrap();
        assert_eq!(
            rules["claim_filing"]["proof"],
            serde_json::json!(["engagement_letter"])
        );

        let served = (Utc::now().date_naive() - Duration::days(3)).to_string();
        let work = site.json(&[
            "billing",
            "work",
            "--service",
            "legal-ease-legal-guard",
            "--benefit",
            "Document review",
            "--performed-on",
            &served,
            "--attorney-minutes",
            "90",
            "--authorization-reference",
            "synthetic-authorization",
            "--rate-cents-per-hour",
            "12000",
            "--covered-minutes",
            "60",
        ]);
        let claim = work["id"].as_str().unwrap().to_owned();
        site.json(&["billing", "prepare", "--claim", &claim, "--on", &served]);

        // The benefit's filing rule in effect on the service date decides
        // where the claim is filed, over the plan-wide claim filing.
        sqlx::query(
            "INSERT INTO legal_plan_filing_rule (legal_plan_id,benefit_code,effective_on,trigger,
             filing_destination,filer) SELECT legal_plan_id,'Document review',$2::date,
             'service_completed','https://claims.example.com/upload','Firm' FROM project WHERE id=$1",
        )
        .bind(site.project.id)
        .bind(&served)
        .execute(site.pg.pool())
        .await
        .unwrap();
        let out = site.root.path().join("packet.pdf");
        let out_arg = out.display().to_string();
        let filed = site.json(&["billing", "packet", "--claim", &claim, "--out", &out_arg]);
        assert_eq!(filed["subject"], "Claim Submission SYN-CASE-7");
        assert_eq!(filed["file_with"], "https://claims.example.com/upload");
        assert_eq!(filed["filer"], "Firm");
        assert_eq!(filed["missing_proof"], serde_json::json!([]));
        assert!(filed.get("pdf_base64").is_none(), "the PDF goes to --out");
        let packet = std::fs::read(&out).unwrap();
        assert_eq!(pdf_sha(&packet), filed["sha256"]);
        assert_eq!(
            pdf::page_count(&packet).unwrap(),
            4,
            "form, invoice, proof page, signed letter"
        );
        for (needle, page) in [
            ("Synthetic LegalGuard claim form", 0),
            ("SYN-MEMBER-3", 0),
            ("LELG", 0),
            ("Synthetic Billing Entity LLC", 1),
            ("$180.00", 1),
            ("Covered by the plan", 1),
            ("$60.00", 1),
            ("2026.11.02_engagement-letter_signed.pdf", 2),
            ("Synthetic signed engagement letter", 3),
        ] {
            let found =
                pdf::locate(&packet, needle, 1).unwrap_or_else(|error| panic!("{needle}: {error}"));
            assert_eq!(found.rects[0].page_index, page, "{needle}");
        }
        // The covered amount: on the form, as the hourly rate, and as the plan's share.
        assert_eq!(pdf::occurrence_count(&packet, "$120.00").unwrap(), 3);
        let asset = filed["asset_id"].as_str().unwrap().parse::<Uuid>().unwrap();
        let (kind, visibility): (String, String) =
            sqlx::query_as("SELECT kind,visibility FROM asset WHERE id=$1 AND project_id=$2")
                .bind(asset)
                .bind(site.project.id)
                .fetch_one(site.pg.pool())
                .await
                .unwrap();
        assert_eq!(
            (kind.as_str(), visibility.as_str()),
            ("invoice", "internal")
        );
        assert_eq!(
            filed["claim"]["state"]["packet_asset_id"],
            asset.to_string()
        );
        let again = site.json(&["billing", "packet", "--claim", &claim, "--out", &out_arg]);
        assert_eq!(
            again["asset_id"], filed["asset_id"],
            "the same packet files once"
        );
        let before = worklist(&site);
        assert!(
            before.contains("File claim and record submission"),
            "{before}"
        );

        site.navigator(&["billing", "submit", "--claim", &claim, "--on", &served])
            .assert()
            .failure();
        let submitted = site.json(&[
            "billing",
            "submit",
            "--claim",
            &claim,
            "--on",
            &served,
            "--sent-message",
            "18c0ffee0001",
        ]);
        assert_eq!(submitted["state"]["stage"], "submitted");
        assert_eq!(submitted["state"]["sent_message_id"], "18c0ffee0001");
        assert!(submitted["state"]["provider_reference"].is_null());
        let text = worklist(&site);
        assert!(
            !text.contains("Prepare claim") && !text.contains("File claim"),
            "{text}"
        );
        assert!(
            text.contains("Check provider decision or payment"),
            "{text}"
        );
        let referenced = site.json(&[
            "billing",
            "reference",
            "--claim",
            &claim,
            "--on",
            &served,
            "--provider-reference",
            "SYN-CLAIM-42",
        ]);
        assert_eq!(referenced["state"]["provider_reference"], "SYN-CLAIM-42");
        site.navigator(&["billing", "packet", "--claim", &claim, "--out", &out_arg])
            .assert()
            .success();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_packet_names_the_proof_the_project_lacks() {
    store::test_support::with_postgres(async {
        let (site, _) = plan_matter().await;
        let served = (Utc::now().date_naive() - Duration::days(1)).to_string();
        let work = site.json(&[
            "billing",
            "work",
            "--service",
            "legal-ease-legal-guard",
            "--benefit",
            "Letter",
            "--performed-on",
            &served,
            "--attorney-minutes",
            "30",
            "--authorization-reference",
            "synthetic-fixed",
            "--fixed-cents",
            "15000",
        ]);
        let claim = work["id"].as_str().unwrap().to_owned();
        let out = site.root.path().join("unproven.pdf");
        let output = site
            .navigator(&[
                "billing",
                "packet",
                "--claim",
                &claim,
                "--out",
                &out.display().to_string(),
            ])
            .assert()
            .success();
        let filed: Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
        assert_eq!(
            filed["missing_proof"],
            serde_json::json!(["engagement_letter"])
        );
        assert!(String::from_utf8_lossy(&output.get_output().stderr)
            .contains("not filed on the Project"));
        let packet = std::fs::read(&out).unwrap();
        assert_eq!(pdf::page_count(&packet).unwrap(), 3);
        assert_eq!(
            pdf::occurrence_count(&packet, "Fixed fee $150.00").unwrap(),
            1
        );
        site.navigator(&[
            "billing",
            "packet",
            "--claim",
            &Uuid::now_v7().to_string(),
            "--out",
            &out.display().to_string(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("404"));
    })
    .await;
}
