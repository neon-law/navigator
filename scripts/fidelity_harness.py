#!/usr/bin/env python3
"""Navigator's LibreOffice (soffice) DOCX-to-PDF fidelity harness (LAW-313).

Renders the repo's pleading and agreement notation fixtures twice — Typst PDF
(``navigator notation pdf``) and editable Word (`navigator notation word`) —
converts the Word copy to PDF with LibreOffice, and reports the drift between
the two PDFs: page count, page breaks, clause numbering, signature placement,
and highlighted-placeholder coverage.

This harness is an evaluation deliverable, not an engine switch: the PDF
backend stays Typst (LAW-313's locked decision). Its output feeds
docs/soffice-engine-recommendation.md.

Usage:

    scripts/fidelity_harness.py                     # full run, writes the report
    scripts/fidelity_harness.py --check             # detection helper only
    scripts/fidelity_harness.py --soffice /path/to/soffice

Exit codes: 0 = no material drift, 1 = drift found, 2 = environment missing
(no soffice, no navigator binary, or a render failed).

Material drift means: a page-count change, a numbered-item sequence change, a
signature moving to a different page, or a character-inventory difference
between the normalized text layers (text present in one PDF and absent from
the other). Layout differences none of those detect are recorded in the
report but do not fail the run.
"""

import argparse
import os
import re
import subprocess
import sys
import tempfile
import textwrap
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# Every fixture the harness runs: (label, template path, document class).
# Pleading fixtures are the synthetic CLI test matters; the agreement fixtures
# are two Neon Law templates at opposite size ends (a two-party NDA and the
# Master Services Agreement). Paths are repo-relative.
FIXTURES = [
    (
        "pleading-nv-notice",
        "cli/tests/fixtures/synthetic_nevada_matter/templates/nevada_notice_of_compliance.md",
        "pleading",
    ),
    (
        "pleading-ca-notice",
        "cli/tests/fixtures/synthetic_matter/templates/notice_of_compliance.md",
        "pleading",
    ),
    ("pleading-ca-rfa", "cli/tests/fixtures/synthetic_matter/templates/rfa_responses.md", "pleading"),
    ("agreement-mutual-nda", "templates/notations/neon_law/mutual_nda.md", "agreement"),
    ("agreement-msa", "templates/notations/neon_law/master_services_agreement.md", "agreement"),
]

REPORT_PATH = REPO_ROOT / "docs" / "soffice-fidelity-drift-report.md"

INSTALL_GUIDANCE = """\
navigator: LibreOffice (soffice) was not found — the fidelity harness needs it \
to convert DOCX to PDF.

Install it with one of:
  macOS (lawyer's Mac):    brew install --cask libreoffice
  Debian/Ubuntu (CI):      sudo apt-get install -y libreoffice-writer
  Fedora:                  sudo dnf install libreoffice-writer

If soffice is installed outside PATH, point the harness at it:
  NAVIGATOR_SOFFICE=/Applications/LibreOffice.app/Contents/MacOS/soffice \\
    scripts/fidelity_harness.py
  # or
  scripts/fidelity_harness.py --soffice /path/to/soffice
"""


def find_soffice(explicit: str | None) -> str | None:
    """Locate soffice: --soffice flag, NAVIGATOR_SOFFICE, then PATH."""
    for candidate in [explicit, os.environ.get("NAVIGATOR_SOFFICE")]:
        if candidate and Path(candidate).exists():
            return candidate
    for directory in os.environ.get("PATH", "").split(os.pathsep):
        candidate = Path(directory) / "soffice"
        if candidate.exists():
            return str(candidate)
    return None


def run(argv: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(argv, check=False, capture_output=True, text=True, **kwargs)


@dataclass
class PageText:
    """The text layer of one page, kept raw and normalized for comparison."""

    number: int
    raw: str = ""

    def words(self) -> list[str]:
        return self.raw.split()


@dataclass
class PdfFacts:
    """Everything the harness compares, extracted from one PDF."""

    path: Path
    pages: list[PageText] = field(default_factory=list)
    highlight_pages: set[int] = field(default_factory=set)

    def page_count(self) -> int:
        return len(self.pages)

    def text(self) -> str:
        return "\n".join(page.raw for page in self.pages)


def pdf_pages(pdf: Path) -> list[PageText]:
    """Page text via poppler's pdftotext, one page at a time."""
    info = run(["pdfinfo", str(pdf)])
    count = int(re.search(r"Pages:\s+(\d+)", info.stdout).group(1))
    pages = []
    for number in range(1, count + 1):
        text = run(
            ["pdftotext", "-f", str(number), "-l", str(number), "-layout", str(pdf), "-"]
        ).stdout
        pages.append(PageText(number=number, raw=text))
    return pages


def highlight_pages(pdf: Path) -> set[int]:
    """Pages whose rendering shows the placeholder highlight wash.

    Typst draws the ``{{placeholder}}`` wash as filled shapes and LibreOffice
    draws Word's ``<w:highlight w:val="yellow"/>`` the same way, so the text
    layer cannot see either. Rasterize at 50 dpi with ``pdftoppm`` (PPM is its
    default output) and scan the raw pixels for the yellow family (the
    placeholder wash is ``#fff3a3`` in Typst; LibreOffice's yellow highlight
    renders nearby).
    """
    stem = pdf.with_suffix("")
    result = run(["pdftoppm", "-r", "50", str(pdf), str(stem)])
    if result.returncode != 0:
        return set()
    seen = set()
    for page_png in sorted(stem.parent.glob(stem.name + "-*.ppm")):
        match = re.search(r"-(\d+)\.ppm$", page_png.name)
        if match and ppm_has_yellow(page_png.read_bytes()):
            seen.add(int(match.group(1)))
        page_png.unlink()
    return seen


def ppm_has_yellow(data: bytes) -> bool:
    """True when a P6 (binary PPM) image carries a yellow-family pixel."""
    # Header: magic, width, height, maxval — whitespace/newline separated.
    header = 0
    for _ in range(4):
        header = data.index(b"\n", header) + 1
    pixels = data[header:]
    for offset in range(0, len(pixels) - 2, 3):
        r, g, b = pixels[offset], pixels[offset + 1], pixels[offset + 2]
        if r > 220 and g > 210 and b < 190:
            return True
    return False


def clause_numbers(text: str) -> list[str]:
    """Ordered numbered-item labels in reading order.

    Three shapes the fixtures use: ``REQUEST FOR ADMISSION NO. 4:`` labels
    (matched regardless of line wrapping), line-start roman clause headings
    (``V.``), and line-start arabic headings (``3.``). Rail line numbers
    carry no period or colon and never match.
    """
    labels = re.findall(r"NO\.\s*(\d{1,2}):", text)
    labels += [f"{found}." for found in re.findall(r"(?m)^\s*([IVXLC]+)\.(?=\s*\S)", text)]
    labels += re.findall(r"(?m)^\s*(\d{1,2})\.(?=\s*\S)", text)
    return labels


def draft_banner(pages: list[PageText]) -> list[int]:
    """Pages carrying the draft banner ("PREVIEW, NOT FOR FILING")."""
    return [page.number for page in pages if "PREVIEW" in page.raw]


def normalized_words(text: str) -> list[str]:
    """Tokens with extraction artifacts normalized away.

    The two engines' text layers disagree on things neither renderer chose:
    Typst smart-quotes what the Word renderer leaves straight, LibreOffice's
    page-number fields extract with arbitrary spacing (``Page 2 of 2`` here,
    ``Page2of2`` and a bare ``Page2`` there), and the draft banner is a
    draft-only frame element reported on its own line. Those are not
    fidelity findings, so they are normalized before comparison.
    """
    text = "\n".join(line for line in text.splitlines() if "PREVIEW" not in line)
    text = re.sub(r"Page\s*\d*\s*(?:of\s*\d+)?", " ", text)
    # Railed pleadings carry a 1–28 line-number column; in layout mode those
    # digits lead their line. A body line never starts a line and is then
    # followed by two spaces, so line-leading digits with room after them are
    # rail numbers.
    text = re.sub(r"(?m)^\s*\d{1,2}(?=\s{2,}\S)", "", text)
    text = text.replace("’", "'").replace("‘", "'")
    text = text.replace("“", '"').replace("”", '"')
    text = text.replace("–", "-").replace("—", "-")
    return text.split()


def flattened(text: str) -> str:
    """The normalized text with whitespace removed and rules dropped.

    Removing whitespace first is what makes narrow-table wrapping
    comparable: LibreOffice wraps 20-underscore signature rules and long
    tokens inside table cells, splitting them around neighboring column
    text. Signature rules themselves are frame decoration — their presence
    is what the signature-page check covers — so underscores leave the
    inventory entirely.
    """
    text = re.sub(r"\s+", "", " ".join(normalized_words(text)))
    return text.replace("_", "")


def signature_page(pages: list[PageText]) -> int | None:
    """The first page carrying a signature line or verification block."""
    for page in pages:
        lowered = page.raw.lower()
        if "/s/" in lowered or "signature" in lowered or "verify" in lowered:
            return page.number
    return None


def missing_words(typst: PdfFacts, soffice: PdfFacts) -> list[str]:
    """Character-inventory difference between the two normalized text layers.

    Token-set comparisons report line-wrap artifacts as lost words: a token
    that wraps mid-word in one engine (``R-0000-`` / ``SAMPLE``) is absent as
    a token from the other's layer while every character is present. The
    honest content-loss check therefore compares multisets of characters
    after normalization — wrapping and reordering cancel out, a dropped word
    cannot.
    """
    ours = Counter(flattened(typst.text()))
    theirs = Counter(flattened(soffice.text()))
    difference = (ours - theirs) + (theirs - ours)
    return sorted(set(difference.elements()))[:40]


def page_break_drift(typst: PdfFacts, soffice: PdfFacts, cap: int = 12) -> list[str]:
    """Words sitting on a different page in the two PDFs (page-break drift).

    Capped at ``cap`` entries: a reflowed fixture drifts dozens of words and
    the full list reads the same as its head.
    """
    where = {}
    for page in typst.pages:
        for word in normalized_words(page.raw):
            where.setdefault(word, page.number)
    drift = []
    for word, page_number in where.items():
        landing = next(
            (p.number for p in soffice.pages if word in normalized_words(p.raw)), None
        )
        if landing is not None and landing != page_number:
            drift.append(f"{word!r}: page {page_number} → {landing}")
    drifting = sorted(set(drift))
    if len(drifting) > cap:
        return drifting[:cap] + [f"… and {len(drifting) - cap} more drifting words"]
    return drifting


def soffice_convert(soffice: str, docx: Path, outdir: Path) -> Path:
    """DOCX → PDF with a headless soffice; returns the converted PDF path."""
    result = run(
        [
            soffice,
            "--headless",
            "--norestore",
            "--convert-to",
            "pdf:writer_pdf_Export",
            "--outdir",
            str(outdir),
            str(docx),
        ],
        timeout=180,
    )
    converted = outdir / (docx.stem + ".pdf")
    if result.returncode != 0 or not converted.exists():
        raise RuntimeError(f"soffice conversion failed for {docx.name}: {result.stderr}")
    return converted


def compare(label: str, typst: PdfFacts, soffice: PdfFacts) -> tuple[bool, list[str]]:
    """Returns (material_drift, report lines) for one fixture."""
    lines = []
    material = False

    t_pages, s_pages = typst.page_count(), soffice.page_count()
    pages_line = f"- Pages: Typst {t_pages}, LibreOffice {s_pages}"
    lines.append(pages_line + ("" if t_pages == s_pages else " — **material drift**"))
    material |= t_pages != s_pages

    t_sig, s_sig = signature_page(typst.pages), signature_page(soffice.pages)
    sig_line = f"- Signature page: Typst {t_sig}, LibreOffice {s_sig}"
    lines.append(sig_line + ("" if t_sig == s_sig else " — **material drift**"))
    material |= t_sig != s_sig

    t_clauses = clause_numbers(typst.text())
    s_clauses = clause_numbers(soffice.text())
    clauses_line = f"- Numbered items: Typst {t_clauses}, LibreOffice {s_clauses}"
    lines.append(clauses_line + ("" if t_clauses == s_clauses else " — **material drift**"))
    material |= t_clauses != s_clauses

    drift = page_break_drift(typst, soffice)
    lines.append(f"- Page-break drift: {'none' if not drift else '; '.join(drift)}")

    absent = missing_words(typst, soffice)
    inventory = "identical" if not absent else "differs — " + ", ".join(repr(c) for c in absent)
    if len(typst.pages) != len(soffice.pages):
        # Each page repeats the running header, so an extra page adds one
        # header copy to the inventory. The page-count line above already
        # flags that drift; this line is the content-loss check for the
        # page-aligned case.
        inventory += " — expected while page counts differ (per-page running headers)"
    lines.append(f"- Character inventory (normalized): {inventory}")
    material |= bool(absent)

    lines.append(
        f"- Draft banner pages: Typst {draft_banner(typst.pages)}, "
        f"LibreOffice {draft_banner(soffice.pages)} — non-material; the banner is a draft-only"
        " frame element, and its absence from the converted PDF is a Word-renderer finding"
    )

    lines.append(
        f"- Highlighted-placeholder pages: Typst {sorted(typst.highlight_pages)}, "
        f"LibreOffice {sorted(soffice.highlight_pages)}"
    )
    lines.append("")
    return material, lines


def render_fixture(navigator: Path, label: str, template: Path, work: Path) -> Path:
    """Render one fixture to Typst PDF and Word DOCX; returns the fixture dir."""
    fixture_dir = work / label
    fixture_dir.mkdir()
    typst_pdf = fixture_dir / f"{label}_typst.pdf"
    word_docx = fixture_dir / f"{label}.docx"
    for argv, out in [
        (["notation", "pdf", str(template), "--out", str(typst_pdf), "--preview"], typst_pdf),
        (["notation", "word", str(template), "--out", str(word_docx), "--preview"], word_docx),
    ]:
        result = run([str(navigator), *argv], cwd=REPO_ROOT)
        if result.returncode != 0 or not out.exists():
            raise RuntimeError(
                f"render failed for {label} ({' '.join(argv[:2])}):\n{result.stdout}\n{result.stderr}"
            )
    return fixture_dir


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--soffice", help="Path to the soffice binary")
    parser.add_argument(
        "--check", action="store_true", help="Detect soffice, print install guidance, exit"
    )
    parser.add_argument(
        "--report", type=Path, default=REPORT_PATH, help="Where to write the Markdown drift report"
    )
    args = parser.parse_args()

    soffice = find_soffice(args.soffice)
    if soffice is None:
        print(INSTALL_GUIDANCE, file=sys.stderr)
        return 2
    if args.check:
        print(f"navigator: soffice found at {soffice}")
        return 0

    navigator = REPO_ROOT / "target" / "debug" / "navigator"
    if not navigator.exists():
        print("navigator: build the CLI first: cargo build -p cli --locked\n", file=sys.stderr)
        return 2
    if run(["pdfinfo", "-v"]).returncode not in (0, 99):
        print("navigator: poppler-utils (pdfinfo/pdftotext/pdftoppm) is required\n", file=sys.stderr)
        return 2

    work = Path(tempfile.mkdtemp(prefix="navigator-fidelity-"))
    any_drift = False
    commit = run(["git", "-C", str(REPO_ROOT), "rev-parse", "--short", "HEAD"]).stdout.strip()
    report = [
        "# LibreOffice DOCX→PDF fidelity drift report",
        "",
        f"Generated by `scripts/fidelity_harness.py` against commit `{commit}`.",
        "",
        "Typst PDFs come from `navigator notation pdf`; the LibreOffice PDFs come from converting the"
        " `navigator notation word` output with headless soffice. Both renders are previews, so"
        " unanswered placeholders survive highlighted and exercise the highlight vocabulary in both"
        " engines.",
        "",
        "Material drift = page-count change, numbered-item sequence change, signature-page change, or a"
        " character-inventory difference between the normalized text layers (text present in one engine and absent"
        " from the other). Everything else is recorded but tolerated.",
        "",
    ]

    for label, template, document_class in FIXTURES:
        try:
            fixture_dir = render_fixture(navigator, label, REPO_ROOT / template, work)
            converted = soffice_convert(soffice, fixture_dir / f"{label}.docx", fixture_dir)
            typst_pdf = fixture_dir / f"{label}_typst.pdf"
            typst_facts = PdfFacts(path=typst_pdf, pages=pdf_pages(typst_pdf))
            typst_facts.highlight_pages = highlight_pages(typst_pdf)
            soffice_facts = PdfFacts(path=converted, pages=pdf_pages(converted))
            soffice_facts.highlight_pages = highlight_pages(converted)
        except (RuntimeError, ValueError) as error:
            print(f"navigator: {error}", file=sys.stderr)
            return 2

        material, lines = compare(label, typst_facts, soffice_facts)
        any_drift |= material
        # M022 wants a blank line on both sides of the heading; M001 wants
        # H2 after H1.
        report.extend([
            "",
            f"## `{label}`",
            "",
            f"_{document_class} fixture, rendered from_ `{template}`",
            "",
        ])
        report.extend(lines)

    report.append(f"**Material drift: {'YES' if any_drift else 'no'}** across {len(FIXTURES)} fixtures.")

    # S101 (the project gate's 120-character limit) applies to this report
    # too. Reflow bullets at write time; code spans in the report never
    # contain spaces, so whitespace-wrapping cannot split one. The gate's
    # S102 packer repacks to its own fill width on contact, so this width
    # only needs to keep a fresh report under the limit.
    wrapped = [
        textwrap.fill(line, width=118, subsequent_indent="  ") if len(line) > 118 else line
        for line in report
    ]
    args.report.write_text("\n".join(wrapped) + "\n")
    print(f"navigator: drift report written to {args.report.relative_to(REPO_ROOT)}")
    return 1 if any_drift else 0


if __name__ == "__main__":
    sys.exit(main())
