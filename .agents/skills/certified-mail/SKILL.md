---
name: certified-mail
description: >
  Send a letter, notice, or packet by USPS Certified Mail through the Firm's mail vendor, then file the receipts.
  Trigger whenever a document must go "by certified mail", "by mail", or "with return receipt", or when the lawyer asks
  to print and mail something or to file a certified-mail receipt or green card. The Firm's vendor, its address, and how
  it bills are Firm facts in the Firm's own companion skill.
---

# Certified mail through the Firm's mail vendor

The Firm's mail vendor prints, mails, and scans the receipts back. Read the vendor's address and billing from the Firm's
companion skill; if the Firm has none, ask the lawyer. The service is USPS Certified Mail with Return Receipt, printed
single-sided, no staples.

## Steps

1. **Final, signed PDF only.** Every signature block is signed (DocuSign or a wet-signed scan) and the pages are
   read. Merge everything going in one envelope into a single PDF, in mailing order, named `YYYY.MM.DD_<name>.pdf`.
   Never mail an unsigned copy.
2. **Dry-run the job sheet**, read it, then run again without `--dry-run` to save it as an unsent Gmail draft.
   `navigator mail packet` writes the request in the standard form, attaches the PDFs, and refuses an unsigned or draft
   packet:

   ```bash
   navigator mail packet --vendor <vendor-address> \
     --recipient "<Name>" --recipient "<Street or P.O. Box>" --recipient "<City, ST ZIP>" \
     --return-receipt --mail-by YYYY-MM-DD --dry-run <signed.pdf>
   ```

   Repeat `--recipient` once per address line. The draft is filed on the matter once the lawyer sends it. One envelope
   per recipient: run it once for each. Check `navigator mail packet --help` first. If the CLI is unavailable, write the
   draft with the Gmail connector's `create_draft`, following the job sheet's form: the attached PDF(s) with page
   counts, each recipient's address block, "Print single-sided, no staples", "Send each USPS Certified Mail with Return
   Receipt", and "Scan the certified receipts back to me, and the green cards when they come back."
3. **The lawyer sends the draft.** Agents never send email.
4. **If the document is also going by email**, send it the same day as the mailing.
5. **File the proof.** The vendor replies that the job is done, and a separate USPS or receipt email carries the
   tracking numbers. Later, the green card or electronic return receipt arrives. For each one:
   - upload the receipt PDF into the matter's Costs folder with `navigator site document upload --kind receipt`,
     named `YYYY.MM.DD_certified-mail-receipt-<recipient>.pdf` (date mailed) and
     `YYYY.MM.DD_return-receipt-<recipient>.pdf` (date delivered);
   - save and file the email with `navigator mail save <gmail-url>` and `navigator mail parse`, so it lands in the
     matter's Communication log;
   - note the 20-digit article number and delivery date in the matter's handoff note, and track it at usps.com until
     it is delivered or returned unclaimed. An unclaimed return is proof too: file it.

## Notes

- Mailing to a P.O. box is fine; certified mail waits at the counter, and the recipient may not pick it up. Count
  notice periods from the mailing date the receipt shows, not the delivery date, unless the governing rule says
  otherwise.
- Keep client names out of the email subject beyond what the job sheet prints.
