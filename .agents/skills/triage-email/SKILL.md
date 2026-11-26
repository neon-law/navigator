---
name: triage-email
description: Triage Gmail and Microsoft 365 inboxes one email at a time from the Navigator workspace.
---

# Triage email

Run `/triage-email` from the top-level workspace that holds the Project checkouts. Use the configured root throughout.
Discover Projects from immediate child checkouts with a `navigator.yaml`.

Connect to the user's Gmail and Microsoft 365 mailboxes. Use the Gmail MCP connector for all Gmail mailbox operations.
Use the Outlook Email connector for Microsoft 365, or the user's signed-in Outlook on the web when needed. Identify
which accounts are accessible. If one needs sign-in, name it and offer to continue with the available inbox. Page
through the inboxes, including read mail. Expand conversations into individual messages and track each by account and
provider message ID so pagination and retries keep the place.

1. **Ask for the Project.** Show the account, sender, date and subject of one message and ask which Project should
   receive it. Wait for the lawyer's choice, even when the thread already has filed messages. Offer to skip or stop. A
   skip leaves the message in its inbox and moves to the next one.
2. **Save the original.** In the selected Project's private checkout, confirm the code and host in `navigator.yaml`.
   For Gmail, use `navigator mail save <message-id>` for the Navigator record. For Microsoft 365, export the individual
   message as an original `.eml` with attachments. Outlook on the web offers **Download as EML** on the expanded
   message's menu. Save it under the checkout's ignored `mail/` directory. Verify the headers against the selected
   message, including its Internet Message-ID when available.
3. **Verify the filing.** Run `navigator mail parse mail/<file>.eml --project <code> --host <host>`. Check the
   Communication result and the internal document pointer under `blobs/mail/`. Follow the Project's review procedure to
   land the pointer. Keep the raw mail and attachments out of Git. Reuse a verified filing for the same message and
   Project. If filing is refused or incomplete, report where it stopped and resume there.
4. **Bump this Project.** Resolve the current published stable Navigator release with
   `gh release view --repo neon-law/navigator --json tagName,url`. Read its release notes. Apply the
   `navigator-version-bump` procedure to the selected checkout alone, honoring its configured source and mirrors. Verify
   the release CLI, update the Project's pins and managed skills, apply required migrations, and run the Project gates.
   Reuse an existing release review and follow it through merge and required deployment. If the authoritative Project is
   already current, verify the pins and record that outcome. A pending or failed bump pauses this message.
5. **Ask what to do next.** Once filing and the release are verified, ask which action the lawyer wants for this
   message. Offer choices that fit, such as an unsent reply draft, recording a deadline, archiving, or simply leaving
   the saved message in the inbox. Carry out only the chosen action. Every composed email stays an unsent draft for the
   lawyer to send personally. If an action affects a whole conversation, confirm that scope first. Verify the result
   before presenting the next message.

On resuming, check the existing filing, release review and action outcome before repeating a write. Summarize filed,
skipped and unfinished messages in the private session, including any inaccessible account.
