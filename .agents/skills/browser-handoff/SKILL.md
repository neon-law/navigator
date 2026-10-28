---
name: browser-handoff
description: >
  Borrow the attorney's own signed-in browser to download a document from a third-party site, such as a court-stamped
  copy from an e-filing provider, then file it in Navigator. Trigger whenever a matter needs a file that sits behind the
  attorney's login on a site Navigator has no integration for. Never handle the attorney's credentials. Kept identical,
  byte for byte, in every Project repository's `.agents/skills/`; this canonical copy lives in Navigator's own
  `.agents/skills/`.
---

# Browser handoff

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`, then read
the term itself:

```bash
navigator glossary show "Document"
```

Some files a matter needs sit behind the attorney's login on a third-party site, such as the stamped copy of a filing on
an e-filing provider. Navigator stores no credentials for those sites. The agent works inside a browser the attorney has
already signed in to, downloads the file, and files it. The attorney's session is the only authority it uses.

## 1. Get the attorney's browser

Use one of these, in order:

1. **Claude in Chrome, connected inside the session.** The attorney runs `/chrome` in Claude Code and confirms the
   extension shows as connected. The agent then has the `mcp__claude-in-chrome__*` tools and works in the attorney's
   real Chrome, with their sign-ins.
2. **Claude in Chrome, from the start.** The attorney starts the session with `claude --chrome`, or `claude --chrome
   --continue` to resume this conversation. Tools register when a session connects to the extension. Installing the
   extension partway through a session does not add them.
3. **Fallback: the Claude desktop app's browser pane.** It keeps its own sign-ins, separate from Chrome, so the attorney
   signs in there once, by hand, before the agent opens the site.

Open a new tab for the work. Leave the attorney's other tabs alone.

## 2. When the browser tools are missing

If no browser tools are listed, or a tool search for them returns nothing, stop and ask the attorney to reconnect: run
`/chrome` and confirm the extension is connected, or restart with `claude --chrome --continue`. If neither works, offer
the desktop app's browser pane, or let the attorney download the file by hand and give you its path.

Never work around missing tools:

- Never read browser cookies, a browser profile, or a saved-password store.
- Never script a login with the attorney's password, and never ask for it.
- Never store a credential, token, or session value in the repository, a file, a note, or a command line.

If the site asks for a login, a second factor, or a CAPTCHA, hand the tab to the attorney and wait.

## 3. Stay read-only

The agent only navigates, reads, and downloads. On the third-party site it never clicks submit, pay, resubmit, or
withdraw, and never accepts terms or edits a filing, a payment method, or an account setting. If the file can only be
reached past one of those controls, stop and ask the attorney to take that step.

## 4. File the download

The file lands in the browser's download folder, outside the repository. Never commit it. File it from the matching
Project repository so the pointer is written there:

```bash
navigator site document upload --project <code> --file <download> --kind filing --slug <name>.pdf
```

Use `--description` to record which site the copy came from and when it was downloaded. Then read the pointer back and
confirm the new revision is the operative row, marked `*`:

```bash
navigator site document log blobs/<name>.pdf.yaml
```

That table, not the upload's exit code, is the proof the file landed. Commit only the Pointer under `blobs/`.
