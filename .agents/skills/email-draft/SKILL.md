---
name: email-draft
description: >
  How to write an email in the lawyer's Gmail: a client update, a reply in a thread, a cover email for a document, or
  any message written on the lawyer's behalf. Trigger whenever creating or updating a Gmail draft or reply. Covers the
  ending (no sign-off, because Gmail adds the signature), structure, threading, and attachments. The lawyer's own voice
  is a Firm fact in the Firm's companion skill.
---

# Writing email for the lawyer

Every message is an unsent draft. The lawyer reviews and sends it. Read the Firm's companion skill for the lawyer's
voice; without one, read a few recent messages in `in:sent` before drafting anything long.

## The default signature closes every email; no sign-off

- Every `create_draft` call passes `includeSignature: true`, so the draft carries the account's default signature.
- `update_draft` cannot add a signature. To change a draft that lacks one, create a new draft with
  `includeSignature: true` in the same thread, then delete the old draft.
- Do not end with "Thanks!", "Best,", "Cheers," or any other sign-off line. The body ends on its last substantive
  sentence, usually the ask, and the signature follows.
- Do not type the signature, paste it from an earlier message, or end the body with `--`; that prints it twice.
- **Never add or edit the signature by hand**, even when the tool leaves the draft without one. Apply it only through
  the Gmail connector's `includeSignature: true`. If that yields no signature, leave the draft unsigned and tell the
  lawyer, who adds it in Gmail.
- Prefer the Gmail connector (`create_draft`, `includeSignature: true`, `replyToMessageId`) over `navigator mail
  draft` for client mail when the CLI would flatten the signature to plain text. To change a draft, delete it and create
  a new one rather than updating it.

## Structure

- Short, plain sentences. Answer the reader's worry first, then the substance, then the asks.
- No legalese, no hedging stacks, no Markdown in a plain-text body.
- State the fact or the ask, cite a rule by number without explaining it, and never argue, recap history, or preview
  strategy to opposing counsel. Tell a vendor what to do.
- Topic by topic: a bold one- or two-word heading (pass `htmlBody` with `<b>`), then one short paragraph.
- Asks go last, as a numbered list under a short heading such as **Before you sign** or **What I need**.
- Short is not vague: a notice still carries what the rule requires, such as an ex parte's relief, date, time, and
  place.
- When the reader is a counterparty without a lawyer, say plainly whom the lawyer represents.

## Cover email for a document

- One line saying what is attached and what it replaces.
- One bold heading per point the client raised, answered in a sentence or two, in the client's own order.
- Name any assumption the document makes (governing law, conversion, dates) so the client can confirm it.
- End with the numbered asks.

## Threading

Reply inside the existing thread: pass the latest message's id as `replyToMessageId` and keep the original recipients
and cc. When a draft already exists in the thread, replace it rather than creating a second one, and keep the quoted
history. Create a draft and never send.

## Attachments

- The Gmail connector takes attachments only as inline base64 typed into the call, which cannot be checked against
  the file on disk. Do not attach a contract or other client document that way.
- `navigator mail draft --attach <path>` reads the file from disk; use it when it runs. It refuses without a stored
  signature or `NAVIGATOR_GMAIL_SIGNATURE`, and its body is plain text only.
- Otherwise, leave the draft without the attachment: copy the file to `~/Downloads` under a client-facing name
  (`<Client> - <Document> - <Counterparty>.docx`), confirm it matches the rendered source byte for byte, open it, and
  tell the lawyer to drag it in.
- `update_draft` drops every attachment that the call does not resend. Updating the body of a draft that has an
  attachment removes the attachment.
