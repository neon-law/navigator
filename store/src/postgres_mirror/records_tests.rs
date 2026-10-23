use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema, mirror_identity};
use crate::test_support::mem_surreal;
use serde_json::json;

#[test]
fn record_keys_name_their_registered_table_and_uuid() {
    let id = Uuid::now_v7();
    for table in TABLES {
        let key = key(table.name, id).unwrap();
        assert_eq!(key.table(), table.name);
        assert_eq!(key.to_string(), format!("{}:{id}", table.name));
    }
    assert!(matches!(key("no_such_table", id), Err(MirrorError::Source)));
}

#[test]
fn every_group_is_a_registered_mirror_after_its_parents() {
    let names: Vec<&str> = super::super::MIRRORS.iter().map(|m| m.name).collect();
    let position = |name: &str| names.iter().position(|n| *n == name).unwrap();
    for table in TABLES {
        let group = position(table.group);
        for parent in [
            "identity",
            "tenancy",
            "workflow",
            "communications_conversations",
        ] {
            assert!(position(parent) < group, "{} after {parent}", table.group);
        }
    }
}

#[test]
fn registered_columns_are_exactly_the_postgres_columns() {
    let schema =
        crate::postgres_schema::parse_schema(crate::postgres_schema::canonical_schema()).unwrap();
    for table in TABLES {
        let columns: std::collections::BTreeSet<&str> = schema
            .get(table.name)
            .unwrap_or_else(|| panic!("{} in the canonical schema", table.name))
            .columns
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            columns,
            table.columns.iter().copied().collect(),
            "{}",
            table.name
        );
        assert_eq!(table.columns[0], "id");
    }
}

#[test]
fn surreal_and_postgres_rows_normalize_to_one_image() {
    let id = Uuid::now_v7();
    let project = Uuid::now_v7();
    let table = table("testimonial").unwrap();
    let (key, surreal) = normalize(
        table,
        json!({
            "id": format!("testimonial:⟨{id}⟩"),
            "project_id": format!("project:{project}"),
            "person_id": {"tb": "person", "id": {"Uuid": project.to_string()}},
            "quote": "Synthetic",
            "consented_at": "2026-10-21T01:02:03.123456789+00:00",
            "display_order": 0,
            "inserted_at": "2026-10-21T01:02:03Z",
            "updated_at": "2026-10-21T01:02:03Z",
            "unmapped": "dropped",
        }),
    )
    .unwrap();
    let (_, postgres) = normalize(
        table,
        json!({
            "id": id.to_string(),
            "project_id": project.to_string(),
            "person_id": project.to_string(),
            "quote": "Synthetic",
            "attribution_label": null,
            "consented_at": "2026-10-21T01:02:03.123456+00:00",
            "published_at": null,
            "display_order": 0,
            "inserted_at": "2026-10-21T01:02:03+00:00",
            "updated_at": "2026-10-21T01:02:03+00:00",
        }),
    )
    .unwrap();
    assert_eq!(
        key,
        MirrorKey::Record {
            table: "testimonial",
            id
        }
    );
    assert_eq!(surreal, postgres);
    assert!(matches!(
        normalize(
            table,
            json!({"id": id.to_string(), "inserted_at": "not a timestamp"})
        ),
        Err(MirrorError::SourceTimestamp)
    ));
}

#[test]
fn source_domain_records_keep_polymorphic_relationship_tags() {
    let id = format!("relationship:{}", Uuid::now_v7());
    let person = Uuid::now_v7();
    let entity = Uuid::now_v7();
    let (key, normalized) = normalize(
        table("relationship").unwrap(),
        json!({
            "id": id,
            "in": {"tb": "person", "id": {"Uuid": person.to_string()}},
            "out": {"tb": "entity", "id": {"Uuid": entity.to_string()}},
            "confidence_pct": 73,
            "detail": "Synthetic relationship",
            "kind": "family",
            "source_id": null,
            "source_kind": "manual",
            "inserted_at": "2026-10-21T01:02:03Z",
            "updated_at": "2026-10-21T01:02:03Z"
        }),
    )
    .unwrap();

    assert_eq!(
        key,
        MirrorKey::RecordString {
            table: "relationship",
            id
        }
    );
    assert_eq!(normalized["in"]["tb"], "person");
    assert_eq!(normalized["out"]["tb"], "entity");
    assert_eq!(normalized["in"]["id"]["Uuid"], person.to_string());
    assert_eq!(normalized["out"]["id"]["Uuid"], entity.to_string());
}

#[test]
fn source_domain_records_keep_domain_string_record_keys() {
    for table_name in [
        "firm_anchor",
        "finance_aggregate",
        "iolta_account",
        "iolta_allocation",
        "iolta_withdrawal",
        "xero_invoice",
    ] {
        let source_key = format!("external-key-{table_name}-with:separator");
        let mut fixture = json!({"id": source_key});
        if table_name == "firm_anchor" {
            fixture["id_kind"] = json!("string");
        }
        let (key, row) = normalize(table(table_name).unwrap(), fixture).unwrap();
        assert_eq!(
            key,
            MirrorKey::RecordString {
                table: table_name,
                id: source_key.clone()
            }
        );
        assert_eq!(row["id"], source_key);
    }
}

#[test]
fn firm_anchor_sentinel_exemption_matches_only_the_reserved_typed_pair() {
    let sentinel = json!({
        "tb": "entity",
        "id": {"String": "__navigator_claim_sentinel__"}
    });
    assert!(is_firm_anchor_sentinel(
        &MirrorKey::Record {
            table: "firm_anchor",
            id: Uuid::nil(),
        },
        &sentinel
    ));
    for (key, value) in [
        (
            MirrorKey::Record {
                table: "firm_anchor",
                id: Uuid::now_v7(),
            },
            sentinel.clone(),
        ),
        (
            MirrorKey::Record {
                table: "firm_anchor",
                id: Uuid::nil(),
            },
            json!({"tb": "entity", "id": {"String": "near-match"}}),
        ),
    ] {
        assert!(!is_firm_anchor_sentinel(&key, &value));
    }
}

#[tokio::test]
async fn natural_string_record_id_survives_a_surreal_round_trip() {
    let surreal = mem_surreal().await;
    let source_key = "firm:anchor-42";
    surreal
        .query("CREATE $id SET entity_id = $entity")
        .bind((
            "id",
            surrealdb::types::RecordId::new("firm_anchor", source_key.to_owned()),
        ))
        .bind((
            "entity",
            crate::surreal::record_id("entity", Uuid::now_v7()),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    let table = table("firm_anchor").unwrap();
    let row = source_one_string(&surreal, table, source_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row["id"], source_key);
}

#[test]
fn upsert_writes_only_a_changed_row() {
    let sql = upsert_sql(table("project_module").unwrap());
    assert!(sql.contains("jsonb_populate_record(NULL::\"project_module\""));
    assert!(sql.contains("ON CONFLICT (\"id\") DO UPDATE"));
    assert!(sql.contains("IS DISTINCT FROM ROW(EXCLUDED.\"project_id\""));
}

async fn count(pg: &PostgresDb, table: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
        .fetch_one(&pg.0)
        .await
        .unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One lifecycle across the group's four tables.
async fn project_records_mirror_replay_reconcile_and_remove() {
    use crate::project_modules::{self, Module};
    use crate::statutory_deadlines::{self, NewStatutoryDeadline};

    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let project = crate::test_support::seed_project(&surreal, "Synthetic records matter").await;
    let person = crate::test_support::dri_person(&surreal).await;
    mirror_identity(&pg, &[project.entity_id], &[]).await;

    let module = project_modules::enable(&surreal, project.id, Module::Deadlines, Some(person))
        .await
        .unwrap();
    let deadline = statutory_deadlines::record(
        &surreal,
        &NewStatutoryDeadline {
            project_id: project.id,
            kind: "synthetic_response",
            trigger_on: "2026-10-21".parse().unwrap(),
            due_on: "2026-11-20".parse().unwrap(),
            statute: "Synthetic Rule 1",
            source: "synthetic test",
        },
    )
    .await
    .unwrap();
    crate::testimonials::find_or_create(
        &surreal,
        &crate::testimonials::NewTestimonial {
            project_id: project.id,
            person_id: person,
            quote: "Synthetic praise",
            attribution_label: Some("Synthetic client".into()),
            consented_at: Some("2026-10-21T01:02:03.123456789+00:00".into()),
            published_at: None,
            display_order: 2,
        },
    )
    .await
    .unwrap();
    let positions = [crate::playbooks::Position {
        topic: "Synthetic topic".into(),
        preferred: "Preferred".into(),
        fallback: "Fallback".into(),
        walkaway: "Walk-away".into(),
        severity: "high".into(),
    }];
    let playbook = crate::playbooks::create(
        &surreal,
        &crate::playbooks::NewPlaybook {
            entity_id: project.entity_id,
            name: "Synthetic MSA",
            positions: &positions,
        },
    )
    .await
    .unwrap();

    // Nothing is mirrored yet: parity names every source row by key only.
    let before = parity(&surreal, &pg, "project_records").await.unwrap();
    assert_eq!(
        before.tables.iter().map(|t| t.table).collect::<Vec<_>>(),
        [
            "project_module",
            "statutory_deadline",
            "testimonial",
            "playbook"
        ]
    );
    assert_eq!(before.divergent().count(), 4);
    assert!(reconcile(&surreal, &pg, "project_records")
        .await
        .unwrap()
        .is_clean());
    for table in [
        "project_module",
        "statutory_deadline",
        "testimonial",
        "playbook",
    ] {
        assert_eq!(count(&pg, table).await, 1, "{table}");
    }
    let due: chrono::NaiveDate =
        sqlx::query_scalar("SELECT due_on FROM statutory_deadline WHERE id = $1")
            .bind(deadline.id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(due.to_string(), "2026-11-20");

    // A replay changes nothing.
    assert_eq!(
        mirror(&surreal, &pg, "project_module", module.id)
            .await
            .unwrap(),
        Mirrored::Unchanged
    );

    // Target drift is named and repaired from SurrealDB.
    sqlx::query("UPDATE testimonial SET quote = 'drifted'")
        .execute(&pg.0)
        .await
        .unwrap();
    let drifted = parity(&surreal, &pg, "project_records").await.unwrap();
    assert!(matches!(
        drifted.divergent().collect::<Vec<_>>()[..],
        [MirrorKey::Record {
            table: "testimonial",
            ..
        }]
    ));
    assert!(reconcile(&surreal, &pg, "project_records")
        .await
        .unwrap()
        .is_clean());

    // A source edit reaches PostgreSQL through the row's key.
    crate::playbooks::update_positions(&surreal, playbook, &[])
        .await
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, "playbook", playbook).await.unwrap(),
        Mirrored::Written
    );
    let stored: Value = sqlx::query_scalar("SELECT positions FROM playbook WHERE id = $1")
        .bind(playbook)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(stored, json!([]));

    // A re-enabled module reuses the unique (project, module) pair under a new
    // id: reconcile removes the stale row before it writes the new one.
    project_modules::disable(&surreal, project.id, Module::Deadlines, Some(person))
        .await
        .unwrap();
    let again = project_modules::enable(&surreal, project.id, Module::Deadlines, Some(person))
        .await
        .unwrap();
    assert_ne!(again.id, module.id);
    assert!(reconcile(&surreal, &pg, "project_records")
        .await
        .unwrap()
        .is_clean());
    let held: Uuid = sqlx::query_scalar("SELECT id FROM project_module")
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(held, again.id);

    // A row SurrealDB no longer has is removed.
    project_modules::disable(&surreal, project.id, Module::Deadlines, Some(person))
        .await
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, "project_module", again.id)
            .await
            .unwrap(),
        Mirrored::Deleted
    );
    assert_eq!(count(&pg, "project_module").await, 0);
    assert!(parity(&surreal, &pg, "project_records")
        .await
        .unwrap()
        .is_clean());
    pg.0.close().await;
}

#[tokio::test]
async fn relationship_typed_endpoints_and_provenance_round_trip_from_surreal_on_postgres() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let person = crate::persons::create(
        &surreal,
        &crate::persons::NewPerson::with_role(
            "Synthetic relationship person",
            format!("relationship-{}@example.test", Uuid::now_v7()),
            crate::persons::Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    let entity_id = crate::test_support::seed_entity(&surreal).await;
    let log = crate::relationship_logs::record(
        &surreal,
        &crate::relationship_logs::NewRelationshipLog {
            actor_person_id: Some(person.id),
            subject_type: "entity".into(),
            subject_id: entity_id,
            action: "synthetic relationship provenance".into(),
            detail: "synthetic fixture only".into(),
            project_id: None,
        },
    )
    .await
    .unwrap();
    crate::relationships::record(
        &surreal,
        &crate::relationships::NewRelationship {
            from: crate::relationships::Endpoint::Person,
            from_id: person.id,
            to: crate::relationships::Endpoint::Entity,
            to_id: entity_id,
            kind: crate::relationships::KIND_ADVERSE_TO.into(),
            confidence_pct: 91,
            source_kind: crate::relationships::SOURCE_LLM.into(),
            source_id: Some(log.id),
            detail: Some("synthetic typed-link fixture".into()),
        },
    )
    .await
    .unwrap();

    let relation_table = table("relationship").unwrap();
    let mut source_rows = source(&surreal, relation_table).await.unwrap();
    assert_eq!(source_rows.len(), 1);
    let (key, expected) = source_rows.pop_first().unwrap();
    assert_eq!(expected["in"]["tb"], "person");
    assert_eq!(expected["in"]["id"]["Uuid"], person.id.to_string());
    assert_eq!(expected["out"]["tb"], "entity");
    assert_eq!(expected["out"]["id"]["Uuid"], entity_id.to_string());
    assert_eq!(expected["source_id"]["tb"], "relationship_log");
    assert_eq!(expected["source_id"]["id"]["Uuid"], log.id.to_string());

    let MirrorKey::RecordString { id, .. } = &key else {
        panic!("relationship record has a string identity");
    };
    assert_eq!(
        mirror_string(&surreal, &pg, relation_table.name, id)
            .await
            .unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        target(&pg, relation_table).await.unwrap().get(&key),
        Some(&expected)
    );
    pg.0.close().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end assertion harness exercises all 34 source destinations.
async fn source_domain_records_round_trip_every_field_on_postgres() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let schema =
        crate::postgres_schema::parse_schema(crate::postgres_schema::canonical_schema()).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../postgres/parity-manifest.json")).unwrap();

    for table in tables("source_domain_records") {
        if table.name == "firm_anchor" {
            // This table is exercised end-to-end below with a source-created
            // natural key rather than a fabricated UUID-shaped fixture.
            continue;
        }
        let id = Uuid::now_v7();
        let string_id = format!("synthetic-{}-external", table.name);
        let mut source_row = serde_json::Map::new();
        for column in table.columns {
            let source_type = manifest["source"]["tables"][table.name][*column]
                .as_str()
                .unwrap_or_else(|| match (table.name, *column) {
                    ("relationship", "in" | "out") => "record<person|entity>",
                    ("firm_anchor", "id_kind") => "string",
                    _ => panic!("{}.{column} has a source type", table.name),
                });
            let optional = source_type.starts_with("option<");
            let source_type = source_type
                .strip_prefix("option<")
                .and_then(|value| value.strip_suffix('>'))
                .unwrap_or(source_type);
            let value = if optional && column.ends_with("_at") {
                serde_json::Value::Null
            } else if *column == "id" {
                if STRING_KEY_TABLES.contains(&table.name) {
                    json!(string_id)
                } else {
                    json!(format!("{}:⟨{id}⟩", table.name))
                }
            } else if let Some((_, parent)) =
                table.references.iter().find(|(name, _)| name == column)
            {
                let parent_id = Uuid::now_v7();
                json!({"tb": parent, "id": {"Uuid": parent_id.to_string()}})
            } else if let Some((_, _, parents)) = TAGGED_REFERENCES
                .iter()
                .find(|(name, field, _)| *name == table.name && field == column)
            {
                let parent = parents[0];
                let parent_id = Uuid::now_v7();
                json!({"tb": parent, "id": {"Uuid": parent_id.to_string()}})
            } else if table.timestamps.contains(column) {
                json!("2026-10-21T01:02:03.123456789+00:00")
            } else {
                match schema[table.name].columns[*column].as_str() {
                    "UUID" => json!(Uuid::now_v7().to_string()),
                    "TIMESTAMPTZ" => json!("2026-10-21T01:02:03.123456789+00:00"),
                    "BIGINT" => json!(17),
                    "DOUBLE PRECISION" => json!(17.25),
                    "BOOLEAN" => json!(true),
                    "JSONB" => match source_type {
                        "array<string>" => json!(["synthetic", "round-trip"]),
                        _ => json!({"synthetic": table.name, "field": column}),
                    },
                    _ => json!(format!("Synthetic {}.{column}", table.name)),
                }
            };
            source_row.insert((*column).to_string(), value);
        }

        let (_, expected) = normalize(table, serde_json::Value::Object(source_row)).unwrap();
        assert_eq!(
            expected.as_object().unwrap().len(),
            table.columns.len(),
            "{}",
            table.name
        );
        sqlx::query(sqlx::AssertSqlSafe(upsert_sql(table)))
            .bind(expected.clone())
            .execute(&pg.0)
            .await
            .unwrap_or_else(|error| panic!("{} synthetic full-row insert: {error}", table.name));
        let actual = target(&pg, table).await.unwrap();
        let key = if STRING_KEY_TABLES.contains(&table.name) {
            MirrorKey::RecordString {
                table: table.name,
                id: string_id,
            }
        } else {
            MirrorKey::Record {
                table: table.name,
                id,
            }
        };
        assert_eq!(actual.get(&key), Some(&expected), "{}", table.name);
    }

    let surreal = mem_surreal().await;
    let anchor_id = Uuid::nil();
    let entity = crate::entities::create(
        &surreal,
        &crate::entities::NewEntity {
            name: "Synthetic anchor with nil-key spelling".into(),
            entity_type_id: crate::test_support::SEED_ENTITY_TYPE_ID,
            jurisdiction_id: crate::test_support::SEED_ENTITY_JURISDICTION_ID,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: Some(anchor_id.to_string()),
        },
    )
    .await
    .unwrap();
    let natural_id = "firm-anchor:\"quoted\"\\slash-🧭";
    surreal
        .query("CREATE $id SET entity_id = $entity")
        .bind((
            "id",
            surrealdb::types::RecordId::new("firm_anchor", natural_id.to_owned()),
        ))
        .bind(("entity", crate::surreal::record_id("entity", entity.id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let natural_key = MirrorKey::RecordString {
        table: "firm_anchor",
        id: natural_id.to_owned(),
    };
    let missing = crate::postgres_mirror::missing(
        &pg,
        &crate::postgres_mirror::TableParity {
            table: "firm_anchor",
            source_rows: 1,
            target_rows: 0,
            divergent: vec![natural_key.clone()],
        },
    )
    .await;
    assert_eq!(missing.unwrap(), vec![natural_key.clone()]);
    mirror_string(&surreal, &pg, "firm_anchor", natural_id)
        .await
        .unwrap();
    mirror(&surreal, &pg, "firm_anchor", anchor_id)
        .await
        .unwrap();
    let firm_table = table("firm_anchor").unwrap();
    let source_row = source(&surreal, firm_table)
        .await
        .unwrap()
        .remove(&natural_key)
        .unwrap();
    assert_eq!(
        target(&pg, firm_table).await.unwrap().get(&natural_key),
        Some(&source_row)
    );
    let sentinel_key = MirrorKey::Record {
        table: "firm_anchor",
        id: Uuid::nil(),
    };
    let sentinel = target(&pg, firm_table)
        .await
        .unwrap()
        .remove(&sentinel_key)
        .expect("reserved nil UUID sentinel imported separately from the string key");
    assert_eq!(sentinel["id_kind"], "uuid");
    assert!(is_firm_anchor_sentinel(
        &sentinel_key,
        &sentinel["entity_id"]
    ));
    surreal
        .query("DELETE $id")
        .bind((
            "id",
            surrealdb::types::RecordId::new("firm_anchor", natural_id.to_owned()),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        mirror_string(&surreal, &pg, "firm_anchor", natural_id)
            .await
            .unwrap(),
        Mirrored::Deleted
    );

    let id = Uuid::now_v7();
    let at = "2026-10-21T01:02:03.123456789+00:00";
    surreal
        .query(
            "CREATE $id SET bucket_date = type::datetime($at), country_code = 'ZZ', \
             route_pattern = '/synthetic', source = 'fixture', locale = 'en', \
             status_class = '2xx', visits = 17, inserted_at = type::datetime($at), \
             updated_at = type::datetime($at)",
        )
        .bind(("id", crate::surreal::record_id("visitor_route_count", id)))
        .bind(("at", at))
        .await
        .unwrap()
        .check()
        .unwrap();
    let table = table("visitor_route_count").unwrap();
    mirror(&surreal, &pg, table.name, id).await.unwrap();
    let source_row = source_one(&surreal, table, id).await.unwrap().unwrap();
    let actual = target(&pg, table).await.unwrap();
    assert_eq!(
        actual.get(&MirrorKey::Record {
            table: table.name,
            id
        }),
        Some(&source_row)
    );

    let orphan_id = Uuid::now_v7();
    let authority_id = Uuid::now_v7();
    let project_id = Uuid::now_v7();
    surreal
        .query(
            "CREATE $id SET authority_id = $authority, project_id = $project, \
             position = 'Synthetic § 1', disposition = 'relied_on', role = NONE, \
             inserted_at = type::datetime($at), updated_at = type::datetime($at)",
        )
        .bind(("id", crate::surreal::record_id("authority_use", orphan_id)))
        .bind((
            "authority",
            crate::surreal::record_id("authority", authority_id),
        ))
        .bind(("project", crate::surreal::record_id("project", project_id)))
        .bind(("at", at))
        .await
        .unwrap()
        .check()
        .unwrap();
    mirror(&surreal, &pg, "authority_use", orphan_id)
        .await
        .unwrap();
    let references = unresolved_references(&surreal, &pg).await.unwrap();
    assert_eq!(
        references
            .iter()
            .find(
                |reference| reference.table == "authority_use" && reference.field == "authority_id"
            )
            .map(|reference| (reference.source_orphans, reference.postgres_missing)),
        Some((1, 1))
    );
    assert_eq!(
        references
            .iter()
            .find(|reference| reference.table == "authority_use" && reference.field == "project_id")
            .map(|reference| (reference.source_orphans, reference.postgres_missing)),
        Some((1, 1))
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One lifecycle across both tables and the Notation.
async fn notation_inputs_mirror_and_follow_their_notation() {
    use crate::notation_sources::{attach, kind, NewNotationSource};

    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let notation_id = crate::test_support::seed_notation(&surreal).await;
    let notation = crate::notations::find_by_id(&surreal, notation_id)
        .await
        .unwrap()
        .unwrap();
    let project = crate::projects::find_by_id(&surreal, notation.project_id)
        .await
        .unwrap()
        .unwrap();
    mirror_identity(&pg, &[project.entity_id], &[]).await;
    for (position, reference) in ["synthetic__first", "synthetic__second"]
        .into_iter()
        .enumerate()
    {
        attach(
            &surreal,
            &NewNotationSource {
                notation_id,
                kind: kind::QUESTIONNAIRE.into(),
                communication_id: None,
                source_ref: Some(reference.into()),
                position: i32::try_from(position).unwrap(),
            },
        )
        .await
        .unwrap();
    }
    let draft = crate::notation_drafts::create(
        &surreal,
        &crate::notation_drafts::NewNotationDraft {
            project_id: project.id,
            slug: "synthetic-preview",
            title: "Synthetic preview",
            source: "Synthetic template body",
        },
    )
    .await
    .unwrap();

    assert_eq!(
        parity(&surreal, &pg, "notation_inputs")
            .await
            .unwrap()
            .divergent()
            .count(),
        3
    );
    assert!(reconcile(&surreal, &pg, "notation_inputs")
        .await
        .unwrap()
        .is_clean());
    assert_eq!(count(&pg, "notation_source").await, 2);
    assert_eq!(
        mirror(&surreal, &pg, "notation_draft", draft.id)
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    let order: Vec<String> =
        sqlx::query_scalar("SELECT source_ref FROM notation_source ORDER BY position")
            .fetch_all(&pg.0)
            .await
            .unwrap();
    assert_eq!(order, ["synthetic__first", "synthetic__second"]);

    // Changed target rows are repaired from SurrealDB.
    sqlx::query("UPDATE notation_draft SET title = 'drifted'")
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(reconcile(&surreal, &pg, "notation_inputs")
        .await
        .unwrap()
        .is_clean());

    // A deleted preview leaves PostgreSQL with it.
    crate::notation_reset::delete_preview_draft(&surreal, draft.id)
        .await
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, "notation_draft", draft.id)
            .await
            .unwrap(),
        Mirrored::Deleted
    );

    // A Notation removed from SurrealDB takes its sources in PostgreSQL.
    surreal
        .query("DELETE notation_source WHERE notation_id = $notation; DELETE $notation;")
        .bind((
            "notation",
            crate::surreal::record_id("notation", notation_id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    crate::postgres_mirror::mirror(&surreal, &pg, &MirrorKey::Notation(notation_id))
        .await
        .unwrap();
    assert_eq!(count(&pg, "notation_source").await, 0);
    assert!(parity(&surreal, &pg, "notation_inputs")
        .await
        .unwrap()
        .is_clean());
    pg.0.close().await;
}

#[tokio::test]
async fn git_access_mirrors_hashes_only_and_follows_revocation() {
    use crate::git_access_tokens::{self, SCOPE_READ, SCOPE_WRITE};

    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let project = crate::test_support::seed_project(&surreal, "Synthetic git matter").await;
    let person = crate::test_support::dri_person(&surreal).await;
    mirror_identity(&pg, &[project.entity_id], &[]).await;
    let repository =
        crate::git_repositories::create(&surreal, "synthetic-remote-hash", "0".repeat(40).as_str())
            .await
            .unwrap();
    let expires = chrono::Utc::now() + chrono::Duration::hours(1);
    let scoped = git_access_tokens::mint(
        &surreal,
        person,
        Some(project.id),
        SCOPE_WRITE,
        "synthetic-plaintext-canary",
        expires,
    )
    .await
    .unwrap();
    git_access_tokens::mint(
        &surreal,
        person,
        None,
        SCOPE_READ,
        "synthetic-unscoped",
        expires,
    )
    .await
    .unwrap();

    assert!(reconcile(&surreal, &pg, "git_access")
        .await
        .unwrap()
        .is_clean());
    assert_eq!(count(&pg, "git_access_token").await, 2);
    assert_eq!(
        mirror(&surreal, &pg, "git_repository", repository.id)
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    let rows: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(t) FROM git_access_token t")
        .fetch_all(&pg.0)
        .await
        .unwrap();
    for row in &rows {
        assert!(!row.to_string().contains("synthetic-plaintext-canary"));
    }
    let stored_hash: String =
        sqlx::query_scalar("SELECT token_hash FROM git_access_token WHERE id = $1")
            .bind(scoped.id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(
        stored_hash,
        git_access_tokens::hash_token("synthetic-plaintext-canary")
    );

    git_access_tokens::revoke(&surreal, scoped.id)
        .await
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, "git_access_token", scoped.id)
            .await
            .unwrap(),
        Mirrored::Deleted
    );
    assert!(parity(&surreal, &pg, "git_access")
        .await
        .unwrap()
        .is_clean());
    pg.0.close().await;
}

#[test]
fn malformed_rows_fail_closed_on_their_key() {
    let id = Uuid::now_v7();
    let testimonial = table("testimonial").unwrap();
    assert!(matches!(
        normalize(testimonial, json!(["not", "an", "object"])),
        Err(MirrorError::Source)
    ));
    assert!(matches!(
        normalize(testimonial, json!({"quote": "Synthetic"})),
        Err(MirrorError::Source)
    ));
    assert!(matches!(
        normalize(testimonial, json!({"id": "testimonial:not-a-uuid"})),
        Err(MirrorError::Source)
    ));
    assert!(matches!(
        normalize(
            testimonial,
            json!({"id": id.to_string(), "project_id": "project:not-a-uuid"})
        ),
        Err(MirrorError::Source)
    ));
    assert!(matches!(
        normalize(testimonial, json!({"id": id.to_string(), "inserted_at": 7})),
        Err(MirrorError::SourceTimestamp)
    ));

    let account = table("iolta_account").unwrap();
    assert!(matches!(
        normalize(account, json!({"id": ""})),
        Err(MirrorError::Source)
    ));
    assert!(matches!(
        normalize(account, json!({"id": 42})),
        Err(MirrorError::Source)
    ));

    let anchor = table("firm_anchor").unwrap();
    assert!(matches!(
        normalize(anchor, json!({"id": id.to_string()})),
        Err(MirrorError::Source)
    ));
    assert!(matches!(
        normalize(anchor, json!({"id": "x", "id_kind": "number"})),
        Err(MirrorError::Source)
    ));
    assert!(matches!(
        normalize(anchor, json!({"id": "not-a-uuid", "id_kind": "uuid"})),
        Err(MirrorError::Source)
    ));
    let (key, row) = normalize(
        anchor,
        json!({"id": {"Uuid": id.to_string()}, "id_kind": "uuid"}),
    )
    .unwrap();
    assert_eq!(
        key,
        MirrorKey::Record {
            table: "firm_anchor",
            id
        }
    );
    assert_eq!(row["id"], id.to_string());
    assert_eq!(row["id_kind"], "uuid");
}

#[test]
fn record_links_decode_every_surreal_rendering_of_a_uuid() {
    let id = Uuid::now_v7();
    for rendered in [
        json!(id.to_string()),
        json!(format!("person:{id}")),
        json!(format!("person:⟨{id}⟩")),
        json!(format!("person:u'{id}'")),
        json!({"tb": "person", "id": {"Uuid": id.to_string()}}),
    ] {
        assert_eq!(uuid_in(&rendered), Some(id), "{rendered}");
    }
    for unreadable in [json!(7), json!(null), json!("person:abc"), json!({"tb": 1})] {
        assert_eq!(uuid_in(&unreadable), None, "{unreadable}");
    }
    assert_eq!(
        string_key_in(&json!({"tb": "relationship", "id": {"String": "k"}})),
        Some("k".to_owned())
    );
    assert_eq!(string_key_in(&json!({"tb": ""})), None);
    assert_eq!(string_key_in(&json!("")), None);
    assert_eq!(string_key_in(&json!(3)), None);
}

#[test]
fn link_parents_resolve_to_their_owning_mirror_key() {
    let id = Uuid::now_v7();
    assert_eq!(parent("project", id).unwrap(), MirrorKey::Project(id));
    assert_eq!(parent("person", id).unwrap(), MirrorKey::Person(id));
    assert_eq!(parent("entity", id).unwrap(), MirrorKey::Entity(id));
    assert_eq!(parent("notation", id).unwrap(), MirrorKey::Notation(id));
    assert_eq!(
        parent("communication", id).unwrap(),
        MirrorKey::Communication(id)
    );
    assert!(matches!(
        parent("jurisdiction", id),
        Err(MirrorError::Source)
    ));
}

#[test]
fn mixed_key_upserts_conflict_on_the_key_kind_and_value() {
    let sql = upsert_sql(table("firm_anchor").unwrap());
    assert!(sql.contains("ON CONFLICT (\"id_kind\", \"id\") DO UPDATE"));
    assert!(sql.contains("\"firm_anchor\".\"entity_id\""));
}

#[test]
fn reference_counts_record_orphans_and_missing_targets_separately() {
    let held = Uuid::now_v7();
    let source_only = Uuid::now_v7();
    let source = BTreeMap::from([(
        "person",
        std::collections::BTreeSet::from([held, source_only]),
    )]);
    let target = BTreeMap::from([("person", std::collections::BTreeSet::from([held]))]);
    let mut counts = BTreeMap::new();
    let key = ("relationship", "in");
    note_reference(&mut counts, key, Some(held), "person", &source, &target);
    assert!(counts.is_empty());
    note_reference(
        &mut counts,
        key,
        Some(source_only),
        "person",
        &source,
        &target,
    );
    assert_eq!(counts[&key], (0, 1));
    note_reference(&mut counts, key, None, "person", &source, &target);
    assert_eq!(counts[&key], (1, 2));
    note_reference(&mut counts, key, Some(held), "entity", &source, &target);
    assert_eq!(counts[&key], (2, 3));
}

async fn surreal_value(
    surreal: &crate::surreal::SurrealDb,
    query: &str,
) -> surrealdb::types::Value {
    let value: Option<surrealdb::types::Value> = surreal
        .query(query)
        .await
        .unwrap()
        .check()
        .unwrap()
        .take(0)
        .unwrap();
    value.unwrap()
}

#[tokio::test]
async fn typed_record_links_keep_their_table_and_key_kind() {
    let surreal = mem_surreal().await;
    let id = Uuid::now_v7();
    let value = surreal_value(
        &surreal,
        &format!(
            "RETURN {{ uuid: person:u'{id}', text: entity:⟨synthetic-key⟩, number: entity:7, \
             list: [entity:8], plain: 'Synthetic' }}"
        ),
    )
    .await;
    let normalized = normalize_typed_record_links(&value);
    assert_eq!(
        normalized["uuid"],
        json!({"tb": "person", "id": {"Uuid": id.to_string()}})
    );
    assert_eq!(
        normalized["text"],
        json!({"tb": "entity", "id": {"String": "synthetic-key"}})
    );
    assert_eq!(
        normalized["number"],
        json!({"tb": "entity", "id": {"Number": 7}})
    );
    assert_eq!(
        normalized["list"],
        json!([{"tb": "entity", "id": {"Number": 8}}])
    );
    assert_eq!(normalized["plain"], "Synthetic");

    let anchor = table("firm_anchor").unwrap();
    let string_key = surreal_value(&surreal, "RETURN firm_anchor:⟨synthetic⟩").await;
    assert_eq!(
        string_record_key(anchor, &string_key),
        Some("synthetic".to_owned())
    );
    let other_table = surreal_value(&surreal, "RETURN entity:⟨synthetic⟩").await;
    assert_eq!(string_record_key(anchor, &other_table), None);
    let number_key = surreal_value(&surreal, "RETURN firm_anchor:7").await;
    assert_eq!(string_record_key(anchor, &number_key), None);
    let not_a_link = surreal_value(&surreal, "RETURN 'firm_anchor:synthetic'").await;
    assert_eq!(string_record_key(anchor, &not_a_link), None);
}

#[tokio::test]
async fn mixed_and_string_keyed_source_rows_fail_closed_on_foreign_key_shapes() {
    let surreal = mem_surreal().await;
    let entity = Uuid::now_v7();
    let anchor_id = Uuid::now_v7();
    surreal
        .query("CREATE $id SET entity_id = $entity")
        .bind(("id", crate::surreal::record_id("firm_anchor", anchor_id)))
        .bind(("entity", crate::surreal::record_id("entity", entity)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let anchor = table("firm_anchor").unwrap();
    let row = source_one(&surreal, anchor, anchor_id)
        .await
        .unwrap()
        .expect("a UUID-keyed firm anchor reads back");
    assert_eq!(row["id"], anchor_id.to_string());
    assert_eq!(row["id_kind"], "uuid");
    assert_eq!(row["entity_id"]["tb"], "entity");
    assert!(source_one(&surreal, anchor, Uuid::now_v7())
        .await
        .unwrap()
        .is_none());
    assert!(source_one_string(&surreal, anchor, "absent-key")
        .await
        .unwrap()
        .is_none());

    surreal
        .query("CREATE firm_anchor:7 SET entity_id = $entity")
        .bind(("entity", crate::surreal::record_id("entity", entity)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(matches!(
        source(&surreal, anchor).await,
        Err(MirrorError::Source)
    ));

    let not_an_object = surreal_value(&surreal, "RETURN 'Synthetic'").await;
    assert!(matches!(
        normalize_surreal(anchor, not_an_object),
        Err(MirrorError::Source)
    ));
    let without_id = surreal_value(&surreal, "RETURN { entity_id: 'Synthetic' }").await;
    assert!(matches!(
        normalize_surreal(anchor, without_id),
        Err(MirrorError::Source)
    ));
    let foreign_table =
        surreal_value(&surreal, &format!("RETURN {{ id: entity:u'{entity}' }}")).await;
    assert!(matches!(
        normalize_surreal(anchor, foreign_table),
        Err(MirrorError::Source)
    ));
    let plain_id = surreal_value(&surreal, "RETURN { id: 'firm_anchor' }").await;
    assert!(matches!(
        normalize_surreal(anchor, plain_id),
        Err(MirrorError::Source)
    ));

    let account = table("iolta_account").unwrap();
    let uuid_keyed = surreal_value(
        &surreal,
        &format!("RETURN {{ id: iolta_account:u'{entity}' }}"),
    )
    .await;
    assert!(matches!(
        normalize_surreal(account, uuid_keyed),
        Err(MirrorError::Source)
    ));
    let surreal_key = surreal_value(
        &surreal,
        &format!("RETURN {{ id: iolta_account:u'{entity}', __surreal_key: 'synthetic-account' }}"),
    )
    .await;
    let (key, row) = normalize_surreal(account, surreal_key).unwrap();
    assert_eq!(
        key,
        MirrorKey::RecordString {
            table: "iolta_account",
            id: "synthetic-account".into()
        }
    );
    assert_eq!(row["id"], "synthetic-account");
}

#[tokio::test]
async fn string_mirroring_refuses_uuid_keyed_tables_before_touching_either_store() {
    let surreal = mem_surreal().await;
    // A lazy pool never dials; the refusal must come before any query.
    let pg = PostgresDb(
        sqlx::postgres::PgPoolOptions::new().connect_lazy_with(
            sqlx::postgres::PgConnectOptions::new()
                .host("127.0.0.1")
                .port(9),
        ),
    );
    assert!(matches!(
        mirror_string(&surreal, &pg, "testimonial", "synthetic").await,
        Err(MirrorError::Source)
    ));
    assert!(matches!(
        mirror_string(&surreal, &pg, "no_such_table", "synthetic").await,
        Err(MirrorError::Source)
    ));
}

#[tokio::test]
async fn natural_and_record_references_count_orphans_without_naming_them() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    surreal
        .query(
            "CREATE iolta_allocation:⟨synthetic-allocation⟩ SET \
             withdrawal_id = 'synthetic-missing-withdrawal', \
             xero_invoice_id = 'synthetic-missing-invoice', project_id = $project, \
             invoice_reference = 'Synthetic', amount_cents = 100, occurred_at = time::now()",
        )
        .bind((
            "project",
            crate::surreal::record_id("project", Uuid::now_v7()),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();

    let references = unresolved_references(&surreal, &pg).await.unwrap();
    for field in ["project_id", "withdrawal_id", "xero_invoice_id"] {
        let reference = references
            .iter()
            .find(|reference| reference.table == "iolta_allocation" && reference.field == field)
            .unwrap_or_else(|| panic!("{field} is counted"));
        assert_eq!(
            (reference.source_orphans, reference.postgres_missing),
            (1, 1),
            "{field}"
        );
    }
    assert!(!format!("{references:?}").contains("synthetic-missing"));
    pg.0.close().await;
}

#[tokio::test]
async fn absent_source_rows_delete_nothing_from_an_empty_target() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    assert_eq!(
        mirror(&surreal, &pg, "firm_anchor", Uuid::now_v7())
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    for table_name in ["firm_anchor", "iolta_account"] {
        assert_eq!(
            mirror_string(&surreal, &pg, table_name, "synthetic-absent")
                .await
                .unwrap(),
            Mirrored::Unchanged,
            "{table_name}"
        );
    }
    pg.0.close().await;
}
