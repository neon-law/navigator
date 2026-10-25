# Social account onboarding

How an operator connects a [Brand](glossary/brand.md)'s own Facebook Page, Instagram professional account, Threads
profile, YouTube channel, LinkedIn Company Page, X account, and TikTok profile. Each connection needs proof. A [Social
Account](glossary/social-account.md) belongs to exactly one Brand; Brands never share one.

This page covers the existing Brands `vesta`, `cyber-injury-law`, and `neon`. All are already compiled keys in
[`views::brand::BrandKey`](../views/src/brand.rs). Onboarding never creates a Brand, and never creates a second registry
entry for the same identity.

## Save and connect

On the Brand edit page, save each intended provider account identity before selecting Connect. A saved [Social Account
Selection](glossary/social-account-selection.md) is not publishing authorization. It survives provider cancellation or
failure, and changing it does not retarget an existing connection or a scheduled Post.

The Meta section groups Facebook, Instagram, and Threads. Save the Facebook Page ID and its linked Instagram
professional account ID, then select **Connect/Reconnect Meta**. One Facebook Login requests the combined permissions
listed below. Navigator verifies the selected Page and its linked Instagram account and stores only those destinations.
A missing permission or mismatched link leaves the attempted pair unconnected. Usable existing connections are reused.
Threads uses its own Connect action, app configuration, token exchange, and grants.

The Connect POST finishes on a visible authorization handoff page before opening the provider. If automatic navigation
does not complete, use **Continue to provider** or **Cancel and return to Brand**. No cross-origin form permission is
required. Deployment app keys remain separate from Brand connections.

Provider references: [Meta’s Instagram API
collection](https://www.postman.com/meta/instagram/documentation/6yqw8pt/instagram-api) and [Meta’s Threads
authorization collection](https://www.postman.com/meta/threads/documentation/dht3nzz/threads-api).

## Current status

Inspect the target deployment before publishing:

```bash
navigator site blog preflight <brand> <slug> --providers facebook,instagram,tiktok --host <host>
```

The command reads authenticated status without uploading, storing a Post, or sending to providers. It exits nonzero with
setup blockers and the Brand's Navigator connection URL when prerequisites are absent. Only selected destinations are
checked. `ready_to_prepare` checks stored connections, permissions, reconnect requirements, OAuth configuration, and the
publisher URL's presence. It does not establish runtime KMS access, worker health, provider approval, media eligibility,
unrevoked consent, or delivery. Verify those through the checklist and actual publication receipts.

Nothing below is verified until the evidence record says so. Every row starts `pending`, and stays `pending` until an
operator completes the checklist for it. Committing this table does not mean a profile exists.

| Brand key | Facebook Page | Instagram | Threads | YouTube | LinkedIn | TikTok | X |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `vesta` | pending | pending | pending | pending | pending | pending | pending |
| `cyber-injury-law` | pending | pending | pending | pending | pending | pending | pending |
| `neon` | pending | pending | pending | pending | pending | pending | pending |

Provider app configuration is deployment state. Check the preflight's `connection_setup` status fields through
`navigator site blog status <brand> <slug> --host <host>` and configure the app ids and secrets in
[`deployment-secrets.md`](deployment-secrets.md#brand-social-accounts-and-runtime-kms) when absent. Creating the
provider accounts is an operator step. It needs the authorized, signed-in account of the person who will own them.
Navigator, an agent, or a test never creates a profile, picks a handle, or sends a test post.

## Record a manual publication on the website

After publishing through a provider website, create the Brand's website Post without sending to providers:

```bash
navigator site blog record /absolute/path/post.yaml --host <host>
```

The YAML supplies `brand`, `slug`, `title`, `description`, `body_markdown`, and the actual RFC 3339 `publish_at`. An
optional `video_asset` names an MP4 already uploaded below `img/<brand>/`. Use approved copy; the approved caption can
supply both the description and body. The command measures the video, needs no provider connection, and refuses future
times or an existing Post. On a retry, read `blog status` first. Use `blog handle` and `blog link` afterward to record
verified public social links. A provider post still under review is pending.

## Platform capabilities

| Provider | Connect | Account id | Publish | Inbound interactions |
| --- | --- | --- | --- | --- |
| Facebook | Meta OAuth | Page id | text or video | receiver |
| Instagram | Meta via Page | business id | portrait video | receiver |
| Threads | Threads OAuth | user id | text or video | receiver |
| YouTube | Google OAuth | channel id | video, any orientation | public comment replies |
| LinkedIn | LinkedIn OAuth | organization id | text, 3000 chars | not implemented |
| TikTok | Business OAuth | app open id | measured portrait MP4 | public text comments |
| X | X PKCE | user id | bounded text | not implemented |

Required publishing and account identity scopes verified before Navigator stores a connection:

- Facebook Page: `pages_show_list`, `pages_read_engagement`, `pages_manage_posts`, `pages_manage_engagement`.
- Instagram with Facebook Login: `pages_show_list`, `pages_read_engagement`, `instagram_basic`,
  `instagram_content_publish`, `instagram_manage_comments`, `instagram_manage_messages`.
- Threads: `threads_basic`, `threads_content_publish`, `threads_manage_replies`.
- YouTube: `https://www.googleapis.com/auth/youtube.force-ssl`,
  `https://www.googleapis.com/auth/youtube.upload`.
- LinkedIn: `r_organization_admin`, `w_organization_social`.
- TikTok Business publishing: `video.publish`, `video.list`, `user.info.basic`, `user.info.username`.
- X: `tweet.read`, `tweet.write`, `users.read`, `offline.access`.

A grant missing a required publishing or account identity permission fails in `portal::social_oauth` and records
nothing. TikTok comment grants are optional for connecting a publishing account: `comment.list` is checked when reading
comments, and both `comment.list` and `comment.list.manage` are checked before replying. Follow the [TikTok comment
setup](tiktok-publishing.md#public-comment-inbox) to update the configured authorization URL and reconnect with these
grants.

TikTok Business requires app approval, verified public media URLs, explicit automated-publishing consent, and separate
OAuth authorization per Brand. Setup and manual fallback are in the [TikTok guide](tiktok-publishing.md).

## Who does this

| Step | Who |
| --- | --- |
| Create or confirm the provider account | The person who will own it, signed in to the provider |
| Grant Page, Business, or profile roles | That owner, in the provider's own settings |
| Connect the account in Navigator | The Firm's Admin DRI or an Owner (`FirmCapability::ManageBrandContent`) |
| Review the evidence record | A second person who did not connect the account |

## Checklist, per Brand and platform

Do each row separately. Never reuse one Brand's Page, profile, or Business portfolio for the other Brand.

1. **Ownership.** Confirm in the provider's settings that the account belongs to the Brand's own business, not a
   personal profile and not the other Brand. For Instagram, confirm it is a professional (business) account linked to
   that Brand's Facebook Page. Record who owns it and who holds admin roles.
2. **Callback.** Confirm the provider app lists the deployment's callback,
   `https://<public host>/social/oauth/callback`, exactly. Navigator renders the same value from the deployment's public
   host; a mismatch fails at the provider, before Navigator sees a code.
3. **Connect.** Sign in to Navigator as the Admin DRI or an Owner, open `/app/admin/brands/<brand key>/edit`, and use
   **Connect** for the provider under **Social accounts**. Approve every requested permission. A partial grant is
   refused. Enter the exact provider account ID for this Brand before connecting. LinkedIn requires the numeric Company
   Page ID and verifies an approved `ADMINISTRATOR` role. Threads accepts its exact username without @ or account ID; X
   and TikTok accept the chosen username. Facebook, Instagram, and YouTube require the exact provider ID. Navigator
   connects only that selected identity, even when consent grants access to other Pages or channels. A historical
   authorization attempt without a selected target is rejected before exchanging its code.
4. **Account id.** On the same page, compare the connected account's name and id with the id shown in the provider's own
   settings. They must match. Supply the exact account ID so a Meta grant covering multiple Pages stores only this
   Brand's account. For Instagram, enter the Instagram business account ID, not the linked Facebook Page ID.
5. **Scopes.** A stored connection already proves every required scope was granted. Record the scope list from the
   capability table above as verified.
6. **Expiry.** Record the actual expiry. Threads refreshes within its last seven days when used; Meta Page tokens have
   no scheduled expiry but can be invalidated. YouTube access tokens refresh from the encrypted refresh token with the
   configured OAuth client. LinkedIn authorization expires and requires a new OAuth connection; no programmatic refresh
   is assumed. Record a reminder before a dated expiry.
7. **Reconnect check.** Disconnect and connect again once. The same account id must come back, and the Brand page
   must show it `connected`.
8. **Record the evidence** below, then have the second person review it.

TikTok Business follows the checklist above using its trailing-slash callback and the chosen profile username. The
manual fallback needs ownership and evidence only; its owner publishes by hand until API setup is verified.

## Reconnect when

- The Brand page shows the account as anything other than `connected`.
- The recorded expiry has passed.
- The owner changed the password, removed Navigator's app access, or changed the Page or profile roles.
- A publication fails with an authorization error.

Reconnect with the same checklist, from step 3. A reconnect that returns a different account id is a different account:
stop and find out why before you post anything.

## The evidence record

One record per Brand and platform. Keep it with the firm's operational records in Navigator-managed storage, not in this
repository, because a filled record carries production account ids. Never put screenshots, tokens, or passwords in it.

| Field | Meaning |
| --- | --- |
| `brand_key` | `vesta`, `cyber-injury-law`, or `neon` |
| `platform` | `facebook`, `instagram`, `threads`, `youtube`, `linkedin`, `tiktok`, or `x` |
| `status` | `pending`, `verified`, or `blocked` |
| `owner` | Who owns the provider account |
| `connected_by` | The Admin DRI or Owner who connected it |
| `provider_account_id` | Copied from Navigator's Brand page and checked against the provider |
| `scopes_verified` | `yes` once a connection is stored |
| `callback_verified` | `yes` or `no` |
| `expires_at` | The connection's recorded expiry |
| `reconnect_checked` | `yes` or `no` |
| `reviewed_by` | A second person |
| `notes` | Why a row is `blocked`, if it is |

A row is `verified` only when every field is filled and reviewed. A synthetic example of the record, as YAML, is
[`evidence.example.yaml`](../cli/tests/fixtures/social-account-onboarding/evidence.example.yaml);
`cli/tests/social_account_onboarding.rs` checks that it and this page name only registered Brand keys and synthetic
values.

## Provider consoles and production setup

Use the existing production publishing service for every Brand. One provider app may serve both Brands, but each Brand
has separate account identities, consent, selected destinations, encrypted tokens, and evidence records. No staging
provider app, token, or production secret is needed.

1. **Meta:** open [My Apps](https://developers.facebook.com/apps/) and create or inspect the production business app.
   Configure Facebook Login and Instagram API **with Facebook Login**, not Instagram Login. Record the public App ID as
   `NAVIGATOR_META_APP_ID`. Register `https://<production public host>/social/oauth/callback` as an exact valid OAuth
   redirect URI. Complete the console's business verification and App Review/Advanced Access requirements for the
   requested permissions and accounts outside app roles. Do not assume that switching the app to live grants scopes. In
   [Business Settings](https://business.facebook.com/settings/), verify each Brand's Page ownership, the operator's
   access, and the linked professional Instagram account. Record IDs in private operational evidence.
2. **Threads:** in [My Apps](https://developers.facebook.com/apps/), create or inspect an app with the Threads use case.
   Use its Threads App ID and secret, not the Facebook App ID. Register the same production callback and the three
   required Threads permissions. Invite and accept tester roles during development; complete review before using
   profiles outside those roles. Verify each Brand's own Threads profile during consent.
3. **LinkedIn:** open [Developer Apps](https://www.linkedin.com/developers/apps). Associate and verify the app with the
   owning business's LinkedIn Page through its Super admin. Request **Community Management API** access and the two
   scopes listed above. **Share on LinkedIn** and `w_member_social` alone publish as a person, not a Company Page.
   Configure the exact production callback under Auth. Confirm separately that the consenting member is an approved
   administrator of each Brand's Company Page. Development-tier quotas and organization restrictions apply until
   LinkedIn approves Standard tier. Enter each Brand's numeric Company Page ID in its own Navigator connection form.
4. **TikTok Business:** create or inspect the app in [Business My Apps](https://business-api.tiktok.com/portal/apps/).
   Obtain Accounts API access approval through the application linked in the [TikTok guide](tiktok-publishing.md).
   Configure the console-generated account authorization URL and exact trailing-slash callback, request the four
   permissions above, and verify the production media URL property. Each Brand then authorizes its own TikTok account
   separately and explicitly consents to automated publishing. Approval and ownership remain unverified until checked.
   Use the [manual handoff](tiktok-publishing.md#manual-handoff-while-setup-is-pending) while a requirement is pending;
   it needs no app credential. This Business Accounts API route has its own approval contract, distinct from the general
   Content Posting API's internal-utility restrictions.
5. **Secret entry:** enter app secrets from the provider console directly into the operator's own terminal, using the
   SOPS stdin method in [deployment secrets](deployment-secrets.md#secure-social-secret-entry). Never paste them into
   chat. Apply and restart only production after the provider app IDs, runtime KMS key, and all required secrets are
   present and the names-only dry run succeeds. Do not add placeholder secrets to make the gate green.
6. **Verify without posting:** check production readiness/version, callback registration, stored account IDs, scopes,
   ownership, expiry, disconnect/reconnect, and selected accounts on an unscheduled draft. Instagram eligibility must
   show measured portrait media and `vertical_video: true`. Connecting an account does not prove publishing works. Do
   not schedule or send a live test without explicit operator instruction. Review existing scheduled Posts before
   enabling credentials: production's reconciler can send already-due approved content.

LinkedIn text comes from the Post's description. Video, image upload, organization inbox, and programmatic token refresh
are not implemented. Its confirmed receipt is stored from `x-restli-id`; Navigator does not invent a permalink from the
receipt. The public link remains pending until a provider-confirmed URL is available. Expired or revoked consent must be
reconnected. The schema extension adds only an optional OAuth target and an allowed provider value, so historical rows
need no backfill.

Inbound subscriptions require additional provider-console verification. `/webhook/meta` currently verifies a single Meta
app secret; a separate Threads app cannot share that signature configuration. Threads publishing is supported, but
inbound delivery from a separate Threads app requires implementation. Facebook messaging subscriptions also require the
appropriate messaging and subscription permissions beyond the publishing grant; do not mark them operational from
publishing consent alone.

TikTok public comments use the separate signed `POST /webhook/tiktok` callback and an app-level `COMMENT` subscription.
Reconnect accounts with the additional comment grants; publishing permission alone does not enable the inbox. Follow the
[public comment setup](tiktok-publishing.md#public-comment-inbox) and verify live delivery after deployment.

## Official references

Checked 2026-10-09. Meta's developer site may rate-limit unauthenticated documentation readers; its own Postman
collections are additional primary examples. Recheck the console's actual granted permissions before activation.

- [Meta Pages API](https://developers.facebook.com/docs/pages-api/getting-started/)
- [Instagram](https://developers.facebook.com/docs/instagram-platform/instagram-api-with-facebook-login/)
- [Meta Facebook collection](https://www.postman.com/meta/facebook/collection/r56bjfd/facebook-api)
- [Meta source collections](https://github.com/fbsamples/messenger-platform-samples/tree/main/postman)
- [Threads authorization](https://www.postman.com/meta/threads/folder/34203612-e0373e84-de6b-46f1-b90d-3fea76ba6782)
- [LinkedIn app review](https://learn.microsoft.com/en-us/linkedin/marketing/community-management-app-review)
- [LinkedIn OAuth](https://learn.microsoft.com/en-us/linkedin/shared/authentication/authorization-code-flow)
- [LinkedIn introspection](https://learn.microsoft.com/en-us/linkedin/shared/authentication/token-introspection)
- [LinkedIn Posts API](https://learn.microsoft.com/en-us/linkedin/marketing/community-management/shares/posts-api)
- [LinkedIn API versioning](https://learn.microsoft.com/en-us/linkedin/marketing/versioning)
- [TikTok Business Accounts API](https://business-api.tiktok.com/portal/docs/accounts-api-overview/v1.3)
- [TikTok general Content Posting API guidelines](https://developers.tiktok.com/doc/content-sharing-guidelines)

## YouTube production connection

Enable YouTube Data API v3 in the [Google Cloud
console](https://console.cloud.google.com/apis/library/youtube.googleapis.com) and configure a web OAuth client with the
exact deployment `/social/oauth/callback` URI. Supply `NAVIGATOR_YOUTUBE_CLIENT_ID` and
`NAVIGATOR_YOUTUBE_CLIENT_SECRET` through the existing [secret entry
flow](deployment-secrets.md#secure-social-secret-entry). Verify the Google consent screen's publishing status, permitted
users, granted scopes, and quota before connecting the intended channel.

Save the exact Channel ID on the Brand edit page, then connect YouTube. Navigator requests offline access and consent,
reads the authenticated user's channels, and stores only the selected identity. The encrypted refresh token allows
access-token renewal. A signed-in YouTube website or a public channel handle does not authorize Navigator.

Run `blog preflight` with `--providers youtube`, compare the returned destination with the channel, then upload an MP4
under `img/<brand>/`. Navigator limits the asset to 64 MiB and accepts either orientation for YouTube. The Post title
becomes the video title; its YouTube caption override, or otherwise its description, becomes the video description. The
publisher requests public visibility and retains the upload receipt. Check actual processing and visibility in YouTube
before treating the stored watch link as public: the current uploader constructs that link from the receipt without a
visibility readback. Reconcile the existing upload after an uncertain result; do not create another Post to retry it.
Publishing requires approved content and destination, as with every provider.

For an upload made through YouTube Studio, verify its public watch URL and use the existing manual Post record and link
commands above. Recording a link never grants OAuth access or sends another video.

Official contracts: [Google web OAuth](https://developers.google.com/identity/protocols/oauth2/web-server), [channel
discovery](https://developers.google.com/youtube/v3/docs/channels/list), and [video
upload](https://developers.google.com/youtube/v3/docs/videos/insert).

## X production connection

X publishing uses the same Brand Social Accounts, Post destinations, Social Publications, and Restate service as Meta.
It supports text only, with a conservative 280-weight budget: each Unicode scalar counts twice and each possible URL
adds 23. Domain separators and protocol prefixes conservatively count as candidates. Video selection is refused before
sending. The general Post description supplies X text. Replies, threads, media upload, and Premium long posts are not
implemented.

Open the [X developer console](https://developer.x.com/) and configure an OAuth 2.0 confidential Web App or Automated
App with the exact production `/social/oauth/callback` URI and the scopes above. Save `NAVIGATOR_X_CLIENT_ID` and
`NAVIGATOR_X_CLIENT_SECRET` only through the production SOPS workflow, alongside the existing redirect URI and runtime
KMS configuration. API access and billing eligibility must be confirmed in the console; a configured app alone does not
authorize any Brand's account.

In the Brand's Social accounts form, enter its exact X username without `@` before granting consent. For Brand `neon`,
verify the intended `NeonLawUSA` account yourself; its public handle is not evidence of ownership or authorization.
Navigator reads `/2/users/me`, matches the chosen username case-insensitively, and stores only that account's numeric
ID. Every publish checks the token's identity again. The connection requires express automatic-publishing consent, with
disconnect as the opt-out. Select approved, purpose-specific Brand content; do not schedule substantially similar Posts
across accounts. Automated mentions, replies, and direct messages are not supported. Tokens and rotated refresh tokens
stay encrypted in the Brand account store, never in SOPS, staging, logs, or source. A known receipt is read back with
its author ID before linking the post; an ambiguous write remains unknown and is never automatically resent. No setup
step sends a live test post.

Official contracts: [OAuth and PKCE](https://docs.x.com/fundamentals/authentication/oauth-2-0/authorization-code),
[create Posts](https://docs.x.com/x-api/posts/create-post), and [weighted
characters](https://docs.x.com/fundamentals/counting-characters).
