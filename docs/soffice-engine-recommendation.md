# One DOCX→PDF engine? LibreOffice evaluation (LAW-313)

## Decision

**Keep Typst as the only PDF backend. Do not adopt LibreOffice as a runtime dependency — in CI or on the lawyer's Mac.**
Keep `scripts/fidelity_harness.py` as an on-demand evaluation tool (`workflow_dispatch`), and re-run it whenever the
Word frame changes materially.

Reversal condition (all three would have to hold at once): Word becomes the source of truth for a document's *print*
form (PDF derived from DOCX rather than rendered alongside it), the drift report starts showing content loss on
page-aligned fixtures — a character-inventory difference that is not attributable to page-count drift — and a court or
recipient rejects the Typst-rendered output for a fidelity reason the frame cannot fix. None holds today.

## What the harness measured

`scripts/fidelity_harness.py` renders five fixtures twice — Typst PDF via `navigator notation pdf`, and PDF converted
from the `navigator notation word` DOCX with headless LibreOffice — then compares page counts, signature-page placement,
numbered-item sequences, highlighted placeholders, page-break positions, and a normalized character inventory of the two
text layers. Full output: `docs/soffice-fidelity-drift-report.md`.

| Fixture | Pages | Signature page | Numbered items | Text inventory |
| --- | --- | --- | --- | --- |
| NV notice of compliance | 1 = 1 | equal | equal | identical |
| CA notice of compliance | 2 = 2 | equal | equal | identical |
| CA RFA responses | 6 vs 7 | 6 vs 7 | equal (46 labels) | differs (see below) |
| Mutual NDA | 2 = 2 | 2 = 2 | equal (I-VI) | identical |
| Master services agreement | 3 = 3 | 3 = 3 | equal (I-X) | identical |

The RFA inventory delta is one extra copy of the per-page running header — the page-count drift's downstream effect, not
missing content. Verified this session: the running heading appears 8 times in the Typst layer and 9 in the LibreOffice
layer, one per page, and no content word is absent from either layer.

### What this says about Word fidelity (the issue's first question)

LibreOffice's import of our DOCX is **textually faithful and layout-drifty**. Every word, placeholder, highlight,
signature rule, and numbered item survives conversion on all five fixtures. What does not survive is geometry:
LibreOffice's Writer lays the same DOCX out with different leading and column widths, and on the longest pleading that
reflow grows the document from six pages to seven and moves the signature page.

That single fact is decisive against adoption. Pleading paper exists to enforce a fixed grid — 28 lines on a 24-point
grid, calibrated per jurisdiction. An engine that reflows our Word output onto an extra page cannot guarantee a
lines-per-page rule, and a motion that grows a signature page in conversion is exactly the failure LAW-313 exists to
prevent. Adopting LibreOffice would mean recalibrating the Word frame against LibreOffice's layout engine — which then
renders *differently in Microsoft Word*, the program lawyers actually open these files in. We would be tuning fidelity
to the converter instead of to the reader.

### What this says about the runtime dependency (the issue's second question)

- **PDF/Word drift** — Adopting: impossible by construction (one path), but only if lawyers print from
  LibreOffice, which they do not. Keeping: real but bounded; the harness measures it on demand.
- **Pleading grid guarantee** — Adopting: lost; conversion reflows (6 → 7 pages observed). Keeping:
  enforced by `Calibration::DEFAULT` in both engines.
- **Dependency weight** — Adopting: ~700 MB of apt packages in CI and a second renderer on every Mac.
  Keeping: nothing beyond the existing toolchain.
- **Reproducible bytes (LAW-19)** — Adopting: conversion output varies with the installed LibreOffice
  version and fonts. Keeping: render inputs pinned to committed data.
- **What the lawyer opens** — Word either way; LibreOffice would only convert.

Adopting LibreOffice would improve one thing: PDF and Word could never diverge, because PDF would be *derived from*
Word. It would worsen everything the format is for: the calibration guarantee, byte reproducibility, dependency weight,
and — the observation the harness contributed — conversion fidelity itself, since LibreOffice demonstrably reflows our
DOCX onto an extra page. And the premise is weaker than it sounds: the drift that matters to a court happens in what the
*court prints*, and the lawyer prints from Microsoft Word either way. One rendering path through LibreOffice would not
make Word's print output agree with anything; it would add a third renderer to the pipeline.

### Harness cadence: why not a PR gate or a clock

The workflow (`.github/workflows/fidelity-harness.yml`) runs on `workflow_dispatch` only. Not a gate: it installs ~700
MB of LibreOffice and builds the CLI a second time in the merge path, to evaluate rather than to verify. Not scheduled:
`docs/gitops.md` and `ci.yml` record the repository contract that nothing runs on a clock. The report is uploaded as a
workflow artifact; copy it over the committed report when the Word frame changes and the drift picture changes with it.

## Rerunning it

```bash
cargo build -p cli --locked
scripts/fidelity_harness.py                # exit 0 = no material drift
scripts/fidelity_harness.py --check        # soffice present? prints install guidance if not
```

Requires LibreOffice Writer (`soffice`) and poppler-utils. On macOS: `brew install --cask libreoffice && brew install
poppler`.
