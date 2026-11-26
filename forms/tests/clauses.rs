//! Composition uses the same evaluator as PDF, Word, and browser previews.

use std::collections::BTreeMap;

#[test]
fn onboarding_clause_compilation_expands_references_and_is_idempotent() {
    let source = include_str!("../../templates/notations/neon_law/onboarding__letter.md");
    let snapshot = rules::clauses::compile(source).unwrap();
    let (_, body) = rules::frontmatter::split(&snapshot).unwrap();
    assert!(!body.contains("{{>"));
    assert!(snapshot.contains("clause_sources:"));
    assert_eq!(rules::clauses::compile(&snapshot).unwrap(), snapshot);
}

#[test]
fn clause_extraction_preserves_the_existing_body_bytes() {
    for (source, expected) in [
        (
            include_str!("../../templates/notations/neon_law/engagement_letter__nevada.md"),
            "6e9fe493afc9080af2388b56f1251aaf7653acf44f7e5c49f12c5559d1143875",
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
            "edbd0b230301246ea1adec69d26fb207044f9ec090870ac015053937b690d42e",
        ),
        (
            include_str!("../../templates/notations/neon_law/dpa_us.md"),
            "7d9e91cdd88b040a16c4926e8abd34ed681f40882e3bd080be8f0fae04f0047a",
        ),
        (
            include_str!("../../templates/notations/neon_law/dpa_global.md"),
            "356a25980d03bba4911d28b37279cd9475e11c6271ec6d0dc1d8daebdbd87795",
        ),
        (
            include_str!("../../templates/notations/neon_law/employee_offer_letter.md"),
            "7e60f8d9f555fecc3d3d3d6e8ab336d409863a2ccbd334c8105999980cf0fdf7",
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
fn mutual_nda_renders_both_parties_and_email_notices() {
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
            (
                "people__notices".to_string(),
                serde_json::json!([
                    {"name": "First Contact", "email": "first@example.com"},
                    {"name": "Second Contact", "email": "second@example.com"}
                ])
                .to_string(),
            ),
        ]),
    );
    assert!(rendered.contains("__For First Entity LLC__\n\n{{company_signer.signature}}"));
    assert!(rendered.contains("First Contact (first@example.com)"));
    assert!(rendered.contains("Second Contact (second@example.com)"));
    assert!(rendered.contains("__For Second Entity LLC__\n\n{{other_party_signer.signature}}"));
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
    assert!(incomplete.contains("__For First Entity LLC__"));
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
            .any(|line| line.trim() == "{{firm.legal_name}}"),
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
fn plan_clause_fixture(reference: &str, bindings: &str, choices: &str) -> String {
    format!(
        "---\nkind: onboarding\nquestionnaire:\n  BEGIN: {{_: jurisdiction__law}}\n  jurisdiction__law: {{_: END}}\n  END: {{}}\nchoices:\n{choices}\nclauses:\n  arbitration:\n    source: engagement_arbitration@1\n    inputs:\n      law: jurisdiction__law\n      seat: jurisdiction__seat\n  terms:\n    source: {reference}\n    inputs:\n{bindings}\n---\n{{{{> terms}}}}\n{{{{> arbitration}}}}\n"
    )
}

fn compile_plan_clause(source: &str) -> String {
    let file = rules::SourceFile {
        path: "synthetic-onboarding.md".into(),
        contents: source.to_owned(),
    };
    assert!(rules::Rule::lint(&rules::clauses::ClauseReferences::default(), &file).is_empty());
    assert!(
        rules::clauses::compile(&source.replace("kind: onboarding", "kind: agreement")).is_err()
    );
    let snapshot = rules::clauses::compile(source).unwrap();
    let compiled = rules::SourceFile {
        path: file.path,
        contents: snapshot.clone(),
    };
    let findings = rules::Rule::lint(&rules::f115::F115PathResolution, &compiled);
    assert!(findings.is_empty(), "{findings:?}");
    rules::frontmatter::split(&snapshot).unwrap().1.to_owned()
}

#[test]
fn arag_renders_each_benefit_and_requires_its_plan_fee() {
    let source = plan_clause_fixture(
        "legal_plan_arag@1",
        "      benefit: custom_single_choice__benefit\n      plan_fee: custom_usd__plan_fee",
        "  benefit:\n    flat_fee: Flat fee\n    consult_only: Consultation only",
    );
    assert!(
        rules::clauses::compile(&source.replace("      plan_fee: custom_usd__plan_fee\n", ""))
            .is_err()
    );
    let body = compile_plan_clause(&source);
    for (benefit, covered) in [
        ("flat_fee", "as a flat fee"),
        ("consult_only", "initial consultation"),
    ] {
        let rendered = forms::notation::fill(
            &body,
            &BTreeMap::from([
                ("custom_single_choice__benefit".into(), benefit.into()),
                ("custom_usd__plan_fee".into(), "$90.00".into()),
            ]),
        );
        assert!(rendered.contains(covered), "{rendered}");
        assert!(rendered.contains("$90.00"));
        assert!(rendered.contains("ARAG pays no costs"));
        assert!(rendered.contains("in writing"));
    }
}

#[test]
fn legalease_renders_each_cap_and_member_billing_combination() {
    let source = plan_clause_fixture(
        "legal_plan_legalease@1",
        concat!(
            "      cap_shape: custom_single_choice__cap_shape\n",
            "      member_billing: custom_single_choice__member_billing\n",
            "      plan_cap: custom_usd__plan_cap\n",
            "      plan_rate: custom_usd__plan_rate\n",
            "      paralegal_rate: custom_usd__paralegal_rate\n",
            "      overage_rate: custom_usd__overage_rate\n",
            "      contested_cap: custom_usd__contested_cap\n",
            "      uncontested_cap: custom_usd__uncontested_cap\n",
            "      current_scope: custom_single_choice__current_scope",
        ),
        concat!(
            "  cap_shape:\n    hourly_cap: Hours\n    dollar_cap: Dollars\n",
            "    contested_uncontested: Contested and uncontested\n    consult_only: Consultation only\n",
            "  member_billing:\n    paid_in_full: Paid in full\n    overage_at_discount: Discounted overage\n",
            "  current_scope:\n    uncontested: Uncontested\n    contested: Contested",
        ),
    );
    let body = compile_plan_clause(&source);
    for (cap_shape, expected) in [
        ("hourly_cap", "authorized hours"),
        ("dollar_cap", "pays up to $800.00"),
        ("contested_uncontested", "The uncontested cap applies now."),
        ("consult_only", "covers only the initial consultation"),
    ] {
        for member_billing in ["paid_in_full", "overage_at_discount"] {
            let mut answers = BTreeMap::from([
                ("custom_single_choice__cap_shape".into(), cap_shape.into()),
                (
                    "custom_single_choice__member_billing".into(),
                    member_billing.into(),
                ),
                (
                    "custom_single_choice__current_scope".into(),
                    "uncontested".into(),
                ),
                ("custom_usd__plan_cap".into(), "$800.00".into()),
                ("custom_usd__plan_rate".into(), "$90.00".into()),
                ("custom_usd__paralegal_rate".into(), "$45.00".into()),
                ("custom_usd__contested_cap".into(), "$900.00".into()),
                ("custom_usd__uncontested_cap".into(), "$600.00".into()),
            ]);
            if member_billing == "overage_at_discount" {
                answers.insert("custom_usd__overage_rate".into(), "$70.00".into());
            }
            let rendered = forms::notation::fill(&body, &answers);
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(normalized.contains(expected), "{normalized}");
            assert!(normalized.contains("LegalEASE pays no costs"));
            assert!(normalized.contains("must approve work beyond the benefit in advance"));
            if member_billing == "paid_in_full" {
                assert!(normalized.contains("We will not bill you for legal fees on this matter."));
                assert!(!normalized.contains("discounted rate"));
                assert!(!normalized.contains("overage_rate"));
            } else {
                assert!(normalized.contains("beyond the benefit is $70.00 per hour"));
            }
        }
    }
}

#[test]
fn technology_clause_renders_the_jurisdiction_and_lawyer_review() {
    let body = compile_plan_clause(&plan_clause_fixture(
        "engagement_technology@1",
        "      law: jurisdiction__law",
        "  {}",
    ));
    let rendered = forms::notation::fill(
        &body,
        &BTreeMap::from([("jurisdiction__law".into(), "Nevada".into())]),
    );
    let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(normalized.contains("our professional obligations in Nevada"));
    assert!(normalized.contains("A lawyer reviews"));
}
