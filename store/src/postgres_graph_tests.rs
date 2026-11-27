//! The AGE graph projection against a real AGE 1.6.0 database, beside
//! SurrealDB. Fixtures live in both stores under the same record UUIDs, so
//! the parity comparison compares the same graph twice: the projection loads
//! from the PostgreSQL mirrors, the Surreal side walks the records the
//! relationship seam wrote, and [`compare_traversal`] must see one graph.

mod walk_row_normalization {
    use super::super::normalized_kind;
    use serde_json::json;

    #[test]
    fn a_structural_tie_normalizes_to_entity_role_at_full_confidence() {
        let row = json!({"edge_type": "HAS_ROLE", "role": "manages"});
        assert_eq!(
            normalized_kind("HAS_ROLE", &row).unwrap(),
            ("entity_role".to_string(), "manages".to_string(), 100)
        );
    }

    #[test]
    fn a_relationship_normalizes_to_its_kind_and_confidence() {
        let row = json!({
            "edge_type": "RELATIONSHIP",
            "kind": "adverse_to",
            "confidence_pct": 80,
        });
        assert_eq!(
            normalized_kind("RELATIONSHIP", &row).unwrap(),
            ("relationship".to_string(), "adverse_to".to_string(), 80)
        );
    }

    #[test]
    fn a_walk_row_without_its_payload_field_is_a_traversal_error() {
        // A RELATIONSHIP row without `kind`, a HAS_ROLE row without
        // `role`, and an unknown edge label all fail loudly rather than
        // emit a row no finding can explain.
        assert!(
            normalized_kind("RELATIONSHIP", &json!({"edge_type": "RELATIONSHIP"})).is_err(),
            "a kind-less relationship row is a traversal error"
        );
        assert!(
            normalized_kind("HAS_ROLE", &json!({"edge_type": "HAS_ROLE"})).is_err(),
            "a role-less structural tie is a traversal error"
        );
        assert!(
            normalized_kind("UNKNOWN_LABEL", &json!({})).is_err(),
            "an unknown edge label is a traversal error"
        );
    }
}
