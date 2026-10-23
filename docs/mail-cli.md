# Mail CLI

`navigator mail search|read|draft|file` is a local Gmail workflow. Search and read use Gmail's read endpoints; draft
creates or updates an unsent Gmail draft. The CLI has no message-send, reply-send, or forward operation. Filing records
each message as a Project-scoped Communication. `file` refuses a thread containing attachments before it writes
anything; `read` prints their names and marks them as not downloaded.

The CLI reads the refresh token from the macOS Keychain item whose service is
`com.neonlaw.navigator.gmail.refresh-token` and whose account is `NAVIGATOR_GMAIL_ACCOUNT`. It exchanges that token at
Google's token endpoint using `NAVIGATOR_GMAIL_CLIENT_ID`. This implementation does not enroll an account or provision
the Keychain item. This code uses the macOS Security framework Keychain API. It does not prove that a shell agent cannot
invoke another process that can use the same account's authorized helper. Deployment still requires a signed-app/
keychain ACL threat model and a managed-host test.

Search adds the current Project client's email to the Gmail query when `navigator.yaml` provides `client_dri`. This is a
search convenience, not an authorization boundary. Filing compares the message participants with that contact and warns
when they do not match. The authenticated Navigator API still enforces Project participation. The message idempotency
key combines the normalized Gmail profile address and Gmail message id; reusing it in a different Project is rejected.

For drafting, provide only the substantive message body. The CLI removes the `--` signature delimiter and an exact
trailing signature from `NAVIGATOR_GMAIL_SIGNATURE`. It preserves common sign-offs and cannot identify arbitrary
undelimited prose as a signature, so this cleanup is not a signature-detection guarantee; an attorney must review the
complete draft in Gmail before any separate sending action.

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
