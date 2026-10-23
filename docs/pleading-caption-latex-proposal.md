# Pleading caption and export proposal

Implement structured court captions and verification blocks shared by the PDF and Word projections of a Template. The
current generic Markdown route can render a pleading, but it does not expose the court-caption structure, keep-together
controls or matching Word rail required for dependable authoring. This proposal and synthetic LaTeX fixture do not
change the production renderer.

## Top gap

The implementation task is [LAW-106](https://linear.app/neon-law/issue/LAW-106). The native PDF renderer is Typst:
`pdf/src/pleading.rs` supplies the rail, `pdf/src/format.rs` the page preamble, and `pdf/src/markdown.rs` the body.
Review page numbering and document-title footers in that native path before choosing another backend. The Word path in
`cli/src/main.rs` and `word/src/render.rs` must preserve the selected pleading profile instead of producing ordinary
letter layout.

Compare measured geometry with the official California Courts [pleading-paper
packet](https://courts.ca.gov/partners/documents/pleading_paper_packet.pdf). Check current rules 2.108, Check footer and
caption rules in that review. Reference pages 3, 5 and 7 contain 25 printed labels. Label counts alone do not establish
compliance. Capture the reference bytes, verify the rules, and render every acceptance-fixture page.

Preserve attorney contact hard breaks, a two-column party and case caption, document title, page numbering,
request-response grouping, and an intact verification. Substitution values must be escaped for the target syntax and
must not become executable LaTeX or Typst. Never put client data in fixtures.

The present PDF calibration intentionally uses a 27-line 24-point grid. An optional 28-line profile needs its own
baseline and margin measurements; changing the global count alone is insufficient. Do not describe the current count
alone as a proved rule violation. Word must use the same profile measurements instead of silently losing the rail.

## LaTeX spike

`examples/pleading-paper-ca-spike.tex` is a standalone synthetic layout. It demonstrates an explicit 28-label shipout
rail, a caption, repeat-page footer and keep-together verification. It is a layout experiment, not a compliant-court
certification or an integrated backend. The 12-point body and 24-point leading are explicit.

Use [geometry](https://ctan.org/pkg/geometry) for dimensions and [eso-pic](https://ctan.org/pkg/eso-pic) for fixed
shipout positions. A public [law-latex example](https://github.com/mtracy-quantum/law-latex) is GPL-3.0; it was
inspected as a reference and its code was not copied.

## Implementation boundaries

1. Parse a structured caption and verification in the Template/render model, with a documented migration for existing
   Markdown.

2. Resolve layout from jurisdiction and an explicit profile; preserve the existing default.

3. Render the shared model into native PDF and DOCX. Evaluate LaTeX as an optional PDF backend rather than install TeX
   at run time.

4. Preserve source answers, Template revision, profile and renderer version as provenance. Keep actual signatures and
   government-form AcroForms on their separate paths.

5. Keep rendered documents in the asset lane and send only through Lawyer Review. A renderer must never certify
   anticipated service.

## Acceptance checks

Use synthetic long-caption and multi-page request fixtures. Assert all caption fields survive extraction, every page is
upright and searchable, profile labels and baseline positions agree, page numbers are consecutive, the verification is
not split, special characters are escaped, and Word preserves its rail after PDF conversion. Validate native renderer
tests before implementing this proposal.
