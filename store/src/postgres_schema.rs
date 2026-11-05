                "20261105120000_inbox_conversation_person_id.sql",
                include_str!(
                    "../postgres/migrations/20261105120000_inbox_conversation_person_id.sql"
                ),
            ),
            (
                "20261105130000_index_message_provider_message_id.sql",
                include_str!(
                    "../postgres/migrations/20261105130000_index_message_provider_message_id.sql"
                ),
            ),
            (
                "20261105140000_entity_metadata.sql",
                include_str!("../postgres/migrations/20261105140000_entity_metadata.sql"),
            ),
            (
                "20261105150000_create_esignature_webhook_journal.sql",
                include_str!(
                    "../postgres/migrations/20261105150000_create_esignature_webhook_journal.sql"
                ),
            ),
        ])
        .expect("timestamped migration assembles");
        assert_eq!(canonical_schema().trim(), assembled.trim());
    }

    #[test]
    fn inbox_conversation_persists_the_sender_person() {
        let schema = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let conversation = schema
            .get("inbox_conversation")
            .expect("the inbox conversation table");
        let kind = conversation
            .columns
            .get("person_id")
            .expect("the sender-person link column");
        assert!(kind.contains("UUID"), "person_id is a UUID: {kind}");
        assert!(
            conversation
                .foreign_keys
                .contains(&("person_id".to_string(), "person".to_string())),
            "person_id references person"
        );
    }

    #[test]
    fn sent_email_audit_has_identity_scope_and_timestamps() {
        let schema = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let email = schema.get("sent_email").expect("outbound audit table");
        for column in [
            "id",
            "recipient",
            "subject",
            "sender",
            "project_id",
            "person_id",
            "template_slug",
            "body",
            "outcome",
            "sg_message_id",
            "sent_at",
            "inserted_at",
            "updated_at",
        ] {
            assert!(email.columns.contains_key(column), "sent_email.{column}");
        }
        let ledger = schema
            .get("sent_email_export")
            .expect("Sent Email Export ledger");
        for column in [
            "day",
            "row_count",
            "content_sha256",
            "data_file",
            "purged_at",
        ] {
            assert!(
                ledger.columns.contains_key(column),
                "sent_email_export.{column}"
            );
        }
        for column in ["sent_at", "inserted_at", "updated_at"] {
            assert_eq!(email.columns[column], "TIMESTAMPTZ");
        }
        assert!(email.references.contains("project"));
    }

    #[test]
    fn social_account_mirror_keeps_encrypted_bundle_and_ownership_links() {
        let tables = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let account = tables
            .get("social_account")
            .expect("Social Account mirror is present");
        for column in [
            "id",
            "firm_id",
            "brand_key",
            "provider",
            "provider_account_id",
            "display_name",
            "scopes",
            "expires_at",
            "ciphertext",
            "wrapped_dek",
            "kms_key_version",
            "kms_context",
            "status",
            "connected_by_person_id",
            "revoked_by_person_id",
            "revoked_at",
            "inserted_at",
            "updated_at",
        ] {
            assert!(
                account.columns.contains_key(column),
                "social_account.{column}"
            );
        }
        assert!(account.references.contains("firm"));
        assert!(account.references.contains("brand"));
        assert!(account.references.contains("person"));
    }

    #[test]
    fn matter_pages_and_captions_carry_their_project_and_firm() {
        let tables = parse_schema(canonical_schema()).expect("canonical SQL parses");
        for name in ["matter_page", "matter_caption"] {
            let table = tables.get(name).expect("matter target table");
            assert_eq!(
                table.foreign_keys,
                [
                    ("firm_id".into(), "firm".into()),
                    ("project_id".into(), "project".into())
                ]
                .into(),
                "{name}"
            );
        }
        let page = &tables["matter_page"];
        for lens in ["client_sections", "lawyer_sections", "clerk_sections"] {
            assert_eq!(page.columns[lens], "TEXT[]");
        }
        let caption = &tables["matter_caption"];
        assert_eq!(caption.columns["parties"], "JSONB");
        assert_eq!(caption.columns["counsel"], "JSONB");
        let matter_tables = ["matter_page", "matter_caption"];
        let differences: Vec<_> = compare_to_surreal(
            &tables,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        )
        .into_iter()
        .filter(|difference| {
            [&difference.table, &difference.missing_table]
                .into_iter()
                .flatten()
                .any(|table| matter_tables.contains(&table.as_str()))
        })
        .collect();
        assert_eq!(
            differences
                .iter()
                .map(|difference| (
                    difference.table.as_deref(),
                    difference.extra_column.as_deref()
                ))
                .collect::<Vec<_>>(),
            [
                (Some("matter_caption"), Some("firm_id")),
                (Some("matter_page"), Some("firm_id"))
            ],
            "only the denormalized owning Firm differs from the source: {differences:?}"
        );
        for statement in [
            "FOREIGN KEY (project_id, firm_id) REFERENCES project (id, firm_id) ON UPDATE CASCADE",
            "project_id UUID NOT NULL UNIQUE REFERENCES project(id)",
        ] {
            assert!(
                matter_pages_captions_migration().contains(statement),
                "{statement}"
            );
        }
    }

    #[test]
    fn mail_rows_keep_source_parent_links_and_weak_optional_project_attribution() {
        let tables = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let mailroom = tables.get("mailroom").expect("Mailroom target table");
        assert_eq!(
            mailroom.foreign_keys,
            [("address_id".into(), "address".into())].into()
        );
        let letter = tables.get("letter").expect("Letter target table");
        assert_eq!(
            letter.foreign_keys,
            [("mailroom_id".into(), "mailroom".into())].into()
        );
        assert_eq!(letter.columns["project_id"], "UUID");
        assert!(!letter
            .foreign_keys
            .iter()
            .any(|(column, _)| column == "project_id"));
    }

    #[test]
    fn project_service_mirror_preserves_project_document_and_sender_scope() {
        let tables = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let service = tables
            .get("project_service")
            .expect("Project Service is mirrored to PostgreSQL");
        for column in [
            "id",
            "project_id",
            "document_id",
            "recipient",
            "recipient_person_id",
            "recipient_entity_id",
            "counsel_for",
            "method",
            "served_on",
            "address",
            "declarant",
            "declarant_person_id",
            "letter_id",
            "sent_email_id",
            "inserted_at",
            "updated_at",
        ] {
            assert!(
                service.columns.contains_key(column),
                "project_service.{column}"
            );
        }
        assert_eq!(
            service.foreign_keys,
            BTreeSet::from([
                ("project_id".into(), "project".into()),
                ("document_id".into(), "asset".into()),
                ("recipient_person_id".into(), "person".into()),
                ("recipient_entity_id".into(), "entity".into()),
                ("declarant_person_id".into(), "person".into()),
                ("sent_email_id".into(), "sent_email".into()),
            ])
        );
        assert!(crate::schema::table_names().contains(&"project_service".to_string()));
    }

    #[test]
    fn workflow_template_pins_are_foreign_keys() {
        let tables = parse_schema(canonical_schema()).expect("canonical SQL parses");
        for (table, column) in [
            ("notation", "template_id"),
            ("notation_event", "template_version_id"),
            ("notation_package_template", "template_id"),
        ] {
            assert!(
                tables[table]
                    .foreign_keys
                    .contains(&(column.into(), "template".into())),
                "{table}.{column} must pin a retained Template version"
            );
        }
    }

    #[test]
    fn clause_is_a_target_only_postgresql_table() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let clause = postgres
            .get("clause")
            .expect("Clause is a PostgreSQL table");
        for column in [
            "id",
            "code",
            "version",
            "kind",
            "inputs",
            "body",
            "body_sha256",
            "is_current",
            "retired_at",
            "inserted_at",
        ] {
            assert!(clause.columns.contains_key(column), "clause.{column}");
        }
        assert_eq!(clause.columns["inputs"], "JSONB");
        assert!(clause.references.is_empty(), "a shared catalog");
        assert!(!crate::schema::table_names().contains(&"clause".to_string()));
    }

    #[test]
    fn template_mirror_preserves_version_scope_and_current_key() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let template = postgres
            .get("template")
            .expect("Template is mirrored to PostgreSQL");
        for column in [
            "id",
            "code",
            "title",
            "respondent_type",
            "project_id",
            "firm_id",
            "is_current",
            "asset_id",
            "form_code",
            "kind",
            "source_commit_sha",
            "body_sha256",
            "current_key",
            "inserted_at",
            "updated_at",
        ] {
            assert!(template.columns.contains_key(column), "template.{column}");
        }
        assert_eq!(
            template.foreign_keys,
            BTreeSet::from([
                ("project_id".to_string(), "project".to_string()),
                ("firm_id".to_string(), "firm".to_string()),
                ("asset_id".to_string(), "asset".to_string()),
            ])
        );
        assert!(crate::schema::table_names().contains(&"template".to_string()));
    }

    #[test]
    fn post_is_a_postgresql_table_and_the_seed_is_covered() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let post = postgres
            .get("post")
            .expect("the PostgreSQL target table uses the ontology name Post");
        assert!(post.columns.contains_key("body_markdown"));
        assert!(!postgres.contains_key("blog_post"));
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        assert!(
            !parity
                .iter()
                .any(|difference| difference.table.as_deref() == Some("post")),
            "{parity:?}"
        );
        let seed = include_str!("../seeds/neon/Post.yaml");
        let seed_differences = seed_schema_differences(&[("neon/Post.yaml", seed)], &postgres)
            .expect("Post seed parses");
        assert!(seed_differences.is_empty(), "{seed_differences:?}");
    }

    #[test]
    fn a_posts_handles_and_publications_are_postgresql_tables_with_their_seed() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        for table in ["social_handle", "social_publication"] {
            assert!(
                postgres.contains_key(table),
                "{table} is a PostgreSQL table"
            );
        }
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        for table in ["social_handle", "social_publication"] {
            assert!(
                !parity
                    .iter()
                    .any(|difference| difference.table.as_deref() == Some(table)),
                "{table}: {parity:?}"
            );
        }
        assert!(
            postgres["social_publication"]
                .foreign_keys
                .contains(&("post_id".to_string(), "post".to_string())),
            "a publication belongs to a Post"
        );
        let seed = include_str!("../seeds/neon/SocialHandle.yaml");
        let differences = seed_schema_differences(&[("neon/SocialHandle.yaml", seed)], &postgres)
            .expect("SocialHandle seed parses");
        assert!(differences.is_empty(), "{differences:?}");
    }

    #[test]
    fn catalog_tables_match_their_surreal_source_and_the_question_seed() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        for table in [
            "authority",
            "glossary_term",
            "question",
            "notation_package",
            "notation_package_template",
            "notation_rate",
        ] {
            assert!(
                postgres.contains_key(table),
                "{table} is a PostgreSQL table"
            );
            let differences: Vec<_> = parity
                .iter()
                .filter(|difference| difference.table.as_deref() == Some(table))
                .collect();
            assert!(differences.is_empty(), "{table}: {differences:?}");
        }
        let seed = include_str!("../seeds/Question.yaml");
        let differences = seed_schema_differences(&[("Question.yaml", seed)], &postgres)
            .expect("Question seed parses");
        assert!(differences.is_empty(), "{differences:?}");
    }

    #[test]
    fn firm_brand_and_membership_tables_match_their_surreal_source() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        for table in ["firm_brand", "person_firm_role", "person_project_role"] {
            assert!(
                postgres.contains_key(table),
                "{table} is a PostgreSQL table"
            );
            assert!(
                !parity
                    .iter()
                    .any(|difference| difference.table.as_deref() == Some(table)),
                "{table}: {parity:?}"
            );
        }
        for (table, column, target) in [
            ("firm_brand", "firm_id", "firm"),
            ("person_firm_role", "firm_id", "firm"),
            ("person_firm_role", "person_id", "person"),
            ("person_project_role", "project_id", "project"),
            ("person_project_role", "person_id", "person"),
        ] {
            assert!(
                postgres[table]
                    .foreign_keys
                    .contains(&(column.to_string(), target.to_string())),
                "{table}.{column} references {target}"
            );
        }
        assert!(postgres["firm_brand"].references.contains("brand"));
    }

    #[test]
    fn identity_detail_tables_match_their_surreal_source() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        for table in [
            "address",
            "credential",
            "email_token",
            "entity_role",
            "person_delegate",
            "person_external_identity",
            "person_mailbox",
        ] {
            assert!(
                postgres.contains_key(table),
                "{table} is a PostgreSQL table"
            );
            let differences: Vec<_> = parity
                .iter()
                .filter(|difference| difference.table.as_deref() == Some(table))
                .map(|difference| difference.extra_column.as_deref())
                .collect();
            // A Surreal relation keeps its endpoints in `in` and `out`, which
            // the source inventory does not list as fields.
            let expected: &[Option<&str>] = if table == "entity_role" {
                &[Some("entity_id"), Some("person_id")]
            } else {
                &[]
            };
            assert_eq!(differences, expected, "{table}: {parity:?}");
        }
        assert_eq!(postgres["person_mailbox"].columns["id"], "TEXT");
        assert!(postgres["entity_role"]
            .foreign_keys
            .contains(&("person_id".to_string(), "person".to_string())));
        assert!(postgres["entity_role"]
            .foreign_keys
            .contains(&("entity_id".to_string(), "entity".to_string())));
    }

    #[test]
    fn one_line_add_constraint_check_is_validated_without_changing_parity_shape() {
        let ddl = "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
                   ALTER TABLE brand ADD CONSTRAINT brand_font_licence_check CHECK (font_licence IS NULL OR font_licence IN ('OFL-1.1', 'Apache-2.0', 'UFL-1.0'));";
        let tables = parse_schema(ddl).expect("narrow one-line CHECK parses");
        assert_eq!(tables["brand"].columns.len(), 1);
        assert!(parse_schema("ALTER TABLE brand ADD CONSTRAINT x CHECK (a);\n").is_err());
        assert!(parse_schema("ALTER TABLE absent ADD CONSTRAINT x CHECK (a);\n").is_err());
        assert!(parse_schema("ALTER TABLE brand ADD CONSTRAINT x CHECK a;\n").is_err());
    }

    #[test]
    fn a_dropped_constraint_parses_without_changing_parity_shape() {
        let ddl = "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
                   ALTER TABLE brand DROP CONSTRAINT brand_font_licence_check;";
        let tables = parse_schema(ddl).expect("dropped constraint parses");
        assert_eq!(tables["brand"].columns.len(), 1);
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE absent DROP CONSTRAINT x;"
        )
        .is_err());
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE brand DROP CONSTRAINT has space;"
        )
        .is_err());
    }

    #[test]
    fn an_added_column_joins_its_table_and_brand_carries_its_website_url() {
        let sql = "CREATE TABLE firm (\n id UUID PRIMARY KEY\n);\n\
                   CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
                   ALTER TABLE brand ADD COLUMN firm_id UUID REFERENCES firm(id);\n\
                   ALTER TABLE brand ADD COLUMN website_url TEXT CHECK (website_url <> '');";
        let tables = parse_schema(sql).expect("ALTER TABLE ADD COLUMN parses");
        assert_eq!(tables["brand"].columns["website_url"], "TEXT");
        assert!(tables["brand"]
            .foreign_keys
            .contains(&("firm_id".to_string(), "firm".to_string())));
        assert!(parse_schema("ALTER TABLE brand DROP COLUMN name;").is_err());
        assert!(parse_schema("ALTER TABLE absent ADD COLUMN name TEXT;").is_err());

        let altered = parse_schema(&format!(
            "{sql}\nALTER TABLE brand DROP COLUMN firm_id;\n\
             ALTER TABLE brand ALTER COLUMN website_url TYPE CITEXT;\n\
             DROP VIEW brand_directory;"
        ))
        .expect("DROP COLUMN, ALTER COLUMN TYPE and DROP VIEW parse");
        assert_eq!(altered["brand"].columns["website_url"], "CITEXT");
        assert!(!altered["brand"].columns.contains_key("firm_id"));
        assert!(altered["brand"].foreign_keys.is_empty());
        assert!(!altered["brand"].references.contains("firm"));
        assert!(parse_schema(&format!("{sql}\nALTER TABLE brand DROP COLUMN absent;")).is_err());

        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        assert_eq!(postgres["person"].columns["email"], "CITEXT");
        assert!(postgres["brand"].columns.contains_key("website_url"));
        let parity = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        assert!(
            !parity.iter().any(|difference| {
                difference.table.as_deref() == Some("brand")
                    && (difference.missing_column.as_deref() == Some("website_url")
                        || difference.extra_column.as_deref() == Some("website_url"))
            }),
            "{parity:?}"
        );
    }

    #[test]
    fn a_named_check_constraint_on_an_existing_table_parses() {
        let sql = "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
                   ALTER TABLE brand ADD CONSTRAINT brand_font_licence_check CHECK (font_licence IS NULL OR font_licence IN ('OFL-1.1', 'Apache-2.0', 'UFL-1.0'));";
        let tables = parse_schema(sql).expect("named CHECK constraint parses");
        assert!(tables.contains_key("brand"));
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE brand ADD CONSTRAINT broken CHECK;"
        )
        .is_err());
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE absent ADD CONSTRAINT missing CHECK (id IS NOT NULL);"
        )
        .is_err());
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE brand ADD CONSTRAINT unbalanced CHECK (id IS NOT NULL;"
        )
        .is_err());
        assert!(parse_schema(
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
             ALTER TABLE brand ADD CONSTRAINT invalid-name CHECK (id IS NOT NULL);"
        )
        .is_err());
    }

    #[test]
    fn migrations_sort_by_timestamp_and_reject_invalid_versions() {
        let assembled = assemble_migrations(&[
            ("20261011120000_second.sql", "SELECT 2;"),
            ("20261010120000_first.sql", "SELECT 1;"),
        ])
        .expect("migrations assemble");
        assert!(
            assembled.find("SELECT 1;").expect("first")
                < assembled.find("SELECT 2;").expect("second")
        );
        assert!(assemble_migrations(&[("first.sql", "SELECT 1;")]).is_err());
    }

    #[test]
    fn current_surreal_tables_and_columns_produce_stable_differences() {
        let postgres = parse_schema(canonical_schema()).expect("canonical SQL parses");
        let actual = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        let second = compare_to_surreal(
            &postgres,
            &crate::schema::table_names(),
            crate::schema::table_columns,
        );
        assert_eq!(actual, second);
        assert!(!actual
            .iter()
            .any(|difference| { difference.missing_table.as_deref() == Some("relationship") }));
        assert!(actual.iter().any(|difference| {
            difference.extra_column.as_deref() == Some("memo")
                && difference.table.as_deref() == Some("journal_entry")
        }));
        assert!(
            !actual
                .iter()
                .any(|difference| difference.table.as_deref() == Some("asset")),
            "asset carries every Surreal column and relationship"
        );
    }

    #[test]
    fn source_links_require_a_foreign_key_or_a_uuid_column() {
        let source_tables = vec!["child".to_string(), "parent".to_string()];
        let columns = |table: &str| match table {
            "child" => vec![
                ("id".to_string(), "record".to_string()),
                ("parent_id".to_string(), "record<parent>".to_string()),
            ],
            _ => vec![("id".to_string(), "record".to_string())],
        };

        let without_column = parse_schema(
            "CREATE TABLE child (\n id UUID PRIMARY KEY\n);\nCREATE TABLE parent (\n id UUID PRIMARY KEY\n);",
        )
        .unwrap();
        assert!(compare_to_surreal(&without_column, &source_tables, columns)
            .iter()
            .any(|difference| difference.missing_column.as_deref()
                == Some("relationship parent_id → parent")));

        let retained_uuid = parse_schema(
            "CREATE TABLE child (\n id UUID PRIMARY KEY,\n parent_id UUID\n);\nCREATE TABLE parent (\n id UUID PRIMARY KEY\n);",
        )
        .unwrap();
        assert!(!compare_to_surreal(&retained_uuid, &source_tables, columns)
            .iter()
            .any(|difference| difference.missing_column.as_deref()
                == Some("relationship parent_id → parent")));
    }

    #[test]
    fn erd_generation_is_deterministic_and_has_relationships() {
        let first = render_erd().expect("ERD renders");
        let second = render_erd().expect("ERD renders again");
        assert_eq!(first, second);
        assert!(first.starts_with("erDiagram\n"));
        assert!(first.contains("firm }o--|| brand : references"));
        let source_first = render_surreal_source_erd();
        let source_second = render_surreal_source_erd();
        assert_eq!(source_first.as_bytes(), source_second.as_bytes());
        assert_eq!(
            source_first
                .lines()
                .filter(|line| line.starts_with("    ") && line.ends_with(" {"))
                .count(),
            crate::schema::table_names().len(),
            "source ERD includes every declared Surreal table"
        );
        assert!(source_first.contains("project {"));
        assert!(source_first.contains("relationship_log {"));
        assert!(source_first.contains("project }o--o{ person : project_reference"));
        assert!(
            source_first.lines().all(|line| {
                line.split_whitespace()
                    .next()
                    .is_none_or(|name| !name.contains('|'))
            }),
            "a union record is one edge per table so Mermaid can parse the diagram"
        );
        assert!(source_first.contains("person }o--|| cap_table_holding : holder_id"));
        assert!(source_first.contains("number parameters_attorney_pct"));
        assert!(source_first.contains("string parties___name"));
        assert_eq!(
            render_parity_manifest().expect("manifest serializes"),
            render_parity_manifest().expect("manifest serializes again")
        );
    }

    #[test]
    fn confidence_fields_are_numeric_in_both_schema_diagrams() {
        let source = super::render_surreal_source_erd();
        let target = super::render_erd().unwrap();
        for field in ["project_confidence", "privilege_confidence"] {
            let label = format!("number {field}");
            let source_numeric = source.contains(&label);
            let target_numeric = target.contains(&label);
            assert!(source_numeric && target_numeric,
                "{field} must be numeric in both diagrams: source={source_numeric}, target={target_numeric}");
        }
    }

    #[test]
    fn glossary_erd_svg_names_every_surreal_table() {
        let svg = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../server/public/glossary/ERD.svg"
        ))
        .expect("server/public/glossary/ERD.svg is the public copy of the Surreal diagram");
        for table in crate::schema::table_names() {
            assert!(
                svg.contains(&format!("entity-{table}-")),
                "{table} is missing from server/public/glossary/ERD.svg"
            );
        }
    }

    #[test]
    fn checked_in_source_erd_and_manifest_match_the_surreal_schema() {
        assert_eq!(
            SOURCE_ERD,
            render_surreal_source_erd(),
            "regenerate store/postgres/surreal-source-erd.mmd after source schema changes"
        );
        assert_eq!(
            PARITY_MANIFEST,
            render_parity_manifest().expect("manifest serializes"),
            "regenerate store/postgres/parity-manifest.json after schema or migration changes"
        );
    }

    #[test]
    fn seed_coverage_flags_missing_target_tables_and_columns() {
        let sql = "CREATE TABLE person (\n id UUID PRIMARY KEY,\n email TEXT NOT NULL\n);";
        let postgres = parse_schema(sql).expect("fixture SQL parses");
        let seeds = [(
            "Person.yaml",
            "records:\n  - email: x@example.test\n    name: Synthetic\n",
        )];
        let differences = seed_schema_differences(&seeds, &postgres).expect("seeds parse");
        assert_eq!(
            differences,
            vec!["Person.yaml: record 1 uses person.name, absent from PostgreSQL"]
        );
    }

    #[test]
    fn seed_contract_passes_when_lookup_and_synthetic_fields_are_represented() {
        let sql = "CREATE TABLE person (\n id UUID PRIMARY KEY,\n email TEXT NOT NULL,\n name TEXT NOT NULL\n);";
        let postgres = parse_schema(sql).expect("fixture SQL parses");
        let seeds = [("Person.yaml", "lookup_fields:\n  - email\nrecords:\n  - email: synthetic@example.test\n    name: Synthetic Person\n")];
        let differences = seed_schema_differences(&seeds, &postgres).expect("seeds parse");
        assert!(differences.is_empty());
    }

    #[test]
    fn role_seed_uses_person_email_instead_of_inventing_a_person_id_field() {
        let sql = "CREATE TABLE person (\n id UUID PRIMARY KEY,\n email TEXT NOT NULL,\n role TEXT NOT NULL\n);";
        let postgres = parse_schema(sql).expect("fixture SQL parses");
        let seeds = [("User.yaml", "lookup_fields:\n  - person_id\nrecords:\n  - person:\n      email: synthetic@example.invalid\n    role: admin\n")];
        let differences = seed_schema_differences(&seeds, &postgres).expect("seeds parse");
        assert!(differences.is_empty());
    }

    #[test]
    fn malformed_ddl_is_refused_line_by_line() {
        for ddl in [
            "CREATE TABLE brand (\nCREATE TABLE firm (\n",
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\nALTER TABLE brand ADD COLUMN x TEXT\n",
            "CREATE TABLE brand (\n id UUID PRIMARY KEY,\nALTER TABLE brand ADD COLUMN x TEXT;\n",
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\nALTER TABLE brand ADD CONSTRAINT x CHECK (a))(b);\n",
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\nALTER TABLE brand ADD CONSTRAINT x CHECK (a = 'b);\n",
            "CREATE TABLE brand (\n id UUID PRIMARY KEY\n",
        ] {
            assert!(
                matches!(parse_schema(ddl), Err(Error::UnsupportedSql(_))),
                "{ddl}"
            );
        }
        let escaped = "CREATE TABLE brand (\n id UUID PRIMARY KEY\n);\n\
                       ALTER TABLE brand ADD CONSTRAINT brand_quote_check CHECK (name <> 'it''s');";
        assert!(parse_schema(escaped).is_ok());
        assert!(valid_check_expression("(a (b))"));
        assert!(!valid_check_expression("(a))(b"));
        assert!(!valid_check_expression("()"));
        assert!(!valid_check_expression("a"));
    }

    #[test]
    fn seed_coverage_names_absent_tables_and_columns_without_values() {
        let postgres = parse_schema(
            "CREATE TABLE person (\n id UUID PRIMARY KEY,\n email TEXT\n);\n\
             CREATE TABLE question (\n id UUID PRIMARY KEY,\n answer_type TEXT\n);\n\
             CREATE TABLE post (\n id UUID PRIMARY KEY,\n brand_key TEXT,\n social_links JSONB\n);\n",
        )
        .unwrap();
        assert!(matches!(
            seed_schema_differences(&[("neon/Person.yaml", "records: [")], &postgres),
            Err(Error::SeedYaml { .. })
        ));
        assert!(matches!(
            seed_schema_differences(&[("neon/Person.yaml", "lookup_fields: []\n")], &postgres),
            Err(Error::MissingRecords(_))
        ));
        let differences = seed_schema_differences(
            &[
                ("neon/AbsentThing.yaml", "records: []\n"),
                (
                    "neon/User.yaml",
                    "lookup_fields:\n  - person_id\n  - project.code\nrecords:\n  - person:\n      email: synthetic@example.invalid\n      nickname: Synthetic\n",
                ),
                (
                    "neon/Question.yaml",
                    "records:\n  - help_text: Synthetic help\n    question_type: string\n",
                ),
                (
                    "neon/Post.yaml",
                    "lookup_fields:\n  - brand\nrecords:\n  - brand: synthetic\n    social: []\n    entity: Synthetic\n",
                ),
            ],
            &postgres,
        )
        .unwrap();
        assert_eq!(
            differences,
            vec![
                "neon/AbsentThing.yaml: target table absent_thing is absent".to_string(),
                "neon/Post.yaml: record 1 uses post.entity_id, absent from PostgreSQL".to_string(),
                "neon/User.yaml: lookup field project.code requires person.project_id".to_string(),
                "neon/User.yaml: record 1 uses person.nickname, absent from PostgreSQL".to_string(),
            ]
        );
        assert!(!format!("{differences:?}").contains("synthetic@example.invalid"));
    }
}
