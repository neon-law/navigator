//! `N134` — an agreement selects a letter render profile.

use crate::{frontmatter, line_byte_range, Kind, Rule, SourceFile, Violation};

pub struct F134AgreementLetterOutput;

impl F134AgreementLetterOutput {
    pub const CODE: &'static str = "N134";
}

impl Rule for F134AgreementLetterOutput {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let Some(fm) = frontmatter::extract(&file.contents) else {
            return Vec::new();
        };
        if frontmatter::field(fm, "kind").as_deref() != Some(Kind::Agreement.as_str()) {
            return Vec::new();
        }
        let output = frontmatter::field(fm, "output");
        let Some(output @ ("letter" | "client_letter")) = output.as_deref() else {
            return Vec::new();
        };
        vec![Violation {
            code: Self::CODE,
            path: file.path.clone(),
            line: 1,
            range: line_byte_range(&file.contents, 1),
            message: format!(
                "`kind: agreement` selects a letter frame with `output: {output}`. \
                 Use `output: contract` or omit `output:` for the agreement's default frame"
            ),
        }]
    }
}
