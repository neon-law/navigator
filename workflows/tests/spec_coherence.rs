//! Cross-check: every bundled template's frontmatter spec must
//! match the standalone `workflows/specs/<code>.yaml`.
//!
//! The template markdown still carries an authoritative copy of the
//! `workflow:` and `questionnaire:` blocks (rendered alongside the
//! body, validated by `workflow_integrity`). The standalone YAML is
//! the format `cli scaffold` will generate; it has to stay in lockstep
//! with the markdown until we delete the duplicated frontmatter.
//! Project-scoped operational specs may be bundled without a catalog
//! template, but every other missing template fails fast.

use std::path::{Path, PathBuf};

use workflows::{
    choices_from_template, choices_from_yaml, prompt_overrides_from_template,
    prompt_overrides_from_yaml, questionnaire_spec_from_template, questionnaire_spec_from_yaml,
    workflow_spec_from_template, workflow_spec_from_yaml, BUNDLED_SPEC_YAML,
};

fn templates_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workflows crate lives one level below the workspace root")
        .join("templates")
}

fn read_template_for(code: &str) -> Option<String> {
    let root = templates_root();
    for entry in walk_markdown(&root) {
        let md = std::fs::read_to_string(&entry)
            .unwrap_or_else(|e| panic!("read {}: {e}", entry.display()));
        if !md.starts_with("---\n") {
            continue;
        }
        // The filename stem is the canonical template code.
        if entry.file_stem().and_then(|stem| stem.to_str()) == Some(code) {
            return Some(md);
        }
    }
    None
}

fn walk_markdown(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_bundled_spec_yaml_matches_its_template_frontmatter() {
    for (code, yaml) in BUNDLED_SPEC_YAML {
        let Some(markdown) = read_template_for(code) else {
            assert_eq!(
                *code, "memo__contract_review",
                "no template found for code `{code}`",
            );
            continue;
        };

        let from_yaml = workflow_spec_from_yaml(yaml)
            .unwrap_or_else(|e| panic!("standalone yaml for `{code}` failed to parse: {e}"));
        let from_template = workflow_spec_from_template(&markdown)
            .unwrap_or_else(|e| panic!("template frontmatter for `{code}` failed to parse: {e}"));
        assert_eq!(
            from_yaml, from_template,
            "workflow spec mismatch between standalone yaml and template frontmatter for `{code}`",
        );

        let q_from_yaml = questionnaire_spec_from_yaml(yaml).unwrap_or_else(|e| {
            panic!("standalone questionnaire yaml for `{code}` failed to parse: {e}")
        });
        let q_from_template = questionnaire_spec_from_template(&markdown).unwrap_or_else(|e| {
            panic!("template questionnaire frontmatter for `{code}` failed to parse: {e}")
        });
        assert_eq!(
            q_from_yaml, q_from_template,
            "questionnaire mismatch between standalone yaml and template frontmatter for `{code}`",
        );

        let prompts_from_yaml = prompt_overrides_from_yaml(yaml).unwrap_or_else(|e| {
            panic!("standalone prompts yaml for `{code}` failed to parse: {e}")
        });
        let prompts_from_template = prompt_overrides_from_template(&markdown).unwrap_or_else(|e| {
            panic!("template prompts frontmatter for `{code}` failed to parse: {e}")
        });
        assert_eq!(
            prompts_from_yaml, prompts_from_template,
            "prompts mismatch between standalone yaml and template frontmatter for `{code}`",
        );

        let cq_from_yaml = choices_from_yaml(yaml).unwrap_or_else(|e| {
            panic!("standalone choices yaml for `{code}` failed to parse: {e}")
        });
        let cq_from_template = choices_from_template(&markdown).unwrap_or_else(|e| {
            panic!("template choices frontmatter for `{code}` failed to parse: {e}")
        });
        assert_eq!(
            cq_from_yaml, cq_from_template,
            "choices mismatch between standalone yaml and template frontmatter for `{code}`",
        );
    }
}

/// Signer roles spelled by a standalone yaml's `signers:` key, if any —
/// bare strings and `{role: …}` entries both count. An absent or null
/// `signers:` yields an empty list (nothing declared on the yaml side).
fn yaml_signer_roles(yaml: &str) -> Vec<String> {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else {
        return Vec::new();
    };
    let Some(signers) = value.get("signers").filter(|s| !s.is_null()) else {
        return Vec::new();
    };
    match signers.as_sequence() {
        Some(entries) => entries
            .iter()
            .filter_map(|entry| {
                if entry.is_string() {
                    entry.as_str().map(String::from)
                } else {
                    entry
                        .get("role")
                        .and_then(|role| role.as_str().map(String::from))
                }
            })
            .collect(),
        None => Vec::new(),
    }
}

/// The compile-time signer gate: a bundled template that declares its own
/// `signers:` list must pair that declaration with questionnaire backing
/// and body anchors, and the standalone yaml must tell the same signer
/// story as the template frontmatter. Drift in any direction (a signer
/// with no questionnaire state, a binding naming no question, a role with
/// no anchor, yaml and markdown disagreeing) fails here, at compile time,
/// instead of at the provider. Implicit default sets carry no backing
/// promise — the send flow resolves the client from the notation's bound
/// Person row — so only a template that actually collects a signature
/// needs one body anchor.
#[test]
fn every_signing_template_pairs_signers_questionnaire_and_anchors() {
    for (code, yaml) in BUNDLED_SPEC_YAML {
        let Some(markdown) = read_template_for(code) else {
            continue;
        };

        let set = rules::f107::signer_set(&markdown)
            .unwrap_or_else(|e| panic!("signers for `{code}` failed to parse: {e}"));

        let questionnaire = questionnaire_spec_from_yaml(yaml).unwrap_or_else(|e| {
            panic!("standalone questionnaire yaml for `{code}` failed to parse: {e}")
        });

        for signer in &set.signers {
            let role = signer.role.as_str();
            // Only an explicit `signers:` list needs questionnaire backing:
            // the implicit default set resolves the client from the
            // notation's bound Person row and the firm from configuration —
            // the same carve-out N115 makes — so there is no `{state}`
            // question to pair.
            if set.explicit && role != "firm" {
                let state = format!("person__{role}");
                assert!(
                    questionnaire
                        .0
                        .states
                        .keys()
                        .any(|name| name.as_str() == state),
                    "`{code}`: signer role `{role}` has no `{state}` question in the bundled questionnaire"
                );
                // A declared identity binding must name a real question, or the
                // send flow resolves nothing at signature time.
                for binding in [signer.name.as_deref(), signer.email.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    assert!(
                        questionnaire
                            .0
                            .states
                            .keys()
                            .any(|name| name.as_str() == binding),
                        "`{code}`: signer role `{role}` binds `{binding}` but the bundled questionnaire has no such question"
                    );
                }
            }
            // Anchor pairing (the N107 direction): a declared role must
            // have somewhere in the body to sign. Implicit default roles
            // carry no anchor promise — a template that signs with the
            // implicit set only needs one anchor (the reverse check
            // below).
            if set.explicit {
                assert!(
                    markdown.contains(&format!("{{{{{role}.signature}}}}"))
                        || markdown.contains(&format!("{{{{{role}.initials}}}}")),
                    "`{code}`: signer role `{role}` has no `{{{role}.signature}}` or `{{{role}.initials}}` anchor in the body"
                );
            }
        }

        // Reverse anchor pairing: a template whose workflow declares a
        // signing state must carry at least one anchor for the field to
        // land on — the live retainer bug shape.
        if rules::f107::declares_signing_state(&markdown) {
            assert!(
                markdown.contains(".signature}}") || markdown.contains(".initials}}"),
                "`{code}` declares a sent_for_signature state but its body has no signature anchor"
            );
        }

        // Signer drift between the standalone yaml and the template
        // frontmatter, in both directions and in order.
        let yaml_roles = yaml_signer_roles(yaml);
        if !yaml_roles.is_empty() {
            let markdown_roles: Vec<String> = set.roles().map(String::from).collect();
            assert_eq!(
                yaml_roles, markdown_roles,
                "`{code}`: signer drift between the standalone yaml and the template frontmatter"
            );
        }
    }
}
