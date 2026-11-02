# Mail CLI

`navigator mail setup|login|search|read|draft|file|parse|save|attachments` is a local Gmail workflow. Search and read
use Gmail's read endpoints; draft creates or updates an unsent Gmail draft. The CLI has no message-send, reply-send, or
forward operation. Filing records each message as a Project-scoped Communication. `file` refuses a thread that carries
attachments before it writes anything; `read` prints their names and marks them as not downloaded. `parse` files one
message the same way — a Gmail web-URL message, a local .eml, or a local file through the document-upload lane — and
`save` writes a message's .eml copy under the checkout's git-ignored `mail/` folder. `attachments` writes a message's
attachments into a directory. None of them touches Gmail beyond reads.

The CLI reads the refresh token from the macOS Keychain item whose service is
`com.neonlaw.navigator.gmail.refresh-token` and whose account is the configured Gmail address. It exchanges that token
at Google's token endpoint using the configured OAuth client id, plus the client secret when one is stored. The macOS
Security framework reads and writes both items. `navigator mail login` mints the token. Deployment requires a
signed-app/Keychain ACL threat model and a managed-host test.

## Draft-only setup

Save the Gmail address and OAuth client id once:

```bash
navigator mail setup --account lawyer@example.test --client-id <oauth-client-id>
```

Both flags are required together. The command saves these settings, then checks the draft-only path. Settings are
written atomically with owner-only permissions to `~/.navigator.mail.json`. The file contains no refresh token. The
location follows the credential-path overrides: `NAVIGATOR_CREDENTIALS_FILE` first, then `NAVIGATOR_CONFIG_DIR`, with
the credential file's extension replaced by `.mail.json`. Nonblank `NAVIGATOR_GMAIL_ACCOUNT` and
`NAVIGATOR_GMAIL_CLIENT_ID` override the saved values independently.

`navigator mail setup` checks the saved settings and environment overrides and prints the fix for each gap. It reports
`ok` or `MISSING` for the account, client id, and Keychain refresh token, and fails until all three are present. It
reports the client secret as `ok` or `optional`; the secret never blocks readiness. Saved settings remain available even
when the token check fails. Output contains fixed text only, never the account, client id, secret, or token. When the
token is missing, it points to `navigator mail login`.

## Authorize Gmail

`navigator mail login` runs Google's installed-app OAuth flow and stores the refresh token:

```bash
navigator mail login
navigator mail login --client-secret-file ~/Downloads/client_secret_<id>.json
```

- **Settings.** It uses the account and client id saved by `navigator mail setup`, or their environment overrides.
- **Client secret.** Google's Desktop-app clients require a client secret at the token endpoint. The CLI reads it from
  the Keychain item whose service is `com.neonlaw.navigator.gmail.client-secret` and whose account is the client id.
  `--client-secret-file` reads `installed.client_secret` from Google's downloaded client JSON, refuses a file for a
  different client id, and stores the secret. With neither, the CLI prompts on the terminal with echo off and stores
  what you type. The secret is never printed or logged.
- **Browser.** A one-shot listener binds `127.0.0.1` on a random port as the redirect. The CLI opens the Google
  consent page with `open` and prints the URL too. The request uses PKCE (S256), a random `state` the redirect must
  echo, `access_type=offline`, `prompt=consent`, and `login_hint` set to the configured account. A `state` mismatch
  refuses the code. The listener waits five minutes.
- **Scopes.** The request names exactly `gmail.readonly` and `gmail.compose`. After the exchange the CLI checks the
  granted scopes. A token missing either scope, or carrying any other, is revoked at Google and not stored. A token for
  a different Gmail account than the configured one is also revoked.
- **Storage.** The refresh token goes into the existing Keychain item, replacing any earlier token.

Every later refresh checks the granted scopes again. A stored token carrying any other scope is deleted from the
Keychain, and the command fails until `navigator mail login` runs again.

Gmail offers no narrower scope that creates drafts, and `gmail.compose` can send at Google's level; the guard is that
this CLI has no send verb and calls no send endpoint (a test pins the endpoint list). `inbox process --apply` archives
through Gmail's modify endpoint. That endpoint needs `gmail.modify`, so a token from `navigator mail login` cannot
archive.

Search adds the current Project client's email to the Gmail query when `navigator.yaml` provides `client_dri`. This is a
search convenience, not an authorization boundary. Filing compares the message participants with that contact and warns
when they do not match. The authenticated Navigator API still enforces Project participation. The message idempotency
key combines the normalized Gmail profile address and Gmail message id; reusing it in a different Project is rejected.

For drafting, provide only the substantive message body. The CLI removes the `--` signature delimiter and an exact
trailing signature from `NAVIGATOR_GMAIL_SIGNATURE`. It preserves common sign-offs and cannot identify arbitrary
undelimited prose as a signature, so this cleanup is not a signature-detection guarantee; an attorney must review the
complete draft in Gmail before any separate sending action.

## Recipients

`--to`, `--cc`, and `--bcc` each repeat or take a comma-separated list. For two parties, pass `--to a@example.test --to
b@example.test --cc c@example.test`. An update with `--draft-id` replaces the whole draft, so pass every recipient
again. When Gmail is not set up, the error lists every missing setting at once.

## Attachments

`navigator mail draft --attach <path>` adds a file to the draft; repeat the flag for more files. It works with `--to`,
`--subject`, `--body-file`, `--thread`, and `--draft-id`. The CLI reads each file's bytes from disk, so they never pass
through an agent's context.

```bash
navigator mail draft --to client@example.test --subject "Engagement letter" --body-file body.txt \
  --attach letter.pdf --attach exhibit-a.png
```

- **File name.** Each attachment keeps its file name. A name with a line break or other control character is refused.
- **MIME type.** The extension sets the type, so `.pdf` is `application/pdf`. An unknown extension is sent as
  `application/octet-stream`.
- **Size.** Gmail's limit is 25 MB (25,000,000 bytes) for the encoded message. The CLI checks each file and the whole
  draft against it from file sizes, before any network call, and names the oversized file when one alone is too large.
- **Replies.** With `--thread`, the draft keeps the thread ID and the `In-Reply-To` and `References` headers.
- **Updates replace.** An update with `--draft-id` replaces the whole draft. The `--attach` list given to that update
  becomes the draft's attachments. An update without `--attach` leaves the draft with none.

The draft stays unsent; there is still no send operation.

## Parse one message into the log

`navigator mail parse <reference>` files one message as a Project-scoped Communication — the same record write `file`
makes for a whole thread, but for a single message the lawyer points at:

- **Gmail web URL.** Copy the message's URL from the browser (`https://mail.google.com/mail/u/0/#inbox/<id>`; the legacy
  `?th=<id>` form works too). The CLI extracts the message id, reads it with `format=full`, and files it. A bare Gmail
  message id is accepted as well.
- **Local .eml.** A path to a saved .eml files from its own bytes: headers, Date, and body come from the message, and
  the Message-ID header (or a content digest when the message has none) scopes the source reference so re-filing the
  same .eml dedupes.
- **PDF or any other file.** Any other local path rides the existing `navigator document upload` lane as a document,
  defaulting to kind `letter` (`--kind` overrides; the asset-lane kinds are accepted), visibility `internal`.

Like `file`, `parse` takes `--project` and `--host` (each defaulting to `navigator.yaml`), warns when the message's
participants do not match the Project's client contact, and refuses a message that carries attachments before it writes
anything.

## Save a message's .eml copy

`navigator mail save <gmail-web-url-or-id>` fetches the message with `format=raw` — the complete RFC 822 copy — and
writes it into the Project checkout's git-ignored `mail/` folder:

```bash
navigator mail save "https://mail.google.com/mail/u/0/#inbox/18fa3bc2d4e5f6a7"
# Saved mail/2026.10.30_Retainer_questions.eml (message 18fa3bc2d4e5f6a7)
```

- **Default path.** `mail/` in the current directory; `--dir` moves it.
- **Filename convention.** The default name is `YYYY.MM.DD_<subject>.eml`, the send date first, the subject as a
  path-safe slug. `--name` must follow the same convention — a filename that drops the send-date prefix or loses the
  `.eml` extension is refused.
- **Never clobbers.** A re-run of the same message is idempotent (already saved); an existing file with different
  contents is refused rather than overwritten.

`mail/` is git-ignored — saved mail is a matter fact, never Git — and the Project gate refuses it. Saving is a read of
Gmail and a write to disk; it never sends, and it does not archive or label the message in Gmail.

## Save a message's attachments

`navigator mail attachments <gmail-web-url-or-id> --dir <dir>` fetches the message with `format=raw` and writes each
attachment's decoded bytes into `<dir>`, one file per attachment:

```bash
navigator mail attachments "https://mail.google.com/mail/u/0/#inbox/18fa3bc2d4e5f6a7" --dir mail/18fa3bc2d4e5f6a7
# Engagement Letter.pdf  48213 bytes  sha256:<64 hex>  written
```

- **Output.** One line per file: the name, the byte count, the sha256, and `written` or `already present`. The bytes go
  only to disk, never to stdout.
- **Filenames.** Each file keeps its attachment's filename, with surrounding whitespace trimmed. A name with a path
  separator or a control character, or one that is empty or only dots, refuses the run before anything is written. Pass
  `--only` to pick the other attachments. An unnamed part becomes `attachment-<n>.<ext>`, with the extension from its
  MIME type. Two attachments with the same name and identical bytes produce one file. With different bytes, the later
  one gets `-2`, `-3`, and so on before the extension.
- **Never clobbers.** An existing file with identical bytes is left alone, so a re-run is idempotent. An existing file
  with different bytes refuses the whole run before anything is written.
- **`--only <glob>`.** Writes only filenames matching the glob. `*` matches any run of characters and `?` matches one.
  Matching ignores ASCII case, so `--only '*.pdf'` matches `Letter.PDF`.
- **Inline images.** Images marked inline, such as a signature logo or a pasted screenshot, are skipped unless you pass
  `--include-inline`. Other inline parts, such as a PDF that Apple Mail marks inline, are written. A forwarded message
  is written as one `.eml` file.

Point `--dir` at a git-ignored folder such as `mail/`. Legal files never belong in Git. This verb uploads nothing. To
file an attachment, use `navigator site document upload`.
