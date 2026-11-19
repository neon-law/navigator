use crate::delegations::{Basis, NewDelegation, NoticeChannel};
use crate::external_identities::{ExternalIdentityError, ExternalSystem};
use crate::person_phones::PhoneLabel;
use crate::persons::{NewPerson, PersonError};
use crate::postgres_mirror::{MirrorKey, Mirrored};
use crate::{postgres::PostgresDb, surreal::SurrealDb};
use uuid::Uuid;

async fn client(surreal: &SurrealDb, label: &str) -> crate::persons::Person {
    crate::persons::create(
        surreal,
        &NewPerson::new(
            "Synthetic Client",
            format!("{label}-{}@example.test", Uuid::now_v7()),
        ),
    )
    .await
    .unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one authority scenario writes every identity table, then merges across them"
)]
async fn people_authority_owns_identity_detail_aliases_and_merges() {
    let (options, database) = crate::postgres_mirror::tests::fresh_schema()
        .await
        .expect("required identity PostgreSQL fixture");
    let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
    crate::postgres::fixtures::activate_application(&pg).await;
    PostgresDb::with_test_db(pg.clone(), async {
        let surreal = SurrealDb::uninitialized();
        let keep = client(&surreal, "keep").await;
        let dropped = client(&surreal, "drop").await;
        let subject = client(&surreal, "subject").await;

        // Aliases share the primary mailbox claim transaction.
        let alias = format!("alias-{}@example.test", Uuid::now_v7());
        crate::persons::add_alias(&surreal, dropped.id, &alias.to_uppercase())
            .await
            .unwrap();
        crate::persons::add_alias(&surreal, dropped.id, &alias)
            .await
            .unwrap();
        for held in [alias.as_str(), dropped.email.as_str()] {
            assert!(matches!(
                crate::persons::add_alias(&surreal, keep.id, held).await,
                Err(PersonError::EmailTaken)
            ));
        }
        assert_eq!(
            crate::persons::aliases(&surreal, dropped.id).await.unwrap(),
            vec![alias.clone()]
        );
        assert_eq!(
            crate::persons::find_by_any_email(&surreal, &alias)
                .await
                .unwrap()
                .map(|person| person.id),
            Some(dropped.id)
        );

        // Catalog parents use the same PostgreSQL authority.
        let suffix = Uuid::now_v7().simple().to_string();
        let jurisdiction = crate::jurisdictions::create(
            &surreal,
            &crate::jurisdictions::NewJurisdiction::new(
                format!("Synthetic State {suffix}"),
                format!("S{}", &suffix[suffix.len() - 8..]),
                "state",
            ),
        )
        .await
        .unwrap()
        .id;
        let entity_type =
            crate::entity_types::create(&surreal, &format!("Synthetic Type {suffix}"))
                .await
                .unwrap()
                .id;

        // Entity: the unique index is the Firm anchor claim, and the projection
        // carries the claim SurrealDB readers resolve.
        let new_entity = |name: &str, anchor: Option<&str>| crate::entities::NewEntity {
            name: name.to_string(),
            entity_type_id: entity_type,
            jurisdiction_id: jurisdiction,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: anchor.map(str::to_string),
        };
        let anchor = crate::entities::create(
            &surreal,
            &new_entity("Synthetic Firm", Some("synthetic-anchor")),
        )
        .await
        .unwrap();
        assert!(matches!(
            crate::entities::create(
                &surreal,
                &new_entity("Second Firm", Some("synthetic-anchor"))
            )
            .await,
            Err(crate::entities::EntityError::FirmAnchorTaken)
        ));
        assert_eq!(
            crate::entities::firm_anchor_holder(&surreal, "synthetic-anchor")
                .await
                .unwrap(),
            Some(anchor.id)
        );

        assert_eq!(
            crate::entities::update(
                &surreal,
                anchor.id,
                &new_entity("Renamed Firm", Some("synthetic-anchor"))
            )
            .await
            .unwrap(),
            None,
            "a Firm anchor keeps its name"
        );
        crate::entities::seed_ein(&surreal, anchor.id, "00-0000000")
            .await
            .unwrap();
        let anchor = crate::entities::set_firm_anchor_key(&surreal, anchor.id, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            anchor.metadata,
            Some(serde_json::json!({"ein": "00-0000000"}))
        );
        assert!(
            !crate::entities::firm_anchor_exists(&surreal, "synthetic-anchor")
                .await
                .unwrap()
        );

        assert_eq!(
            crate::entities::find_by_identity(
                &surreal,
                "Synthetic Firm",
                entity_type,
                jurisdiction
            )
            .await
            .unwrap()
            .map(|entity| entity.id),
            Some(anchor.id)
        );

        // Address.
        let address = crate::addresses::create(
            &surreal,
            &crate::addresses::NewAddress {
                person_id: Some(dropped.id),
                entity_id: Some(anchor.id),
                line1: "1 Synthetic Way".into(),
                city: "Example City".into(),
                region: "NV".into(),
                postal_code: "89000".into(),
                country: "US".into(),
                ..crate::addresses::NewAddress::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            crate::addresses::for_entity(&surreal, anchor.id)
                .await
                .unwrap(),
            vec![address.clone()]
        );
        assert_eq!(crate::addresses::count(&surreal).await.unwrap(), 1);

        // Credential: the kept Person holds the same licence, so the merge keeps
        // one row.
        crate::credentials::grant(&surreal, dropped.id, jurisdiction, "LIC-1")
            .await
            .unwrap();
        assert!(matches!(
            crate::credentials::grant(&surreal, dropped.id, jurisdiction, "LIC-2").await,
            Err(crate::credentials::CredentialError::AlreadyGranted)
        ));
        crate::credentials::find_or_grant(&surreal, keep.id, jurisdiction, "LIC-3")
            .await
            .unwrap();
        assert!(
            crate::credentials::is_licensed_in(&surreal, dropped.id, jurisdiction)
                .await
                .unwrap()
        );

        // Email token: single use and expiry are PostgreSQL predicates.
        let now = chrono::Utc::now();
        let token = crate::email_tokens::mint(
            &surreal,
            dropped.id,
            &dropped.email,
            crate::email_tokens::PURPOSE_EMAIL_CONFIRM,
            "synthetic-plaintext",
            now + chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        assert!(crate::email_tokens::has_live_token_since(
            &surreal,
            dropped.id,
            crate::email_tokens::PURPOSE_EMAIL_CONFIRM,
            now - chrono::Duration::minutes(1),
            now,
        )
        .await
        .unwrap());
        let found = crate::email_tokens::validate(
            &surreal,
            "synthetic-plaintext",
            crate::email_tokens::PURPOSE_EMAIL_CONFIRM,
            now,
        )
        .await
        .unwrap();
        assert_eq!(found.map(|row| row.id), Some(token.id));
        crate::email_tokens::consume(&surreal, token.id, now)
            .await
            .unwrap();
        assert!(crate::email_tokens::validate(
            &surreal,
            "synthetic-plaintext",
            crate::email_tokens::PURPOSE_EMAIL_CONFIRM,
            now,
        )
        .await
        .unwrap()
        .is_none());

        // External identities: both Persons hold GitHub, so the dropped one's
        // goes in the merge; Slack moves.
        crate::external_identities::link(
            &surreal,
            keep.id,
            ExternalSystem::Github,
            "gh-keep",
            None,
        )
        .await
        .unwrap();
        crate::external_identities::link(
            &surreal,
            dropped.id,
            ExternalSystem::Github,
            "gh-drop",
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            crate::external_identities::link(
                &surreal,
                subject.id,
                ExternalSystem::Github,
                "gh-drop",
                None
            )
            .await,
            Err(ExternalIdentityError::AccountTaken { .. })
        ));
        crate::external_identities::set_for_person(
            &surreal,
            dropped.id,
            ExternalSystem::Slack,
            Some("U-OLD"),
        )
        .await
        .unwrap();
        let _slack = crate::external_identities::set_for_person(
            &surreal,
            dropped.id,
            ExternalSystem::Slack,
            Some("U-NEW"),
        )
        .await
        .unwrap()
        .unwrap();

        // Phones: the first is primary; a new primary demotes it in both stores.
        let first = crate::person_phones::set_phone(
            &surreal,
            dropped.id,
            "(510) 555-0100",
            PhoneLabel::Mobile,
            false,
        )
        .await
        .unwrap();
        assert!(first.is_primary);
        let second = crate::person_phones::set_phone(
            &surreal,
            dropped.id,
            "+1 510 555 0101",
            PhoneLabel::Work,
            true,
        )
        .await
        .unwrap();
        let listed = crate::person_phones::list_phones(&surreal, dropped.id)
            .await
            .unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|phone| (phone.id, phone.is_primary))
                .collect::<Vec<_>>(),
            vec![(second.id, true), (first.id, false)]
        );

        assert_eq!(
            crate::person_phones::remove_phone(&surreal, dropped.id, "510-555-0101")
                .await
                .unwrap()
                .as_deref(),
            Some("+15105550101")
        );

        // Delegation: instants are timestamps here and RFC 3339 text in the
        // projection.
        let delegation = crate::delegations::grant(
            &surreal,
            &NewDelegation {
                delegate_person_id: dropped.id,
                subject_person_id: subject.id,
                basis: Basis::Assistant,
                instrument_reference: None,
                granted_by_person_id: keep.id,
                subject_notified_at: None,
                subject_notified_channel: None,
            },
        )
        .await
        .unwrap();
        crate::delegations::record_subject_notice(&surreal, delegation.id, NoticeChannel::Email)
            .await
            .unwrap();
        assert_eq!(
            crate::delegations::live_for_subject(&surreal, subject.id)
                .await
                .unwrap()
                .len(),
            1
        );
        let granted_at: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT granted_at FROM person_delegate WHERE id=$1")
                .bind(delegation.id)
                .fetch_optional(pg.pool())
                .await
                .unwrap();
        assert_eq!(
            granted_at,
            Some(
                chrono::DateTime::parse_from_rfc3339(&delegation.granted_at)
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );

        // The legacy mirror never copies a projection back over PostgreSQL.
        for key in [
            MirrorKey::Entity(anchor.id),
            MirrorKey::Address(address.id),
            MirrorKey::PersonMailbox(dropped.id),
            MirrorKey::Record {
                table: "person_phone",
                id: first.id,
            },
        ] {
            assert_eq!(
                crate::postgres_mirror::mirror(&surreal, &pg, &key)
                    .await
                    .unwrap(),
                Mirrored::Unchanged,
                "{key}"
            );
        }

        // The merge moves every PostgreSQL link and keeps every address.
        let report = crate::person_merge::merge(&surreal, keep.id, dropped.id)
            .await
            .unwrap();
        let mut expected = vec![
            keep.email.to_lowercase(),
            dropped.email.to_lowercase(),
            alias.clone(),
        ];
        expected.sort();
        assert_eq!(report.emails, expected);
        assert!(crate::persons::find_by_id(&surreal, dropped.id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            crate::persons::find_by_any_email(&surreal, &dropped.email)
                .await
                .unwrap()
                .map(|person| person.id),
            Some(keep.id)
        );

        let held = |sql: &'static str| {
            let pg = pg.clone();
            async move {
                sqlx::query_scalar::<_, i64>(sql)
                    .bind(keep.id)
                    .fetch_one(&pg.0)
                    .await
                    .unwrap()
            }
        };
        assert_eq!(
            held("SELECT count(*) FROM credential WHERE person_id = $1").await,
            1
        );
        assert_eq!(
            held("SELECT count(*) FROM address WHERE person_id = $1").await,
            1
        );
        assert_eq!(
            held("SELECT count(*) FROM email_token WHERE person_id = $1").await,
            1
        );
        assert_eq!(
            held("SELECT count(*) FROM person_phone WHERE person_id = $1").await,
            1
        );
        assert_eq!(
            held("SELECT count(*) FROM person_delegate WHERE delegate_person_id = $1").await,
            1
        );
        let systems: Vec<String> = sqlx::query_scalar(
            "SELECT system || ':' || external_id FROM person_external_identity \
         WHERE person_id = $1 ORDER BY system",
        )
        .bind(keep.id)
        .fetch_all(&pg.0)
        .await
        .unwrap();
        assert_eq!(systems, vec!["github:gh-keep", "slack:U-NEW"]);
        pg.0.close().await;
    })
    .await;
}
