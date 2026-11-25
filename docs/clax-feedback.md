# In-app problem reports (CLax)

Lawyer-tier sessions on `/app` pages get a Report button. A report carries a screenshot, recent browser errors, and the
page path into a CLax report queue, where an agent proposes a fix and the reporter confirms it.

## How it fits

- `portal/src/clax_feedback.rs` mints a reporter token at `POST /app/api/clax/token`. The token's reporter ID is the
  person's `persons.id`, never an email address. The route is lawyer-tier only in the policy and in the handler.
- The overlay is vendored at `server/public/vendor/clax/overlay.js` (pinned in `VENDOR.toml`, LGPL-2.1-only) and
  started by `server/public/js/clax-feedback.js`. `script-src` stays `'self'`. Only `connect-src` names the CLax
  instance, and only on a response that carries the overlay.
- The service grant's key never leaves the server.

## Configuration

Reports are off unless `CLAX_FEEDBACK_ENABLED=true` and the rest are set. An unusable key also turns them off.

| Variable | Value |
| --- | --- |
| `CLAX_FEEDBACK_ENABLED` | `true` turns reports on; anything else leaves them off. |
| `CLAX_QUEUE` | The queue ID. |
| `CLAX_QUEUE_TOKEN` | The service grant's `cxq_` token (secret). |
| `CLAX_QUEUE_KEY` | The grant's base64url PKCS#8 Ed25519 key (secret). |
| `CLAX_PAGE_ORIGIN` | This deployment's exact origin, which the queue lists. |
| `CLAX_INSTANCE` | The CLax origin; `https://clax.dev` by default. |

## License

The overlay is LGPL-2.1-only; its license is `server/public/vendor/clax/LICENSE`, and the upstream source is the CLax
repository's `web/overlay`. Everything else here is `MIT OR Apache-2.0`. With the flag off the portal answers 404 for
the overlay and its loader, so nothing of it is served. To use a modified overlay, replace the vendored file and update
its `sha256` in `VENDOR.toml`.

## Client data

Screenshots leave our infrastructure for the CLax instance. Keep the overlay off pages that show client matter content
until the queue is self-hosted or the capture can be redacted. Refresh the vendored overlay by fetching
`<instance>/_clax/overlay.js` and updating its `sha256` in `VENDOR.toml`.
