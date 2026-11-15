//! Manual Relationship Log observations through the portal graph endpoints.

use super::{
    bearer, body_json, build_fixture, get, lane, post, stage_store, Role, SessionStore, StatusCode,
    KEY,
};
use serde_json::json;
use store::firms::{FirmMembership, NewPersonFirmRole};
use store::postgres_mirror::{mirror, MirrorKey};
use uuid::Uuid;

fn person_id(bearer_header: &str) -> Uuid {
    SessionStore::new(KEY)
        .decode(bearer_header.strip_prefix("Bearer ").unwrap())
        .unwrap()
        .person_id
        .unwrap()
}

#[test]
fn project_observation_reaches_all_three_authorized_graph_endpoints() {
    lane().block_on(async {
        if stage_store().await.is_none() {
            return;
        }
        let fx = build_fixture().await;
        let lawyer_id = person_id(&fx.lawyer);
        let subject_id = person_id(&fx.client);
        let pg = store::postgres::PostgresDb::from_env().await.unwrap();
        store::firms::add_membership(
            &fx.surreal,
            &NewPersonFirmRole {
                person_id: lawyer_id,
                firm_id: fx.firm_id,
                membership: FirmMembership::Lawyer,
                is_dri: false,
            },
        )
        .await
        .unwrap();
        for key in [
            MirrorKey::Project(fx.project_id),
            MirrorKey::Person(subject_id),
            MirrorKey::FirmMembership {
                person_id: lawyer_id,
                firm_id: fx.firm_id,
            },
        ] {
            mirror(&fx.surreal, &pg, &key).await.unwrap();
        }

        let request_id = Uuid::now_v7();
        let path = "/app/api/graph/relationship-log";
        let payload = json!({
            "request_id": request_id, "scope": "project", "project_id": fx.project_id,
            "subject": "person", "subject_id": subject_id, "action": "witness",
            "detail": "Synthetic private detail",
        });
        assert_eq!(
            post(&fx, path, Some(&fx.outsider), payload.clone())
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            post(&fx, path, Some(&fx.clerk), payload.clone())
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        let mut spoofed = payload.clone();
        spoofed["actor_id"] = json!(subject_id);
        assert_eq!(
            post(&fx, path, Some(&fx.lawyer), spoofed).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        for _ in 0..2 {
            let response = post(&fx, path, Some(&fx.lawyer), payload.clone()).await;
            assert_eq!(response.status(), StatusCode::CREATED);
            assert_eq!(body_json(response).await["id"], json!(request_id));
        }
        for (view, seed) in [
            ("project", fx.project_id),
            ("lawyer", lawyer_id),
            ("firm", fx.firm_id),
        ] {
            let response = get(
                &fx,
                &format!("/app/api/graph/{view}/{seed}"),
                Some(&fx.lawyer),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            let observations = body["relationship_logs"].as_array().unwrap();
            assert_eq!(observations.len(), 1);
            assert_eq!(observations[0]["id"], json!(request_id));
            assert_eq!(observations[0]["subject_id"], json!(subject_id));
            assert_eq!(observations[0]["action"], "witness");
            assert!(observations[0].get("detail").is_none());
        }
        let owner = store::persons::create(
            &fx.surreal,
            &store::persons::NewPerson::with_role(
                "Synthetic Owner",
                format!("owner-{}@example.invalid", Uuid::now_v7()),
                Role::Owner,
            ),
        )
        .await
        .unwrap();
        let owner = bearer(owner.id, Role::Owner);
        assert_eq!(
            post(&fx, path, Some(&owner), payload).await.status(),
            StatusCode::NOT_FOUND
        );
        let firm = get(
            &fx,
            &format!("/app/api/graph/firm/{}", fx.firm_id),
            Some(&owner),
        )
        .await;
        assert_eq!(firm.status(), StatusCode::OK);
        assert_eq!(body_json(firm).await["relationship_logs"], json!([]));
    });
}
