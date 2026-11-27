use super::*;

async fn postgres_fixture() -> Option<(crate::postgres::PostgresDb, ApplicationStore, Uuid, Uuid)> {
    use crate::postgres::fixtures::{activate_application, firm_for_notation, workflow_fixture};
    use crate::postgres::fixtures::{connect, fresh_schema};

    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    activate_application(&pg).await;
    let (person, notation, _) = workflow_fixture(&pg).await;
    let (_, project) = firm_for_notation(&pg, notation).await;
    sqlx::query("UPDATE person SET name = 'A. Client', title = 'Founder', profile_image_url = '/images/testimonial.webp' WHERE id = $1")
        .bind(person)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE project SET name = 'Published matter', brand = 'neon' WHERE id = $1")
        .bind(project)
        .execute(pg.pool())
        .await
        .unwrap();
    Some((
        pg,
        crate::application_store::ApplicationStore::uninitialized(),
        person,
        project,
    ))
}

async fn with_postgres<T>(
    pg: crate::postgres::PostgresDb,
    future: impl std::future::Future<Output = T>,
) -> T {
    crate::postgres::PostgresDb::with_test_db(
        pg.clone(),
        crate::postgres::tenancy::with_test_authority(
            pg.clone(),
            true,
            crate::postgres::matter::with_test_db(pg, future),
        ),
    )
    .await
}

#[tokio::test]
async fn published_reads_require_consent_and_publication() {
    let Some((pg, application_store, person, project)) = postgres_fixture().await else {
        return;
    };
    with_postgres(pg.clone(), async {
        for (quote, published_at, display_order) in [
            ("Published quote.", Some("2026-06-24T00:00:00Z".into()), 1),
            ("Draft quote.", None, 0),
        ] {
            find_or_create(
                &application_store,
                &NewTestimonial {
                    project_id: project,
                    person_id: person,
                    quote,
                    attribution_label: None,
                    consented_at: Some("2026-06-23T00:00:00Z".into()),
                    published_at,
                    display_order,
                },
            )
            .await
            .unwrap();
        }
        let rows = published_for_home(&application_store, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].quote, "Published quote.");
        assert_eq!(
            published_for_brand(&application_store, "neon", Some(10))
                .await
                .unwrap(),
            rows
        );
        assert!(
            published_for_brand(&application_store, "delete-your-data", Some(10))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            published_for_brand(&application_store, "neon", None)
                .await
                .unwrap(),
            rows
        );
        let public = format!("{:?}", rows[0]);
        assert!(
            !public.contains("Published matter"),
            "public testimonial data must not carry its matter title"
        );
        assert!(
            !public.contains("A. Client"),
            "public projection must not carry the stored Person name: {public}"
        );
        assert!(
            !public.contains("Founder"),
            "public projection must not fall back to the stored Person title: {public}"
        );
        assert!(
            !public.contains("/images/testimonial.webp"),
            "public projection must not carry a profile image: {public}"
        );
        assert!(rows[0].attribution_label.is_none());
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn public_projection_uses_chosen_attribution_and_omits_blank_identity() {
    let Some((pg, application_store, person, project)) = postgres_fixture().await else {
        return;
    };
    sqlx::query("UPDATE person SET name = 'Stored Identity', title = 'Stored Title', profile_image_url = '/images/stored-identity.webp' WHERE id = $1")
        .bind(person)
        .execute(pg.pool())
        .await
        .unwrap();
    with_postgres(pg.clone(), async {
        find_or_create(
            &application_store,
            &NewTestimonial {
                project_id: project,
                person_id: person,
                quote: "Use the label I chose.",
                attribution_label: Some("Chosen Label".into()),
                consented_at: Some("2026-06-23T00:00:00Z".into()),
                published_at: Some("2026-06-24T00:00:00Z".into()),
                display_order: 1,
            },
        )
        .await
        .unwrap();
        find_or_create(
            &application_store,
            &NewTestimonial {
                project_id: project,
                person_id: person,
                quote: "Publish the quote without a name.",
                attribution_label: None,
                consented_at: Some("2026-06-23T00:00:00Z".into()),
                published_at: Some("2026-06-25T00:00:00Z".into()),
                display_order: 2,
            },
        )
        .await
        .unwrap();
        let rows = published_for_home(&application_store, 10).await.unwrap();
        assert_eq!(rows.len(), 2);
        let chosen = rows
            .iter()
            .find(|row| row.quote == "Use the label I chose.")
            .expect("chosen attribution row");
        assert_eq!(chosen.attribution_label.as_deref(), Some("Chosen Label"));
        let blank = rows
            .iter()
            .find(|row| row.quote == "Publish the quote without a name.")
            .expect("blank attribution row");
        assert!(blank.attribution_label.is_none());
        let public = format!("{rows:?}");
        for leaked in [
            "Stored Identity",
            "Stored Title",
            "/images/stored-identity.webp",
        ] {
            assert!(
                !public.contains(leaked),
                "public projection leaked `{leaked}`: {public}"
            );
        }
    })
    .await;
    pg.pool().close().await;
}
