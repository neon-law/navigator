---
name: message-processing
description: File an original email and verify its Project document pointer.
---

# File a message

1. Confirm the Project and message. Save the original with `navigator mail save <message-id>` in the Project checkout.
2. Run `navigator mail parse mail/<dated-name>.eml`. For a saved copy without Gmail thread metadata, supply a verified
   `--gmail-thread-id` when available. The Communication links to the original document.
3. Review the dated pointer under `blobs/mail/`. Check the evidence source and keep visibility internal unless the
   lawyer explicitly chooses to share the complete original with the client.
4. Run `navigator project gate` and include the pointer in the Project pull request. Keep the raw message under the
   ignored `mail/` directory. Other authorized checkouts retrieve the bytes with `navigator project sync` under
   `documents/Mail (Correspondence)/`.

Prepare any outgoing email as a draft for lawyer review.

For Project terms, follow the **Find the term first** section of `AGENTS.md`.
