//! `navigator notation kinds` — every `kind:` value and every `output:`
//! value a notation template may declare, with the frame each gets.
//!
//! Both tables are read from `rules` ([`rules::Kind`] and
//! [`rules::F109OutputFormat`]), the same source `S103` and `N109` validate
//! against, so this listing cannot drift from the gate.

use std::fmt::Write as _;
use std::process::ExitCode;

use rules::kind::Lane;
use rules::{F109OutputFormat, Kind};

/// Shown under `navigator notation pdf --help` and `word --help`. Kept
/// static for clap; `help_names_every_notation_kind_under_its_default_frame`
/// pins it to [`Kind::default_output`].
pub const RENDER_FRAME_HELP: &str = "\
Default frame by `kind:` (override with `output:`; run `navigator notation kinds` for the full list):
  letter         letter, onboarding, offboarding, memo   (firm letterhead, even with no `output:`)
  form           filing                                  (AcroForm fill; needs `form:` and `origin_url:`)
  pleading       pleading                                (court paper; needs a calibrated `jurisdiction:`)
  contract       agreement                               (no letterhead)
  plain          will, trust, directive, trial_prep      (no letterhead; trial prep is lawyer-only)
  resume         resume                                  (no letterhead; dense, centered name header)

`output:` accepts letter, client_letter, contract, pleading, form, resume. `plain` is never declared: omit `output:`.
For a letter sent in a client's own name, declare `kind: letter` with `output: client_letter` (no letterhead).";

/// The listing `navigator notation kinds` prints.
#[must_use]
pub fn render() -> String {
    let mut out = String::from("kind: values (default frame when `output:` is omitted)\n\n");
    let kinds: Vec<Kind> = Kind::ALL
        .iter()
        .copied()
        .filter(|k| k.valid_for(Lane::Template))
        .collect();
    let width = kinds.iter().map(|k| k.as_str().len()).max().unwrap_or(0);
    for kind in kinds {
        let frame = kind.render_default().unwrap_or("not rendered");
        let _ = writeln!(
            out,
            "  {:<width$}  {:<12}  {}",
            kind.as_str(),
            frame,
            kind.describe()
        );
    }
    out.push_str("\noutput: values (the frame a template may select)\n\n");
    let width = F109OutputFormat::PROFILES
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0);
    for (name, note) in F109OutputFormat::PROFILES {
        let _ = writeln!(out, "  {name:<width$}  {note}");
    }
    out
}

#[must_use]
pub fn run() -> ExitCode {
    print!("{}", render());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{render, RENDER_FRAME_HELP};
    use rules::kind::Lane;
    use rules::{F109OutputFormat, Kind};

    #[test]
    fn listing_names_every_kind_and_output_value() {
        let text = render();
        for kind in Kind::ALL.iter().filter(|k| k.valid_for(Lane::Template)) {
            assert!(text.contains(kind.as_str()), "{} missing", kind.as_str());
        }
        for (name, _) in F109OutputFormat::PROFILES {
            assert!(text.contains(name), "{name} missing");
        }
    }

    #[test]
    fn letter_lists_letterhead_and_directive_lists_plain() {
        let text = render();
        let line = |k: &str| {
            text.lines()
                .find(|l| l.trim_start().starts_with(k))
                .unwrap_or_default()
                .to_string()
        };
        assert!(line("letter ").contains(" letter "), "{}", line("letter "));
        assert!(
            line("directive").contains(" plain "),
            "{}",
            line("directive")
        );
        assert!(line("post").contains("not rendered"), "{}", line("post"));
    }

    #[test]
    fn help_names_every_notation_kind_under_its_default_frame() {
        for kind in Kind::ALL.iter().filter(|k| k.is_notation()) {
            let frame = kind.default_output();
            let line = RENDER_FRAME_HELP
                .lines()
                .find(|l| l.trim_start().starts_with(frame) && l.contains("  "))
                .unwrap_or_default();
            assert!(
                line.contains(kind.as_str()),
                "help does not list {} under {frame}",
                kind.as_str()
            );
        }
    }
}
