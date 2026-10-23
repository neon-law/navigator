//! `N129` — a notation's `audiences:` may only narrow its `custom_*`
//! questions.
//!
//! Which side of the intake sees a question is the `question.audience`
//! column, seeded from `store/seeds/Question.yaml`. A record or reference
//! type — `person`, `entity`, `address`, `project` — is one row shared by
//! every notation, so its audience is declared once there. A `custom_*`
//! primitive is different: `custom_text__business_fees` exists only in the
//! notation that asks it, so that notation's `audiences:` map is the one
//! place its audience can be written.
//!
//! An `audiences:` key is the role after the state's `__`
//! (`custom_datetime__effective_date` → `effective_date`). A key that names
//! no `custom_*` state is either a record type's audience declared in the
//! wrong place or a key that matches nothing; both are errors.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{frontmatter, line_byte_range, Rule, SourceFile, Violation};

pub struct F129AudiencesOnlyCustom;

impl F129AudiencesOnlyCustom {
    pub const CODE: &'static str = "N129";
}

#[derive(Deserialize)]
struct Frontmatter {
    #[serde(default)]
    audiences: BTreeMap<String, serde_yaml::Value>,
    #[serde(default)]
    questionnaire: BTreeMap<String, serde_yaml::Value>,
}

impl Rule for F129AudiencesOnlyCustom {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let Some(fm) = frontmatter::extract(&file.contents) else {
            return Vec::new();
        };
        // Unparseable frontmatter is another rule's finding.
        let Ok(parsed) = crate::questionnaire::deserialize_frontmatter::<Frontmatter>(fm) else {
            return Vec::new();
        };
        parsed
            .audiences
            .keys()
            .filter(|key| {
                !parsed.questionnaire.keys().any(|state| {
                    state.starts_with("custom_")
                        && state.split_once("__").is_some_and(|(_, role)| role == *key)
                })
            })
            .map(|key| Violation {
                code: Self::CODE,
                path: file.path.clone(),
                line: 1,
                range: line_byte_range(&file.contents, 1),
                message: format!(
                    "`audiences:` key `{key}` names no `custom_*` questionnaire state; a record \
                     type's audience belongs on its row in store/seeds/Question.yaml"
                ),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::F129AudiencesOnlyCustom;
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    fn lint(frontmatter: &str) -> Vec<crate::Violation> {
        F129AudiencesOnlyCustom.lint(&SourceFile {
            path: PathBuf::from("templates/notations/neon_law/onboarding__letter.md"),
            contents: format!("---\n{frontmatter}\n---\n\nBody.\n"),
        })
    }

    const QUESTIONNAIRE: &str = "questionnaire:\n  BEGIN:\n    _: entity__company\n  \
        entity__company:\n    _: custom_datetime__effective_date\n  \
        custom_datetime__effective_date:\n    _: END\n  END: {}";

    #[test]
    fn allows_a_custom_state_audience() {
        let v = lint(&format!(
            "audiences:\n  effective_date: lawyer\n{QUESTIONNAIRE}"
        ));
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn flags_a_record_type_audience() {
        let v = lint(&format!("audiences:\n  company: lawyer\n{QUESTIONNAIRE}"));
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].code, "N129");
        assert!(v[0].message.contains("`company`"), "{v:?}");
    }

    #[test]
    fn flags_a_key_matching_no_state() {
        assert_eq!(
            lint(&format!("audiences:\n  nonsense: lawyer\n{QUESTIONNAIRE}")).len(),
            1
        );
    }

    #[test]
    fn ignores_a_template_without_audiences() {
        assert!(lint(QUESTIONNAIRE).is_empty());
    }
}
