//! Project Slack reconciliation uses only Lawyer DRIs and participating Clerks.

use store::persons::{self, NewPerson, Role};
use store::projects::{
    self, create, designate_dri_in_surreal, set_internal_slack_channel_id, DriSide, NewProject,
};
use store::test_support::{mem_surreal, seed_entity};

#[tokio::test]
async fn targets_include_lawyer_dris_and_clerks_but_skip_clients_and_keep_unconfigured_projects() {
    let surreal = mem_surreal().await;
    let entity_id = seed_entity(&surreal).await;
    let project = create(
        &surreal,
        &NewProject {
            code: "slack-sync".to_string(),
            name: "Slack Sync".to_string(),
            status: "open".to_string(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .expect("create Project");
    set_internal_slack_channel_id(&surreal, project.id, "C123456")
        .await
        .expect("set channel")
        .expect("Project exists");
    create(
        &surreal,
        &NewProject {
            code: "slack-sync-unconfigured".to_string(),
            name: "Slack Sync Unconfigured".to_string(),
            status: "open".to_string(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .expect("create unconfigured Project");

    let lawyer = persons::create(
        &surreal,
        &NewPerson::with_role("Lawyer DRI", "lawyer@example.com", Role::Lawyer),
    )
    .await
    .expect("create lawyer");
    let clerk = persons::create(
        &surreal,
        &NewPerson::with_role("Project Clerk", "clerk@example.com", Role::Clerk),
    )
    .await
    .expect("create Clerk");
    let client = persons::create(
        &surreal,
        &NewPerson::with_role("Client DRI", "client@example.com", Role::Client),
    )
    .await
    .expect("create client");

    projects::add_participation(&surreal, project.id, lawyer.id, "lawyer")
        .await
        .expect("add lawyer");
    designate_dri_in_surreal(&surreal, project.id, lawyer.id, DriSide::Lawyer)
        .await
        .expect("designate Lawyer DRI");
    projects::add_participation(&surreal, project.id, clerk.id, "clerk")
        .await
        .expect("add Clerk");
    projects::add_participation(&surreal, project.id, client.id, "client")
        .await
        .expect("add client");

    let targets = projects::slack_membership_targets(&surreal)
        .await
        .expect("list Slack targets");
    assert_eq!(targets.projects, 2);
    assert_eq!(targets.targets.len(), 2);
    let configured = targets
        .targets
        .iter()
        .find(|target| target.code == "slack-sync")
        .expect("configured target");
    assert_eq!(configured.channel_id.as_deref(), Some("C123456"));
    assert_eq!(configured.project_id, project.id);
    assert_eq!(configured.status, "open");
    let unconfigured = targets
        .targets
        .iter()
        .find(|target| target.code == "slack-sync-unconfigured")
        .expect("unconfigured target");
    assert_eq!(unconfigured.channel_id, None);
    assert!(unconfigured.member_emails.is_empty());
    assert_eq!(
        configured.member_emails,
        ["clerk@example.com", "lawyer@example.com"]
    );
}

#[tokio::test]
async fn recorded_slack_ids_include_owning_firm_lawyers_and_clerks_without_assignment() {
    use store::external_identities::{link, ExternalSystem};
    use store::firms::{self, FirmMembership, NewFirm, NewPersonFirmRole};
    let db = mem_surreal().await;
    let entity_id = seed_entity(&db).await;
    let admin = persons::create(
        &db,
        &NewPerson::with_role("Admin", "admin@example.com", Role::Admin),
    )
    .await
    .unwrap();
    let firm = firms::create(
        &db,
        &NewFirm {
            name: "Synthetic Practice".into(),
            status: "active".into(),
            entity_id,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    let project = create(
        &db,
        &NewProject {
            code: "firm-slack".into(),
            name: "Firm Slack".into(),
            status: "open".into(),
            entity_id,
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    for (name, role, member, slack_id) in [
        ("lawyer", Role::Lawyer, true, "U-LAWYER"),
        ("clerk", Role::Clerk, true, "U-CLERK"),
        ("outside", Role::Lawyer, false, "U-OUTSIDE"),
        ("client", Role::Client, false, "U-CLIENT"),
    ] {
        let person = persons::create(
            &db,
            &NewPerson::with_role(name, format!("{name}@example.com"), role),
        )
        .await
        .unwrap();
        if member {
            firms::add_membership(
                &db,
                &NewPersonFirmRole {
                    person_id: person.id,
                    firm_id: firm.id,
                    membership: FirmMembership::for_role(role).unwrap(),
                    is_dri: false,
                },
            )
            .await
            .unwrap();
        }
        link(&db, person.id, ExternalSystem::Slack, slack_id, None)
            .await
            .unwrap();
    }
    let targets = projects::slack_membership_targets(&db).await.unwrap();
    let target = targets
        .targets
        .iter()
        .find(|t| t.project_id == project.id)
        .unwrap();
    assert_eq!(target.member_slack_ids, ["U-CLERK", "U-LAWYER"]);
    assert!(target.member_emails.is_empty());
}

#[tokio::test]
async fn one_target_matches_the_sweep_and_a_missing_project_has_none() {
    let surreal = mem_surreal().await;
    let entity_id = seed_entity(&surreal).await;
    let project = create(
        &surreal,
        &NewProject {
            code: "slack-single".to_string(),
            name: "Slack Single".to_string(),
            status: "open".to_string(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .expect("create Project");
    let clerk = persons::create(
        &surreal,
        &NewPerson::with_role("Project Clerk", "clerk@example.com", Role::Clerk),
    )
    .await
    .expect("create Clerk");
    projects::add_participation(&surreal, project.id, clerk.id, "clerk")
        .await
        .expect("add Clerk");

    let one = projects::slack_membership_target(&surreal, project.id)
        .await
        .expect("read one target")
        .expect("Project exists");
    let all = projects::slack_membership_targets(&surreal)
        .await
        .expect("read every target");
    assert_eq!(all.targets, std::slice::from_ref(&one));
    assert_eq!(one.member_emails, ["clerk@example.com"]);

    assert!(
        projects::slack_membership_target(&surreal, uuid::Uuid::new_v4())
            .await
            .expect("read a missing target")
            .is_none()
    );
}

#[tokio::test]
async fn the_canvas_reads_both_dri_sides_but_never_adds_a_client_to_the_invite_list() {
    let surreal = mem_surreal().await;
    let entity_id = seed_entity(&surreal).await;
    let project = create(
        &surreal,
        &NewProject {
            code: "sample-litigation".to_string(),
            name: "Cruller v. Prine".to_string(),
            status: "open".to_string(),
            description: Some("A synthetic matter.".to_string()),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .expect("create Project");
    set_internal_slack_channel_id(&surreal, project.id, "CINTERNAL")
        .await
        .expect("set channel")
        .expect("Project exists");
    let lawyer = persons::create(
        &surreal,
        &NewPerson::with_role("Lawyer DRI", "lawyer@example.com", Role::Lawyer),
    )
    .await
    .expect("create lawyer");
    let client = persons::create(
        &surreal,
        &NewPerson::with_role("Client DRI", "client@example.com", Role::Client),
    )
    .await
    .expect("create client");
    for (person, participation, side) in [
        (&lawyer, "lawyer", DriSide::Lawyer),
        (&client, "client", DriSide::Client),
    ] {
        projects::add_participation(&surreal, project.id, person.id, participation)
            .await
            .expect("add participation");
        designate_dri_in_surreal(&surreal, project.id, person.id, side)
            .await
            .expect("designate DRI");
    }

    let canvas = projects::project_canvas(&surreal, project.id)
        .await
        .expect("read canvas")
        .expect("Project exists");
    assert_eq!(canvas.code, "sample-litigation");
    assert_eq!(canvas.description.as_deref(), Some("A synthetic matter."));
    assert_eq!(canvas.internal_channel_id.as_deref(), Some("CINTERNAL"));
    assert_eq!(canvas.client_dri_emails, ["client@example.com"]);
    assert_eq!(canvas.lawyer_dris.len(), 1);
    assert_eq!(canvas.lawyer_dris[0].email, "lawyer@example.com");

    let target = projects::slack_membership_target(&surreal, project.id)
        .await
        .expect("read target")
        .expect("Project exists");
    assert!(!target
        .member_emails
        .contains(&"client@example.com".to_string()));

    assert!(projects::project_canvas(&surreal, uuid::Uuid::new_v4())
        .await
        .expect("read a missing canvas")
        .is_none());
}
