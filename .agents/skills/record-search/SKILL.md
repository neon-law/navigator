---
name: record-search
description: >
  Answer "where in the record does it say X" with exact matches and an explicit list of what was and was not searched.
  Trigger whenever asked to find, cite, or confirm a fact, phrase, name, or event in a Project's documents, filings,
  correspondence, transcripts, or notes, and before stating that the record does or does not contain something. Kept
  identical, byte for byte, in every Project repository's `.agents/skills/`; this canonical copy lives in Navigator's
  own `.agents/skills/`.
---

# Record search

The answer to "where does the record say X" is a quotation with a location, never a summary. Cite every match by path
and line or page, and quote its text exactly. Limit every absence claim to the files the search covered.

## Find the term first

Before naming a new record class, run `navigator glossary list` and then `navigator glossary show "<Term>"` for the
closest existing term.

## Name the record

Run `navigator project sync` first so the checkout holds every document visible to you. Then decide which record the
question means:

- Court record: docket entries, filed papers, minute orders, rulings, and docket exports.
- Correspondence: saved email and mail exports.
- Client statements: call transcripts, intake notes, and client-forwarded mail.
- Work product: memos, drafts, and notes.

Use `--record court` when the question concerns the court record. A fact found only in correspondence or a client
statement is not in the court record. Discovery papers such as subpoenas are ordinarily not filed with the court, so
their absence from the court record proves nothing about whether they exist.

## Search the extracted text

Use the deterministic Project search command. Quote the regex so the shell does not reinterpret it:

```bash
navigator project search '<pattern>' --ignore-case --context 2 --json
```

Search exact words first, then alternations for known spellings and aliases. The command searches ignored files under
`documents/` and `notes/`, extracts supported document text, and reports files that could not be searched. Read its
coverage block before making an absence claim. A search with no hits means only "no match in the N files searched."

## Report

For each hit, give its record class, path and line or page, message date and sender when available, and exact matched
text. Extraction can change line layout, so line numbers locate extracted text, not source bytes or original-page lines.
Report file counts and any unsearched files or pages.

Treat OCR text as a lead to verify against the image. Keep attribution clear: a party's assertion, opposing counsel's
assertion, and a court's finding are different.
