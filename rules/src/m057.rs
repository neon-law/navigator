//! `M057` — every relative link target must resolve to a file on disk.
//!
//! A markdown cross-reference has to survive in the repo (an editor or
//! GitHub, where a relative path opens the sibling file). This rule
//! resolves each inline-link target against the linking file's own
//! directory and flags the ones that point nowhere — catching typos and
//! dangling references anywhere in the tree. Its sibling
//! [`crate::M061WebPortableLink`] adds the *website* half: whether a
//! resolvable link is also renderable once published at `/glossary`.
//!
//! A `#fragment` must also name a heading (GitHub slug) or an explicit
//! `<a id>` in the target markdown file, or in this file for a bare
//! `#anchor`. Inline links (`[text](target)`) and reference definitions
//! (`[label]: target`) are checked; image embeds (`![alt](src)`) route
//! through the asset seam, not the repo tree.

use std::path::Path;

use crate::links::{document_anchors, link_targets, reference_definition, split_relative};
use crate::{frontmatter, line_byte_range, Rule, SourceFile, Violation};

pub struct M057RelativeLinkResolves;

impl M057RelativeLinkResolves {
    pub const CODE: &'static str = "M057";
}

impl Rule for M057RelativeLinkResolves {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn description(&self) -> &'static str {
        crate::description_for_code(Self::CODE)
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let dir = file.path.parent().unwrap_or_else(|| Path::new("."));
        let mut violations = Vec::new();
        let mut flag = |line_no: usize, message: String| {
            violations.push(Violation {
                code: Self::CODE,
                path: file.path.clone(),
                line: line_no,
                range: line_byte_range(&file.contents, line_no),
                message,
            });
        };
        for (line_no, line) in frontmatter::body_lines(&file.contents) {
            let masked = frontmatter::mask_code_spans(line);
            let targets = link_targets(&masked)
                .into_iter()
                .chain(reference_definition(&masked));
            for target in targets {
                let Some((file_part, fragment)) = split_relative(&target) else {
                    continue;
                };
                let resolved = dir.join(file_part);
                if !file_part.is_empty() && !resolved.exists() {
                    flag(
                        line_no,
                        format!("Relative link `{target}` does not resolve to a file on disk"),
                    );
                    continue;
                }
                let Some(fragment) = fragment else { continue };
                let anchors = if file_part.is_empty() {
                    document_anchors(&file.contents)
                } else if Path::new(file_part)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                {
                    match std::fs::read_to_string(&resolved) {
                        Ok(contents) => document_anchors(&contents),
                        Err(_) => continue,
                    }
                } else {
                    continue;
                };
                if !anchors.contains(&fragment.to_lowercase()) {
                    flag(
                        line_no,
                        format!(
                            "Link `{target}` points at `#{fragment}`, which no heading defines"
                        ),
                    );
                }
            }
        }
        violations
    }
}

#[cfg(test)]
mod tests {
    use super::M057RelativeLinkResolves;
    use crate::{Rule, SourceFile};
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Write `rel` under `dir` with `contents`, creating parents.
    fn write(dir: &TempDir, rel: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        path
    }

    fn source(path: PathBuf, contents: &str) -> SourceFile {
        SourceFile {
            path,
            contents: contents.to_string(),
        }
    }

    #[test]
    fn flags_a_link_to_a_missing_sibling() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "docs/guide.md", "See [the setup](setup.md).\n");
        let v = M057RelativeLinkResolves.lint(&source(path, "See [the setup](setup.md).\n"));
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].code, "M057");
        assert!(v[0].message.contains("setup.md"));
    }

    #[test]
    fn passes_when_the_sibling_exists() {
        let dir = TempDir::new().unwrap();
        write(&dir, "docs/setup.md", "# Setup\n");
        let path = write(&dir, "docs/guide.md", "See [setup](setup.md).\n");
        assert!(M057RelativeLinkResolves
            .lint(&source(path, "See [setup](setup.md).\n"))
            .is_empty());
    }

    #[test]
    fn resolves_parent_relative_paths() {
        let dir = TempDir::new().unwrap();
        write(&dir, "store/foo.rs", "// code\n");
        let body = "Impl in [foo](../store/foo.rs).\n";
        let path = write(&dir, "docs/guide.md", body);
        assert!(M057RelativeLinkResolves
            .lint(&source(path, body))
            .is_empty());
        // A typo in the same path is caught.
        let bad = "Impl in [foo](../store/typo.rs).\n";
        let path2 = write(&dir, "docs/guide2.md", bad);
        assert_eq!(M057RelativeLinkResolves.lint(&source(path2, bad)).len(), 1);
    }

    #[test]
    fn strips_anchor_before_resolving() {
        let dir = TempDir::new().unwrap();
        write(&dir, "docs/glossary.md", "# Glossary\n\n## Lawyer Review\n");
        let body = "See [term](glossary.md#lawyer-review).\n";
        let path = write(&dir, "docs/guide.md", body);
        assert!(M057RelativeLinkResolves
            .lint(&source(path, body))
            .is_empty());
    }

    #[test]
    fn ignores_absolute_urls_anchors_and_placeholders() {
        let dir = TempDir::new().unwrap();
        let body =
            "# Intro\n\n[site](https://www.neonlaw.com) [top](#intro) [mail](mailto:x@mail.example) \
                    [abs](/docs/glossary) [tmpl]({{confirm_url}})\n";
        let path = write(&dir, "docs/guide.md", body);
        assert!(
            M057RelativeLinkResolves
                .lint(&source(path, body))
                .is_empty(),
            "non-repo-relative targets must not be disk-checked"
        );
    }

    #[test]
    fn skips_image_embeds() {
        // Image sources route through the asset seam, not the repo tree.
        let dir = TempDir::new().unwrap();
        let body = "![collage](img/thanks-apple/collage-6.jpg)\n";
        let path = write(&dir, "web/content/blog/post.md", body);
        assert!(
            M057RelativeLinkResolves
                .lint(&source(path, body))
                .is_empty(),
            "image embeds are resolved by the asset seam, not on disk"
        );
    }

    #[test]
    fn ignores_links_inside_code_fences_and_spans() {
        let dir = TempDir::new().unwrap();
        let body = "```\n[x](missing.md)\n```\n\nInline `[y](gone.md)` too.\n";
        let path = write(&dir, "docs/guide.md", body);
        assert!(M057RelativeLinkResolves
            .lint(&source(path, body))
            .is_empty());
    }

    #[test]
    fn flags_a_fragment_no_heading_defines() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "docs/access.md",
            "# Access\n\n## How embedded Rego decides\n",
        );
        let good = "See [rego](access.md#how-embedded-rego-decides).\n";
        let path = write(&dir, "docs/guide.md", good);
        assert!(M057RelativeLinkResolves
            .lint(&source(path, good))
            .is_empty());

        let bad = "See [rego](access.md#how-opa-decides).\n";
        let path = write(&dir, "docs/guide.md", bad);
        let v = M057RelativeLinkResolves.lint(&source(path, bad));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(
            v[0].message.contains("#how-opa-decides"),
            "{}",
            v[0].message
        );
    }

    #[test]
    fn checks_same_file_anchors() {
        let dir = TempDir::new().unwrap();
        let body = "# Guide\n\n## Setup\n\nJump to [setup](#setup) or [gone](#missing).\n";
        let path = write(&dir, "docs/guide.md", body);
        let v = M057RelativeLinkResolves.lint(&source(path, body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("#missing"));
    }

    #[test]
    fn leaves_fragments_on_non_markdown_targets_alone() {
        let dir = TempDir::new().unwrap();
        write(&dir, "store/foo.rs", "// code\n");
        let body = "Impl at [foo](../store/foo.rs#L10).\n";
        let path = write(&dir, "docs/guide.md", body);
        assert!(M057RelativeLinkResolves
            .lint(&source(path, body))
            .is_empty());
    }

    #[test]
    fn checks_reference_definitions() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "skills/impl/SKILL.md",
            "# Implement\n\n## Start current\n",
        );
        let body = "Use [start][a] then [gone][b].\n\n\
                    [a]: ../impl/SKILL.md#start-current\n\
                    [b]: ../impl/SKILL.md#ground-the-issue\n\
                    [c]: ../missing/SKILL.md\n";
        let path = write(&dir, "skills/triage/SKILL.md", body);
        let v = M057RelativeLinkResolves.lint(&source(path, body));
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.iter().any(|x| x.message.contains("#ground-the-issue")));
        assert!(v.iter().any(|x| x.message.contains("missing/SKILL.md")));
    }

    #[test]
    fn leaves_prose_that_only_looks_like_a_definition_alone() {
        let dir = TempDir::new().unwrap();
        let body = "[text]: this is prose\n";
        let path = write(&dir, "docs/guide.md", body);
        assert!(
            M057RelativeLinkResolves
                .lint(&source(path, body))
                .is_empty(),
            "`CommonMark` leftover words are not a destination"
        );
    }
}
