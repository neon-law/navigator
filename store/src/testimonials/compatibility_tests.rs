use super::*;
use crate::projects::{create, designate_dri_in_surreal, DriSide, NewProject};
use crate::test_support::mem_surreal;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn only_the_client_dri_can_submit_and_publication_is_two_step() {
    let surreal = mem_surreal().await;
    let client = persons::create(
        &surreal,
        &crate::persons::NewPerson::new("Synthetic Client", "testimonial-client@example.com"),
    )
    .await
    .unwrap();
    let other_client = persons::create(
        &surreal,
        &crate::persons::NewPerson::new("Synthetic Other", "testimonial-other@example.com"),
    )
    .await
    .unwrap();
    let lawyer = persons::create(
        &surreal,
        &crate::persons::NewPerson {
            role: persons::Role::Lawyer,
            ..crate::persons::NewPerson::new("Synthetic Lawyer", "testimonial-lawyer@example.com")
        },
    )
    .await
    .unwrap();
    let project = create(
        &surreal,
        &NewProject {
            code: "testimonial-workflow".into(),
            name: "Synthetic testimonial matter".into(),
            status: "open".into(),
            entity_id: Uuid::now_v7(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    designate_dri_in_surreal(&surreal, project.id, client.id, DriSide::Client)
        .await
        .unwrap();
    designate_dri_in_surreal(&surreal, project.id, lawyer.id, DriSide::Lawyer)
        .await
        .unwrap();

    let private = save_for_client_dri(
        &surreal,
        client.id,
        project.id,
        &TestimonialSubmission {
            quote: "A private note.",
            attribution_label: Some("Founder".into()),
            request_public: false,
        },
    )
    .await
    .unwrap();
    assert!(private.consented_at.is_none());
    assert!(private.published_at.is_none());
    assert!(published_for_home(&surreal, 10).await.unwrap().is_empty());
    assert!(matches!(
        save_for_client_dri(
            &surreal,
            other_client.id,
            project.id,
            &TestimonialSubmission {
                quote: "Not mine.",
                attribution_label: None,
                request_public: true,
            },
        )
        .await,
        Err(TestimonialError::NotAuthorized)
    ));

    let requested = save_for_client_dri(
        &surreal,
        client.id,
        project.id,
        &TestimonialSubmission {
            quote: "A public request.",
            attribution_label: Some("Founder".into()),
            request_public: true,
        },
    )
    .await
    .unwrap();
    assert!(requested.consented_at.is_some());
    assert!(requested.published_at.is_none());
    assert!(published_for_home(&surreal, 10).await.unwrap().is_empty());

    let published = publish(
        &surreal,
        Some(lawyer.id),
        persons::Role::Lawyer,
        requested.id,
    )
    .await
    .unwrap();
    assert!(published.published_at.is_some());
    assert_eq!(published_for_home(&surreal, 10).await.unwrap().len(), 1);

    let edited = save_for_client_dri(
        &surreal,
        client.id,
        project.id,
        &TestimonialSubmission {
            quote: "An edited public request.",
            attribution_label: Some("Founder".into()),
            request_public: true,
        },
    )
    .await
    .unwrap();
    assert!(edited.consented_at.is_some());
    assert!(edited.published_at.is_none());
    assert!(published_for_home(&surreal, 10).await.unwrap().is_empty());

    publish(&surreal, Some(lawyer.id), persons::Role::Lawyer, edited.id)
        .await
        .unwrap();

    let unpublished = unpublish(
        &surreal,
        Some(lawyer.id),
        persons::Role::Lawyer,
        requested.id,
    )
    .await
    .unwrap();
    assert!(unpublished.consented_at.is_some());
    assert!(unpublished.published_at.is_none());
    assert!(published_for_home(&surreal, 10).await.unwrap().is_empty());
}

struct BoundaryFixture {
    surreal: SurrealDb,
    client: persons::Person,
    other_client: persons::Person,
    lawyer: persons::Person,
    clerk: persons::Person,
    admin: persons::Person,
    owner: persons::Person,
    project: projects::Project,
    other_project: projects::Project,
}

async fn person_with_role(
    surreal: &SurrealDb,
    name: &str,
    email: &str,
    role: persons::Role,
) -> persons::Person {
    persons::create(
        surreal,
        &crate::persons::NewPerson {
            role,
            ..crate::persons::NewPerson::new(name, email)
        },
    )
    .await
    .unwrap()
}

async fn matter(surreal: &SurrealDb, code: &str, name: &str) -> projects::Project {
    create(
        surreal,
        &NewProject {
            code: code.into(),
            name: name.into(),
            status: "open".into(),
            entity_id: Uuid::now_v7(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

async fn boundary_fixture() -> BoundaryFixture {
    let surreal = mem_surreal().await;
    let client = person_with_role(
        &surreal,
        "Boundary Client",
        "testimonial-boundary-client@example.com",
        persons::Role::Client,
    )
    .await;
    let other_client = person_with_role(
        &surreal,
        "Boundary Other Client",
        "testimonial-boundary-other@example.com",
        persons::Role::Client,
    )
    .await;
    let lawyer = person_with_role(
        &surreal,
        "Boundary Lawyer",
        "testimonial-boundary-lawyer@example.com",
        persons::Role::Lawyer,
    )
    .await;
    let clerk = person_with_role(
        &surreal,
        "Boundary Clerk",
        "testimonial-boundary-clerk@example.com",
        persons::Role::Clerk,
    )
    .await;
    let admin = person_with_role(
        &surreal,
        "Boundary Admin",
        "testimonial-boundary-admin@example.com",
        persons::Role::Admin,
    )
    .await;
    let owner = person_with_role(
        &surreal,
        "Boundary Owner",
        "testimonial-boundary-owner@example.com",
        persons::Role::Owner,
    )
    .await;
    let project = matter(&surreal, "testimonial-boundary", "Boundary matter").await;
    let other_project = matter(
        &surreal,
        "testimonial-boundary-other",
        "Other boundary matter",
    )
    .await;
    designate_dri_in_surreal(&surreal, project.id, client.id, DriSide::Client)
        .await
        .unwrap();
    designate_dri_in_surreal(&surreal, project.id, lawyer.id, DriSide::Lawyer)
        .await
        .unwrap();
    designate_dri_in_surreal(&surreal, other_project.id, other_client.id, DriSide::Client)
        .await
        .unwrap();
    designate_dri_in_surreal(&surreal, other_project.id, lawyer.id, DriSide::Lawyer)
        .await
        .unwrap();
    for (person_id, participation) in [
        (clerk.id, "clerk"),
        (admin.id, "admin"),
        (owner.id, "owner"),
    ] {
        projects::add_participation(&surreal, project.id, person_id, participation)
            .await
            .unwrap();
    }
    BoundaryFixture {
        surreal,
        client,
        other_client,
        lawyer,
        clerk,
        admin,
        owner,
        project,
        other_project,
    }
}

fn public_request(quote: &str) -> TestimonialSubmission<'_> {
    TestimonialSubmission {
        quote,
        attribution_label: Some("Founder".into()),
        request_public: true,
    }
}

fn private_request(quote: &str) -> TestimonialSubmission<'_> {
    TestimonialSubmission {
        quote,
        attribution_label: Some("Founder".into()),
        request_public: false,
    }
}

async fn snapshot(surreal: &SurrealDb, person_id: Uuid, project_id: Uuid) -> Option<Testimonial> {
    for_person_project(surreal, person_id, project_id)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_client_cannot_submit_for_the_wrong_project() {
    let fixture = boundary_fixture().await;
    let saved = save_for_client_dri(
        &fixture.surreal,
        fixture.client.id,
        fixture.project.id,
        &public_request("Kept on the right matter."),
    )
    .await
    .unwrap();
    let before = snapshot(&fixture.surreal, fixture.client.id, fixture.project.id).await;

    assert!(matches!(
        save_for_client_dri(
            &fixture.surreal,
            fixture.client.id,
            fixture.other_project.id,
            &public_request("Wrong matter."),
        )
        .await,
        Err(TestimonialError::NotAuthorized)
    ));
    assert_eq!(
        snapshot(&fixture.surreal, fixture.client.id, fixture.project.id).await,
        before
    );
    assert!(snapshot(
        &fixture.surreal,
        fixture.client.id,
        fixture.other_project.id
    )
    .await
    .is_none());
    assert!(snapshot(
        &fixture.surreal,
        fixture.other_client.id,
        fixture.other_project.id
    )
    .await
    .is_none());
    assert_eq!(saved.quote, "Kept on the right matter.");
}

#[tokio::test]
async fn non_client_tiers_cannot_create_client_consent() {
    let fixture = boundary_fixture().await;
    for (person_id, role) in [
        (fixture.lawyer.id, persons::Role::Lawyer),
        (fixture.clerk.id, persons::Role::Clerk),
        (fixture.admin.id, persons::Role::Admin),
        (fixture.owner.id, persons::Role::Owner),
    ] {
        assert!(
            matches!(
                save_for_client_dri(
                    &fixture.surreal,
                    person_id,
                    fixture.project.id,
                    &public_request("Firm-written consent."),
                )
                .await,
                Err(TestimonialError::NotAuthorized)
            ),
            "{role:?} must not write client consent"
        );
        assert!(
            snapshot(&fixture.surreal, person_id, fixture.project.id)
                .await
                .is_none(),
            "{role:?} refusal must not insert a row"
        );
    }
    assert!(
        snapshot(&fixture.surreal, fixture.client.id, fixture.project.id)
            .await
            .is_none()
    );
    assert!(published_for_home(&fixture.surreal, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn admin_cannot_manufacture_or_overwrite_client_consent() {
    let fixture = boundary_fixture().await;
    let consented = save_for_client_dri(
        &fixture.surreal,
        fixture.client.id,
        fixture.project.id,
        &public_request("Client consent."),
    )
    .await
    .unwrap();
    let published = publish(
        &fixture.surreal,
        Some(fixture.lawyer.id),
        persons::Role::Lawyer,
        consented.id,
    )
    .await
    .unwrap();
    let before = snapshot(&fixture.surreal, fixture.client.id, fixture.project.id).await;

    assert!(matches!(
        save_for_client_dri(
            &fixture.surreal,
            fixture.admin.id,
            fixture.project.id,
            &public_request("Admin overwrite."),
        )
        .await,
        Err(TestimonialError::NotAuthorized)
    ));
    let after = snapshot(&fixture.surreal, fixture.client.id, fixture.project.id).await;
    assert_eq!(after, before);
    assert_eq!(
        after.as_ref().map(|row| row.quote.as_str()),
        Some("Client consent.")
    );
    assert_eq!(
        after.as_ref().and_then(|row| row.consented_at.as_deref()),
        published.consented_at.as_deref()
    );
    assert!(after.as_ref().is_some_and(|row| row.published_at.is_some()));
    assert_eq!(
        published_for_home(&fixture.surreal, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_client_cannot_publish_directly() {
    let fixture = boundary_fixture().await;
    let saved = save_for_client_dri(
        &fixture.surreal,
        fixture.client.id,
        fixture.project.id,
        &public_request("Waiting for approval."),
    )
    .await
    .unwrap();
    assert!(saved.published_at.is_none());
    let before = snapshot(&fixture.surreal, fixture.client.id, fixture.project.id).await;

    assert!(matches!(
        publish(
            &fixture.surreal,
            Some(fixture.client.id),
            persons::Role::Client,
            saved.id,
        )
        .await,
        Err(TestimonialError::NotAuthorized)
    ));
    assert_eq!(
        snapshot(&fixture.surreal, fixture.client.id, fixture.project.id).await,
        before
    );
    assert!(published_for_home(&fixture.surreal, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn revoking_consent_removes_a_published_testimonial_from_public_reads() {
    let fixture = boundary_fixture().await;
    let saved = save_for_client_dri(
        &fixture.surreal,
        fixture.client.id,
        fixture.project.id,
        &public_request("Publish me."),
    )
    .await
    .unwrap();
    publish(
        &fixture.surreal,
        Some(fixture.lawyer.id),
        persons::Role::Lawyer,
        saved.id,
    )
    .await
    .unwrap();
    assert_eq!(
        published_for_home(&fixture.surreal, 10)
            .await
            .unwrap()
            .len(),
        1
    );

    let revoked = save_for_client_dri(
        &fixture.surreal,
        fixture.client.id,
        fixture.project.id,
        &private_request("Keep this private now."),
    )
    .await
    .unwrap();
    assert!(revoked.consented_at.is_none());
    assert!(revoked.published_at.is_none());
    assert!(published_for_home(&fixture.surreal, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn editing_clears_publication_and_follows_the_submitted_consent_choice() {
    let fixture = boundary_fixture().await;
    let first = save_for_client_dri(
        &fixture.surreal,
        fixture.client.id,
        fixture.project.id,
        &public_request("First public request."),
    )
    .await
    .unwrap();
    publish(
        &fixture.surreal,
        Some(fixture.lawyer.id),
        persons::Role::Lawyer,
        first.id,
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let renewed = save_for_client_dri(
        &fixture.surreal,
        fixture.client.id,
        fixture.project.id,
        &public_request("Edited public request."),
    )
    .await
    .unwrap();
    assert_eq!(renewed.quote, "Edited public request.");
    assert!(renewed.published_at.is_none());
    assert!(renewed.consented_at.is_some());
    assert_ne!(renewed.consented_at, first.consented_at);
    assert!(published_for_home(&fixture.surreal, 10)
        .await
        .unwrap()
        .is_empty());

    let cleared = save_for_client_dri(
        &fixture.surreal,
        fixture.client.id,
        fixture.project.id,
        &private_request("Edited private note."),
    )
    .await
    .unwrap();
    assert_eq!(cleared.quote, "Edited private note.");
    assert!(cleared.consented_at.is_none());
    assert!(cleared.published_at.is_none());
    assert!(published_for_home(&fixture.surreal, 10)
        .await
        .unwrap()
        .is_empty());
}
