//! Composition uses the same evaluator as PDF, Word, and browser previews.

use std::collections::BTreeMap;

#[test]
fn clause_extraction_preserves_the_existing_body_bytes() {
    for (source, expected) in [
        (
            include_str!("../../templates/notations/neon_law/onboarding__letter.md"),
            "4adb979a33d124ae916e99ad6501c414b15dba2eb7845049e7e5169f8775c82a",
        ),
        (
            include_str!("../../templates/notations/neon_law/engagement_letter__nevada.md"),
            "85724fe43756ef26739a5d304da3f6ae52955fc2f28708a63cdb248725ea03d2",
        ),
        (
            include_str!("../../templates/notations/neon_law/mutual_nda.md"),
            "cb7b8825fea7f170a55074d819ef8c1d6fff2e6f8dac3f50e3c47c21e2d11cdc",
        ),
        (
            include_str!("../../templates/notations/neon_law/one_way_nda.md"),
            "db1d29e6d5e5f44aa34ad2a2d545cdb93edb410d08defc980d95636269ff19ec",
        ),
    ] {
        let snapshot = rules::clauses::compile(source).unwrap();
        let (_, body) = rules::frontmatter::split(&snapshot).unwrap();
        assert_eq!(forms::sha256_hex(body.as_bytes()), expected);
        assert!(!body.contains("{{>"));
        assert!(snapshot.contains("clause_sources:"));
        assert_eq!(rules::clauses::compile(&snapshot).unwrap(), snapshot);
    }
}

#[test]
fn remaining_agreement_clauses_preserve_the_compiled_body_bytes() {
    for (source, expected) in [
        (
            include_str!("../../templates/notations/neon_law/advisor_agreement.md"),
            "9800b1261de9fc61794319cb2839f030e58eb89e1e29995b4ee4ac795741b958",
        ),
        (
            include_str!("../../templates/notations/neon_law/master_services_agreement.md"),
            "438f8633d7b469f252bd48984c3f473232c40848d07b7720d1f353a280715640",
        ),
        (
            include_str!("../../templates/notations/neon_law/business_associate_agreement.md"),
            "a564785aff8a785bd35fae4b818b355cd4c9596b8d78d6adda7b3294b42999a7",
        ),
        (
            include_str!("../../templates/notations/neon_law/dpa_us.md"),
            "9ea91cc7db340a09d2679cdc06a8d9849828bc7bbac9eaf1f39a948af6a51c83",
        ),
        (
            include_str!("../../templates/notations/neon_law/dpa_global.md"),
            "392d811624ec8c21dde63a340c34fb048f18bf6c38f98a16b032206d2a1f68cc",
        ),
        (
            include_str!("../../templates/notations/neon_law/employee_offer_letter.md"),
            "e5d360602b0ed0a8445ec66d7993946bb6719077a30dfc0a586b268da2b45639",
        ),
    ] {
        let snapshot = rules::clauses::compile(source).unwrap();
        let (_, body) = rules::frontmatter::split(&snapshot).unwrap();
        assert_eq!(forms::sha256_hex(body.as_bytes()), expected);
        assert!(!body.contains("{{>"));
        assert!(snapshot.contains("clause_sources:"));
        assert_eq!(rules::clauses::compile(&snapshot).unwrap(), snapshot);
    }
}

#[test]
fn nda_signature_party_inputs_render_independently_and_remain_required() {
    let source = include_str!("../../templates/notations/neon_law/mutual_nda.md");
    let snapshot = rules::clauses::compile(source).unwrap();
    let (_, body) = rules::frontmatter::split(&snapshot).unwrap();
    let rendered = forms::notation::fill(
        body,
        &BTreeMap::from([
            (
                "entity__company.name".to_string(),
                "First Entity LLC".to_string(),
            ),
            (
                "entity__other_party.name".to_string(),
                "Second Entity LLC".to_string(),
            ),
        ]),
    );
    assert!(rendered.contains("| First Entity LLC |"));
    assert!(rendered.contains("| Second Entity LLC |"));
    assert!(!rules::final_document::scan(&rendered)
        .iter()
        .any(|finding| {
            matches!(
                &finding.marker,
                rules::final_document::Marker::Placeholder(state)
                    if state == "entity__company.name" || state == "entity__other_party.name"
            )
        }));

    let incomplete = forms::notation::fill(
        body,
        &BTreeMap::from([(
            "entity__company.name".to_string(),
            "First Entity LLC".to_string(),
        )]),
    );
    assert!(incomplete.contains("| First Entity LLC |"));
    assert!(incomplete.contains("{{entity__other_party.name}}"));
}

#[test]
fn arbitration_injects_the_seat_without_changing_the_protections() {
    let source = "---\nkind: onboarding\nquestionnaire: [entity__company]\nclauses:\n  arbitration:\n    source: engagement_arbitration@1\n    inputs:\n      law: jurisdiction__law\n      seat: jurisdiction__seat\n---\n{{> arbitration}}";
    let snapshot = rules::clauses::compile(source).unwrap();
    let (_, body) = rules::frontmatter::split(&snapshot).unwrap();
    for seat in ["Nevada", "California"] {
        let rendered = forms::notation::fill(
            body,
            &BTreeMap::from([
                ("jurisdiction__seat".to_string(), seat.to_string()),
                ("jurisdiction__law".to_string(), "Nevada".to_string()),
            ]),
        );
        assert!(rendered.contains(&format!("seated in\n{seat}")));
        assert!(rendered.contains("The law of Nevada governs this letter."));
        assert!(rendered.contains("statutory fee-arbitration right that cannot be waived"));
        assert!(rendered.contains("consult independent counsel"));
        assert!(!rendered.contains("{{"));
        assert!(rules::final_document::scan(&rendered).is_empty());
    }
    let incomplete = forms::notation::fill(body, &BTreeMap::new());
    assert!(rules::final_document::scan(&incomplete)
        .iter()
        .any(|finding| {
            matches!(
                finding.marker,
                rules::final_document::Marker::Placeholder(_)
            )
        }));
    assert!(
        rules::clauses::compile(&source.replace("kind: onboarding", "kind: agreement")).is_err()
    );
}

#[test]
fn nevada_engagement_uses_aaa_and_keeps_the_fee_rights() {
    let source = include_str!("../../templates/notations/neon_law/engagement_letter__nevada.md");
    let snapshot = rules::clauses::compile(source).unwrap();
    let (_, body) = rules::frontmatter::split(&snapshot).unwrap();
    for law in ["Nevada", "California", "Washington"] {
        let rendered = forms::notation::fill(
            body,
            &BTreeMap::from([(
                "custom_single_choice__governing_law".to_string(),
                law.to_string(),
            )]),
        );
        assert!(rendered.contains(&format!("This letter is governed by the law of {law}.")));
        assert!(rendered.contains(&format!("decided under the law of {law}.")));
        assert!(rendered.contains("administered by the American Arbitration Association under its Commercial Arbitration Rules"));
        assert!(rendered.contains("fee-dispute program of the State Bar of Nevada"));
        assert!(rendered.contains("consult independent counsel"));
    }
}

#[test]
fn nevada_engagement_signature_line_names_the_firm_alone() {
    let source = include_str!("../../templates/notations/neon_law/engagement_letter__nevada.md");
    let (_, body) = rules::frontmatter::split(source).unwrap();
    let signature = body.split("## IX. Signatures").nth(1).unwrap();
    assert!(
        signature
            .lines()
            .any(|line| line.trim() == "Guieb & Shook PC"),
        "{signature}"
    );
    assert!(!body.contains("Neon Law, by"));
}

#[test]
fn employee_offer_letter_includes_the_california_clause_only_for_california() {
    let source = include_str!("../../templates/notations/neon_law/employee_offer_letter.md");
    let (frontmatter, _) = rules::frontmatter::split(source).unwrap();
    assert_eq!(
        rules::frontmatter::field(frontmatter, "jurisdiction").as_deref(),
        Some("US")
    );
    let file = rules::SourceFile {
        path: "templates/notations/neon_law/employee_offer_letter.md".into(),
        contents: source.to_string(),
    };
    assert!(rules::Rule::lint(&rules::clauses::ClauseReferences::default(), &file).is_empty());
    let snapshot = rules::clauses::compile(source).unwrap();
    assert!(snapshot.contains("california_employment@1"));
    let (_, body) = rules::frontmatter::split(&snapshot).unwrap();
    assert!(!body.contains("{{>"));
    let context =
        |state: &str| BTreeMap::from([("jurisdiction__work_state".to_string(), state.to_string())]);
    let california = forms::notation::fill(body, &context("California"));
    assert!(california.contains("## California supplement"));
    assert!(california.contains("fair-chance"));
    assert!(california
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .contains("The law of California governs"));
    let texas = forms::notation::fill(body, &context("Texas"));
    assert!(!texas.contains("California supplement"));
    assert!(!texas.contains("fair-chance requirements for any criminal-history"));
    assert!(texas
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .contains("The law of Texas governs"));
}

#[test]
fn privacy_policy_includes_each_supplement_only_when_its_answer_is_yes() {
    let source = include_str!("../../templates/notations/neon_law/privacy_policy.md");
    let file = rules::SourceFile {
        path: "templates/notations/neon_law/privacy_policy.md".into(),
        contents: source.to_string(),
    };
    assert!(rules::Rule::lint(&rules::clauses::ClauseReferences::default(), &file).is_empty());
    let snapshot = rules::clauses::compile(source).unwrap();
    let (_, body) = rules::frontmatter::split(&snapshot).unwrap();
    assert!(!body.contains("{{>"));
    let context = |us: &str, gdpr: &str| {
        BTreeMap::from([
            ("custom_yes_no__us_states".to_string(), us.to_string()),
            ("custom_yes_no__gdpr".to_string(), gdpr.to_string()),
        ])
    };
    let both = forms::notation::fill(body, &context("yes", "yes"));
    assert!(both.contains("Supplement: U.S. state disclosures"));
    assert!(both.contains("Supplement: European, UK, and Swiss information"));
    let us_only = forms::notation::fill(body, &context("yes", "no"));
    assert!(us_only.contains("Supplement: U.S. state disclosures"));
    assert!(!us_only.contains("European, UK, and Swiss"));
    let neither = forms::notation::fill(body, &context("no", "no"));
    assert!(!neither.contains("Supplement:"));
}

#[test]
fn every_dispute_clause_in_the_catalog_names_the_aaa_and_never_jams() {
    for source in [
        include_str!("../../templates/notations/neon_law/mutual_nda.md"),
        include_str!("../../templates/notations/neon_law/one_way_nda.md"),
        include_str!("../../templates/notations/neon_law/advisor_agreement.md"),
        include_str!("../../templates/notations/neon_law/master_services_agreement.md"),
        include_str!("../../templates/notations/neon_law/employee_offer_letter.md"),
        include_str!("../../templates/notations/neon_law/onboarding__letter.md"),
        include_str!("../../templates/notations/neon_law/engagement_letter__nevada.md"),
    ] {
        let snapshot = rules::clauses::compile(source).unwrap();
        assert!(snapshot.contains("American Arbitration Association"));
        assert!(!snapshot.contains("JAMS"));
    }
}
