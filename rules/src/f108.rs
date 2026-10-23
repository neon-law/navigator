//! `N108` — notation template codes come from their filename.

use crate::{frontmatter, line_byte_range, Rule, SourceFile, TextEdit, Violation};

pub struct F108TemplateCodeRequired;

impl F108TemplateCodeRequired {
    pub const CODE: &'static str = "N108";
    /// The final release that accepts the retired frontmatter key with a warning.
    pub const FINAL_COMPATIBILITY_RELEASE: &'static str = "26.9.29";
}

impl Rule for F108TemplateCodeRequired {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn description(&self) -> &'static str {
        "Notation template code is derived from its filename"
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let Some(fm) = frontmatter::extract(&file.contents) else {
            return Vec::new();
        };
        let Some(declared) = frontmatter::field(fm, "code") else {
            return Vec::new();
        };
        let stem = file
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        let expired = after_compatibility_release(env!("CARGO_PKG_VERSION"));
        if !expired && declared == stem {
            return Vec::new();
        }
        let message = if expired {
            format!(
                "Frontmatter `code:` is no longer accepted; filename stem `{stem}` is the template code"
            )
        } else {
            format!(
                "Frontmatter `code:` `{declared}` does not match filename stem `{stem}`; remove it because the filename supplies the template code"
            )
        };
        let line = fm
            .lines()
            .position(|line| line.starts_with("code:"))
            .map_or(1, |index| index + 2);
        vec![Violation {
            code: Self::CODE,
            path: file.path.clone(),
            line,
            range: line_byte_range(&file.contents, line),
            message,
        }]
    }

    fn fix(&self, file: &SourceFile, violation: &Violation) -> Option<TextEdit> {
        let start = violation.range.start;
        let line = file.contents.get(start..)?;
        let terminator = line.find('\n').map_or(line.len(), |end| end + 1);
        Some(TextEdit {
            range: start..start + terminator,
            new_text: String::new(),
        })
    }
}

fn after_compatibility_release(current: &str) -> bool {
    let Ok(current) = semver::Version::parse(current) else {
        return false;
    };
    let Ok(final_release) =
        semver::Version::parse(F108TemplateCodeRequired::FINAL_COMPATIBILITY_RELEASE)
    else {
        return false;
    };
    current > final_release
}

#[cfg(test)]
mod tests {
    use super::{after_compatibility_release, F108TemplateCodeRequired};
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    fn file(body: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("onboarding__letter.md"),
            contents: body.to_string(),
        }
    }

    #[test]
    fn accepts_missing_code() {
        assert!(F108TemplateCodeRequired
            .lint(&file("---\ntitle: T\n---\n"))
            .is_empty());
    }

    #[test]
    fn rejects_frontmatter_code_that_matches_the_filename() {
        let violation = F108TemplateCodeRequired
            .lint(&file("---\ntitle: T\ncode: onboarding__letter\n---\n"))
            .remove(0);
        assert_eq!(violation.code, "N108");
        assert!(violation.message.contains("no longer accepted"));
        assert!(violation.message.contains("onboarding__letter"));
    }

    #[test]
    fn removes_frontmatter_code_that_does_not_match_the_filename() {
        let source = file("---\ntitle: T\ncode: stale_code\n---\n");
        let violation = F108TemplateCodeRequired.lint(&source).remove(0);
        assert_eq!(violation.code, "N108");
        assert!(violation.message.contains("no longer accepted"));
        assert!(violation.message.contains("onboarding__letter"));
        let edit = F108TemplateCodeRequired
            .fix(&source, &violation)
            .expect("frontmatter code can be removed safely");
        let mut fixed = source.contents.clone();
        fixed.replace_range(edit.range, &edit.new_text);
        assert_eq!(fixed, "---\ntitle: T\n---\n");
    }

    #[test]
    fn compatibility_ends_after_the_one_accepting_release() {
        assert!(!after_compatibility_release("26.9.29"));
        assert!(after_compatibility_release("26.9.30"));
        assert!(after_compatibility_release("26.9.30-rc.1"));
    }
}
