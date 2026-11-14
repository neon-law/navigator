---
name: ocr
description: >-
  Read, transcribe, and file scanned legal papers, including PDF scans and HEIC or JPEG photos. Check source quality and
  file reviewed text as a transcript of the exact source revision.
---

# OCR a scan

## Find the term first

Use the glossary guidance in `AGENTS.md` for Document, Pointer, and Project.

## File the scan and transcript

1. Inspect each original page for order, rotation, cropping, and legibility. Check dimensions with `sips -g pixelWidth
   -g pixelHeight <image>` on macOS. If small print cannot be read at native resolution, ask for full-resolution
   originals before relying on OCR.
2. Preserve the originals. On macOS, convert HEIC to JPEG with `sips -s format jpeg <original> --out <scratch-copy>`.
   Rotate working copies as needed and verify their order visually. Assemble the JPEG copies with `img2pdf <first-page>
   <second-page> --output <scratch.pdf>`. A PDF already in order can be filed directly.
3. File the source PDF on the current Project with `navigator site document upload`, selecting the kind that fits the
   source. Keep working copies in ignored Project storage or the session scratch directory. Run the transcript steps
   against that Document's Pointer.
4. Run `navigator site document transcribe <pointer>`. It uses local Poppler and Tesseract. Check flagged pages against
   the original. Fewer than three high-confidence words flags a page for review. Ask for a better original when
   recognition is poor and the page is illegible.
5. If machine OCR is unusable and the original is legible, write a UTF-8 transcription in ignored storage. Keep page
   breaks and mark unreadable text rather than guessing. Have the lawyer review the text against the source, then run
   `navigator site document transcribe <pointer> --text-file <reviewed.md>`. The stored proofread transcript names the
   source asset, version, and SHA-256. Report unclear passages with the matching page number.
