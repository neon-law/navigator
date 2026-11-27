//! Real canonical rows shared by merge rollback and fixed-role acceptance.
use serde_json::{json, Value};
use store::application_store::ApplicationStore;
use store::postgres::PostgresDb;
use store::project_references::Party;
use store::relationships::{Endpoint, NewRelationship};
use uuid::Uuid;

pub struct Scenario {
    pub kept: Uuid,
    pub dropped: Uuid,
    pub target: store::projects::Project,
    holding: store::cap_table_holdings::CapTableHolding,
    untouched: store::cap_table_holdings::CapTableHolding,
    fiduciary: store::fiduciary_roles::FiduciaryRole,
    audit: store::relationship_logs::RelationshipLog,
    relationships: Vec<Value>,
    emails: Vec<String>,
}

async fn project(
    db: &ApplicationStore,
    parent: &store::projects::Project,
) -> store::projects::Project {
    store::projects::create(
        db,
        &store::projects::NewProject {
            code: format!("synthetic-merge-{}", Uuid::new_v4().simple()),
            name: "Synthetic merge matter".into(),
            entity_id: parent.entity_id,
            firm_id: parent.firm_id,
            brand: parent.brand.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

async fn typed_edges(
    db: &ApplicationStore,
    target: &store::projects::Project,
    kept: Uuid,
    dropped: Uuid,
    audit: Uuid,
) {
    store::entity_roles::grant(db, dropped, target.entity_id, "director")
        .await
        .unwrap();
    for (from, from_id, to, to_id, detail) in [
        (
            Endpoint::Person,
            dropped,
            Endpoint::Entity,
            target.entity_id,
            "outgoing retained provenance",
        ),
        (
            Endpoint::Entity,
            target.entity_id,
            Endpoint::Person,
            dropped,
            "incoming retained provenance",
        ),
        (
            Endpoint::Person,
            kept,
            Endpoint::Entity,
            target.entity_id,
            "existing kept provenance",
        ),
    ] {
        store::relationships::record(
            db,
            &NewRelationship {
                from,
                from_id,
                to,
                to_id,
                kind: store::relationships::KIND_ADVERSE_TO.into(),
                confidence_pct: 100,
                source_kind: store::relationships::SOURCE_MANUAL.into(),
                source_id: Some(audit),
                detail: Some(detail.into()),
            },
        )
        .await
        .unwrap();
    }
}

async fn relationship_rows(pg: &PostgresDb) -> Vec<Value> {
    sqlx::query_scalar("SELECT to_jsonb(r) FROM relationship r ORDER BY id")
        .fetch_all(pg.pool())
        .await
        .unwrap()
}

async fn link_identity(
    db: &ApplicationStore,
    target: &store::projects::Project,
    other: &store::projects::Project,
    kept: Uuid,
    dropped: Uuid,
) {
    store::firms::ensure_membership(
        db,
        &store::firms::NewPersonFirmRole {
            person_id: kept,
            firm_id: target.firm_id.unwrap(),
            membership: store::firms::FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    store::projects::add_participation(db, other.id, kept, "lawyer")
        .await
        .unwrap();
    store::projects::add_participation(db, target.id, dropped, "client")
        .await
        .unwrap();
    store::projects::designate_dri(db, target.id, dropped, store::projects::DriSide::Client)
        .await
        .unwrap();
}

impl Scenario {
    pub async fn seed(
        db: &ApplicationStore,
        pg: &PostgresDb,
        parent: &store::projects::Project,
    ) -> Self {
        let target = project(db, parent).await;
        let other = project(db, parent).await;
        let kept_email = format!("kept-{}@example.test", Uuid::new_v4());
        let dropped_email = format!("dropped-{}@example.test", Uuid::new_v4());
        let kept = store::persons::create(
            db,
            &store::persons::NewPerson::with_role(
                "Synthetic kept lawyer",
                kept_email.clone(),
                store::persons::Role::Lawyer,
            ),
        )
        .await
        .unwrap()
        .id;
        let dropped = store::persons::create(
            db,
            &store::persons::NewPerson::new("Synthetic dropped client", dropped_email.clone()),
        )
        .await
        .unwrap()
        .id;
        link_identity(db, &target, &other, kept, dropped).await;
        let holding = store::cap_table_holdings::insert(
            db,
            target.id,
            &store::cap_table_holdings::CapTableHoldingFields {
                holder: Party::person(dropped),
                share_class: "synthetic preferred".into(),
                shares: 37,
                vesting_note: Some("Synthetic vesting retained".into()),
            },
        )
        .await
        .unwrap();
        let fiduciary = store::fiduciary_roles::insert(
            db,
            target.id,
            &store::fiduciary_roles::FiduciaryRoleFields {
                role: "trustee".into(),
                fiduciary: Party::person(dropped),
                successor_order: 1,
                notation_id: None,
            },
        )
        .await
        .unwrap();
        // A real different-kind parent with the same UUID witnesses typed-link isolation.
        sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) SELECT $1,'Synthetic same-UUID Entity',entity_type_id,jurisdiction_id FROM entity WHERE id=$2")
            .bind(dropped).bind(target.entity_id).execute(pg.pool()).await.unwrap();
        let untouched = store::cap_table_holdings::insert(
            db,
            other.id,
            &store::cap_table_holdings::CapTableHoldingFields {
                holder: Party::entity(dropped),
                share_class: "synthetic common".into(),
                shares: 19,
                vesting_note: None,
            },
        )
        .await
        .unwrap();
        let audit = store::relationship_logs::record(
            db,
            &store::relationship_logs::NewRelationshipLog {
                actor_person_id: Some(dropped),
                subject_type: "person".into(),
                subject_id: dropped,
                action: "synthetic_identity_reviewed".into(),
                detail: "Synthetic review provenance".into(),
                project_id: Some(target.id),
            },
        )
        .await
        .unwrap();
        typed_edges(db, &target, kept, dropped, audit.id).await;
        let relationships = relationship_rows(pg).await;
        Self {
            kept,
            dropped,
            target,
            holding,
            untouched,
            fiduciary,
            audit,
            relationships,
            emails: vec![kept_email, dropped_email],
        }
    }

    pub async fn assert_merged(&self, db: &ApplicationStore, pg: &PostgresDb) {
        let expected = store::cap_table_holdings::CapTableHolding {
            holder: Party::person(self.kept),
            ..self.holding.clone()
        };
        assert_eq!(
            store::cap_table_holdings::find(db, self.target.id, self.holding.id)
                .await
                .unwrap(),
            Some(expected)
        );
        let expected = store::fiduciary_roles::FiduciaryRole {
            fiduciary: Party::person(self.kept),
            ..self.fiduciary.clone()
        };
        assert_eq!(
            store::fiduciary_roles::find(db, self.target.id, self.fiduciary.id)
                .await
                .unwrap(),
            Some(expected)
        );
        assert_eq!(
            store::cap_table_holdings::find(db, self.untouched.project_id, self.untouched.id)
                .await
                .unwrap(),
            Some(self.untouched.clone())
        );
        for email in &self.emails {
            assert_eq!(
                store::persons::find_by_any_email(db, email)
                    .await
                    .unwrap()
                    .map(|p| p.id),
                Some(self.kept)
            );
        }
        assert!(store::persons::find_by_id(db, self.dropped)
            .await
            .unwrap()
            .is_none());
        let participation =
            store::projects::participation_for_person(db, self.kept, self.target.id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(participation.participation, "lawyer");
        assert!(participation.is_client_dri);
        let mut expected = self.relationships.clone();
        for row in &mut expected {
            for field in ["in", "out"] {
                if row[field]["tb"] == "person"
                    && row[field]["id"]["Uuid"] == self.dropped.to_string()
                {
                    row[field]["id"]["Uuid"] = json!(self.kept.to_string());
                }
            }
        }
        assert_eq!(
            relationship_rows(pg).await,
            expected,
            "relationship row identities and provenance must survive"
        );
        let observed = store::relationship_logs::for_subject(db, "person", self.kept)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == self.audit.id)
            .unwrap();
        let mut expected = serde_json::to_value(&self.audit).unwrap();
        expected["actor_person_id"] = json!(self.kept);
        expected["subject_id"] = json!(self.kept);
        assert_eq!(serde_json::to_value(observed).unwrap(), expected);
        self.assert_graph(pg).await;
    }

    async fn assert_graph(&self, pg: &PostgresDb) {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM navigator_graph.\"Person\" WHERE properties::text::jsonb->>'id'=$1",
        )
        .bind(self.dropped.to_string())
        .fetch_one(pg.pool())
        .await
        .unwrap();
        assert_eq!(count, 0, "dropped Person vertex must not survive");
        let projected: Vec<(String, i64, String)> = sqlx::query_as("SELECT properties::text::jsonb->>'source_kind',(properties::text::jsonb->>'confidence_pct')::bigint,properties::text::jsonb->>'detail' FROM navigator_graph.\"RELATIONSHIP\" WHERE properties::text::jsonb->>'source_id'=$1 ORDER BY properties::text::jsonb->>'detail'")
            .bind(self.audit.id.to_string()).fetch_all(pg.pool()).await.unwrap();
        assert_eq!(
            projected,
            [
                ("manual".into(), 100, "existing kept provenance".into()),
                ("manual".into(), 100, "incoming retained provenance".into()),
                ("manual".into(), 100, "outgoing retained provenance".into()),
            ]
        );
        let attached: Vec<String> =
            sqlx::query_scalar("SELECT t.v::text FROM graph_project_attach($1) AS t(v)")
                .bind(json!({"seed": self.target.id.to_string()}).to_string())
                .fetch_all(pg.pool())
                .await
                .unwrap();
        assert!(attached.iter().any(|edge| edge.contains("WORKS_ON")
            && edge.contains(&self.kept.to_string())
            && edge.contains("lawyer")));
        assert!(attached.iter().any(|edge| edge.contains("HANDLES")
            && edge.contains(&self.target.firm_id.unwrap().to_string())));
    }
}

pub async fn snapshot(pg: &PostgresDb) -> Vec<Value> {
    let mut rows = Vec::new();
    for table in [
        "public.person",
        "public.project",
        "public.firm",
        "public.entity",
        "public.person_mailbox",
        "public.person_project_role",
        "public.person_firm_role",
        "public.entity_role",
        "public.cap_table_holding",
        "public.fiduciary_role",
        "public.relationship",
        "public.relationship_log",
        "navigator_graph.\"Person\"",
        "navigator_graph.\"Entity\"",
        "navigator_graph.\"Project\"",
        "navigator_graph.\"Firm\"",
        "navigator_graph.\"HAS_ROLE\"",
        "navigator_graph.\"RELATIONSHIP\"",
        "navigator_graph.\"WORKS_ON\"",
        "navigator_graph.\"MEMBER_OF\"",
        "navigator_graph.\"HANDLES\"",
    ] {
        rows.push(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM {table} t")))
            .fetch_one(pg.pool()).await.unwrap());
    }
    rows
}
