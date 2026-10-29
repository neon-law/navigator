---
name: analyze-memo
description: >
  Review a finished memo or letter for legal substance, consistency, tone, rendered Word layout, and internal filing.
  Report findings first and apply corrections only with the lawyer's go-ahead. Use for a review pass over existing work
  product; use legal-writing when drafting it and litigation-filing-review for a court filing package.
---

# Analyze a memo or letter

Before naming a record or UI concept, follow **Find the term first** in `AGENTS.md`.

Read the exact document, its previous version, and the saved sources it relies on. Follow [[legal-writing]] for source
proof and [[record-search]] when checking the matter record. Keep the review and document bytes in the Project's
approved stores. The Project repository carries reusable Drafts and Pointers, never completed work product.

Review in this order. Report a missing source or unavailable previous version as a gap rather than assuming it agrees.

## 1. Visibility

Every uploaded revision is internal. A client release requires the lawyer's explicit per-document decision; an earlier
client-visible revision does not authorize releasing the correction. Keep that release decision separate from approval
to correct the document. Verify the stored revision and its visibility after an authorized upload.

## 2. Substance

- Check engagement and fee terms against the governing professional rules, including required disclosures and consent.
- Market-check every fee, demand, and monetary range against current, comparable sources. Explain comparability and
  uncertainty; identify an unsupported figure rather than inventing a benchmark.
- Verify the forum and governing jurisdiction. For every federal claim, check exemptions, covered parties, and coverage
  thresholds as well as the elements and remedy.
- Verify every authority against its saved original through [[case-downloader]], including treatment and pinpoint
  support. Explain what each citation establishes and why it matters to the conclusion.
- Define every acronym and product name on first use in words the intended reader understands.

## 3. Consistency

For each proposed correction, list every related document carrying the same analysis and the passage that must change.
Use the Project's record to establish that list and state what was searched. After the lawyer authorizes corrections,
update every affected document or name the outstanding dependency; do not silently leave conflicting advice in place.

## 4. Tone and form

For letters, use formal verbs and a formal close. Give an identifier once. Honor the stated page limit. Preserve the
document's intended audience, voice, and scope.

## 5. Rendered Word document

Compare the firm letterhead with the previous version. Render the Word document and read every page, checking headers,
footers, page breaks, numbering, spacing, and the page limit. Inspect the rendered document again after an authorized
correction. If rendering or a page cannot be inspected, report that limitation before describing the review as complete.

## 6. Filing

After correction approval, save the final document under `documents/Work Product/<Subfolder>/YYYY.MM.DD_<name>`. Choose
the subfolder and stable name from the matter's existing organization. Follow [[legal-writing]] for the upload or
`navigator project sync`; explicitly keep visibility internal, verify the stored revision, and review and commit only
its Pointer. Keep document bytes out of Git. A correction approval alone never authorizes a client release.

## 7. Report and authorization

Lead with findings, ordered by consequence. For each, name the passage, supporting source or gap, proposed correction,
and other documents affected. State what was checked and what remains unverified, including the rendered pages.

A review request authorizes findings and proposed corrections. Apply corrections only on the lawyer's go-ahead; preserve
that boundary even when the issue appears obvious. Report corrected documents and verification results after the
approved work is complete. Sending, filing with a tribunal, and client release require their own authorization.
