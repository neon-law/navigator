//! A Firm's e-signature provider: who may choose it, and that the choice
//! never reaches another Firm.
use super::super::PostgresDb;
use crate::firm_esign::{for_brand, for_firm, set_for_firm};
use crate::firms::FirmError;
use crate::persons::Role;
use crate::signatures::SignatureProvider::{DocuSign, DropboxSign};
use uuid::Uuid;

async fn firm(pg: &PostgresDb) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO firm(id, name, status, slug) VALUES ($1, 'Synthetic', 'active', $2)")
        .bind(id)
        .bind(id.to_string())
        .execute(pg.pool())
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn only_an_owner_or_the_firms_own_admin_chooses_its_provider() {
    crate::test_support::with_required_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let (ours, theirs) = (firm(&pg).await, firm(&pg).await);
        let brand = format!("synthetic-{}", Uuid::new_v4().simple());
        sqlx::query("INSERT INTO brand(name, brand_key, firm_id) VALUES ($1, $1, $2)")
            .bind(&brand)
            .bind(ours)
            .execute(pg.pool())
            .await
            .unwrap();
        let admin = Uuid::new_v4();
        sqlx::query("INSERT INTO person(id, name, email) VALUES ($1, 'Synthetic', $2)")
            .bind(admin)
            .bind(format!("{admin}@example.invalid"))
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO person_firm_role(person_id, firm_id, membership) VALUES ($1, $2, 'admin')",
        )
        .bind(admin)
        .bind(ours)
        .execute(pg.pool())
        .await
        .unwrap();

        // No stored choice, an unknown Brand, and an unknown Firm all sign through DocuSign.
        assert_eq!(for_firm(ours).await.unwrap(), DocuSign);
        assert_eq!(for_brand(&brand).await.unwrap(), DocuSign);
        assert_eq!(for_brand("no-such-brand").await.unwrap(), DocuSign);
        assert_eq!(for_firm(Uuid::new_v4()).await.unwrap(), DocuSign);

        for (role, person, target) in [
            (Role::Admin, Some(admin), theirs),
            (Role::Admin, None, ours),
            (Role::Admin, Some(Uuid::new_v4()), ours),
            (Role::Lawyer, Some(admin), ours),
            (Role::Client, Some(admin), ours),
        ] {
            assert!(
                matches!(
                    set_for_firm(role, person, target, "dropbox_sign").await,
                    Err(FirmError::NotAuthorized)
                ),
                "{role:?} {person:?}"
            );
        }
        assert!(matches!(
            set_for_firm(Role::Owner, None, ours, "carrier_pigeon").await,
            Err(FirmError::InvalidEsignProvider)
        ));
        let missing = Uuid::new_v4();
        assert!(matches!(
            set_for_firm(Role::Owner, None, missing, "docusign").await,
            Err(FirmError::NoSuchFirm(id)) if id == missing
        ));
        assert_eq!(for_firm(ours).await.unwrap(), DocuSign);

        assert_eq!(
            set_for_firm(Role::Admin, Some(admin), ours, " dropbox_sign ")
                .await
                .unwrap(),
            DropboxSign
        );
        assert_eq!(for_firm(ours).await.unwrap(), DropboxSign);
        assert_eq!(for_brand(&brand).await.unwrap(), DropboxSign);
        assert_eq!(for_firm(theirs).await.unwrap(), DocuSign);
        assert_eq!(
            set_for_firm(Role::Owner, None, ours, "docusign")
                .await
                .unwrap(),
            DocuSign
        );
        assert_eq!(for_brand(&brand).await.unwrap(), DocuSign);
    })
    .await;
}

#[tokio::test]
async fn changing_office_suite_pauses_google_and_preserves_saved_settings() {
    use crate::firms::{
        calendar_owner, google_drive, set_calendar_owner, set_google_drive, set_workspace_provider,
        workspace_provider,
    };

    crate::test_support::with_required_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let (ours, theirs) = (firm(&pg).await, firm(&pg).await);
        let store = crate::application_store::ApplicationStore::from(pg);
        let mailbox = "calendar@example.test";

        set_calendar_owner(&store, Role::Owner, None, ours, mailbox)
            .await
            .unwrap();
        set_google_drive(&store, Role::Owner, None, ours, true)
            .await
            .unwrap();
        assert_eq!(workspace_provider(ours).await.unwrap(), "google_workspace");
        assert!(matches!(
            set_workspace_provider(&store, Role::Admin, None, ours, "microsoft_365").await,
            Err(FirmError::NotAuthorized)
        ));
        assert!(matches!(
            set_workspace_provider(&store, Role::Owner, None, ours, "unknown").await,
            Err(FirmError::InvalidWorkspaceProvider)
        ));
        set_workspace_provider(&store, Role::Owner, None, ours, "microsoft_365")
            .await
            .unwrap();
        assert_eq!(workspace_provider(ours).await.unwrap(), "microsoft_365");
        assert_eq!(
            workspace_provider(theirs).await.unwrap(),
            "google_workspace"
        );
        assert!(calendar_owner(ours).await.unwrap().is_none());
        assert!(!google_drive(ours).await.unwrap());

        set_workspace_provider(&store, Role::Owner, None, ours, "google_workspace")
            .await
            .unwrap();
        assert_eq!(
            calendar_owner(ours).await.unwrap().as_deref(),
            Some(mailbox)
        );
        assert!(google_drive(ours).await.unwrap());
    })
    .await;
}
