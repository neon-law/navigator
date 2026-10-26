//! S105 keeps document teaching prose aligned with the glossary.

use crate::{line_byte_range, Rule, SourceFile, Violation};

pub struct S105DocumentTerminology;

impl Rule for S105DocumentTerminology {
    fn code(&self) -> &'static str {
        "S105"
    }

    fn description(&self) -> &'static str {
        "Use the glossary's Pointer, Blob, and Matter Folder terms"
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let path = file.path.to_string_lossy().replace('\\', "/");
        if !(path.contains("docs/")
            || path.contains(".agents/skills/")
            || path.ends_with("AGENTS.md")
            || path.ends_with("agent_contract.md"))
        {
            return Vec::new();
        }
        let retired = [
            "pointer yaml",
            "repository pointer",
            "source pointer",
            "yaml pointer (a blob)",
        ];
        file.contents.lines().enumerate().filter_map(|(index, line)| {
            let lower = line.to_ascii_lowercase();
            retired.iter().find(|phrase| lower.contains(**phrase)).map(|_| Violation {
                code: "S105", path: file.path.clone(), line: index + 1,
                range: line_byte_range(&file.contents, index + 1),
                message: "Use Pointer for a tracked document YAML file, Blob for revision bytes, and Matter Folder for their local folder".to_string(),
            })
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teaching_surfaces_refuse_retired_document_names() {
        for path in [
            "docs/project-repositories.md",
            ".agents/skills/legal-writing/SKILL.md",
            "cli/src/projects/agent_contract.md",
            "AGENTS.md",
        ] {
            for phrase in [
                "pointer YAML",
                "repository pointer",
                "source pointer",
                "YAML pointer (a Blob)",
            ] {
                let file = SourceFile {
                    path: path.into(),
                    contents: format!("Allowed line\nCommit the {phrase}.\n"),
                };
                let findings = S105DocumentTerminology.lint(&file);
                assert_eq!(findings.len(), 1, "{path}: {phrase}");
                assert_eq!(findings[0].line, 2);
            }
        }
    }

    #[test]
    fn canonical_words_and_unrelated_uses_pass() {
        let file = SourceFile {
            path: "docs/project-repositories.md".into(),
            contents: "The Pointer names a Blob in a Matter Folder.\n".into(),
        };
        assert!(S105DocumentTerminology.lint(&file).is_empty());
        let file = SourceFile {
            path: "examples/pointers.md".into(),
            contents: "A source pointer in a compiler.\n".into(),
        };
        assert!(S105DocumentTerminology.lint(&file).is_empty());
    }
}
