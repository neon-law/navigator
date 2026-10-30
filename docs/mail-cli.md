# Mail CLI

`navigator mail setup|search|read|draft|file|parse` is a local Gmail workflow. Search and read use Gmail's read
endpoints; draft creates or updates an unsent Gmail draft. The CLI has no message-send, reply-send, or forward
operation. Filing records each message as a Project-scoped Communication. `file` refuses a thread that carries
attachments before it writes anything; `read` prints their names and marks them as not downloaded. `parse` files one
message the same way — a Gmail web-URL message, a local .eml, or a local file through the document-upload lane.

The CLI reads the refresh token from the macOS Keychain item whose service is
`com.neonlaw.navigator.gmail.refresh-token` and whose account is the configured Gmail address. It exchanges that token
at Google's token endpoint using the configured OAuth client id. The macOS Security framework reads the token. This
implementation does not enroll an account or provision the Keychain item. Deployment requires a signed-app/Keychain ACL
threat model and a managed-host test.

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
`ok` or `MISSING` for the account, client id, and Keychain refresh token, and fails until all three are present. Saved
settings remain available even when the token check fails. Output contains fixed text only, never the account, client
id, or token. To store the token, run the following with your configured Gmail address. The command prompts for the
token so it stays out of shell history:

```bash
security add-generic-password -s com.neonlaw.navigator.gmail.refresh-token -a "<gmail-address>" -w
```

Authorize the refresh token for `gmail.readonly` and `gmail.compose` only. Gmail offers no narrower scope that creates
drafts, and `gmail.compose` can send at Google's level; the guard is that this CLI has no send verb and calls no send
endpoint (a test pins the endpoint list). The CLI never requests scopes itself; the token carries what you authorized.

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
