# TikTok publishing

Navigator supports the TikTok API for Business Accounts route for approved, scheduled Brand video Posts. A connection
requires approved app permissions, provider OAuth authorization, and provider-verified account identity. No credential,
account connection, approval, or published post is established by this document.

## Choose the Business route

The [Accounts API overview](https://business-api.tiktok.com/portal/docs/accounts-api-overview/v1.3) permits businesses
managing their own accounts, including organic video or photo publishing. This differs from the general Content Posting
API, whose [guidelines](https://developers.tiktok.com/doc/content-sharing-guidelines) exclude internal upload utilities.
Navigator implements Business video publishing; it does not implement the general Direct Post API or photo publishing.

From March 20, 2026, a developer must complete the [Accounts API Access
Application](https://bytedance.sg.larkoffice.com/share/base/form/shrlgu4WEvtSXpEDLcCw56u4Rfc) before submitting a new
app or requesting the "TikTok Accounts" permission scope. Being a Business Account alone does not establish API access.
The owner completes that application and checks approval in [My Apps](https://business-api.tiktok.com/portal/apps/).

## Configure the approved app

1. Register the production callback `https://www.neonlaw.com/social/oauth/callback/`.
   TikTok requires HTTPS, the trailing slash, and no query, fragment, or port. Navigator provides that route beside the
   other providers' callback. The callback must exactly match the app's registered redirect. See
   [Authorization](https://business-api.tiktok.com/portal/docs?id=1738083939371009).
2. Configure `NAVIGATOR_TIKTOK_BUSINESS_APP_ID`, `NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET`, and
   `NAVIGATOR_TIKTOK_BUSINESS_AUTHORIZATION_URL` through the production SOPS workflow in [deployment
   secrets](deployment-secrets.md). The last value is the app console's generated **TikTok account holder authorization
   URL**, not an advertiser authorization URL. Navigator preserves its parameters, adds a one-time state, and disables
   automatic reauthorization. App credentials are shared by the publishing service, never by staging.
3. Verify ownership of the permanent public Asset URL property through the provider's
   [URL-property guide](https://business-api.tiktok.com/portal/docs?id=1769324038780930). Navigator checks the app’s
   verified properties through the [read-only property
   list](https://business-api.tiktok.com/portal/docs?id=1769325368603650) before sending. Verify
   `https://www.neonlaw.com/assets/` as a URL prefix, or the parent `neonlaw.com` domain. URL verification has been
   mandatory since November 16, 2023. Navigator refuses expiring signed URLs for this adapter. Its public Asset URLs
   have no expiry; TikTok recommends at least 30 minutes of availability.
4. For each Brand separately, open its admin page, save the exact TikTok username without `@`, then connect the account
   to authorize Navigator to publish approved scheduled Posts. Complete provider OAuth for that account. The returned
   identity must match the chosen username before Navigator stores any token.

The connection requires `video.publish`, `video.list`, `user.info.basic`, and `user.info.username`. Navigator verifies
permissions and both the app id and application-specific account id with the provider's [token
inspector](https://business-api.tiktok.com/portal/docs?id=1765927978092545), then reads the account's
[profile](https://business-api.tiktok.com/portal/docs?id=1762228399168514). The encrypted account token belongs to one
Brand. App-key configuration alone is not a verified account connection or permission grant.

Access tokens last one day. The refresh response rotates the encrypted refresh token and must preserve the account id
and required scopes. An expired access token can refresh; a revoked or expired refresh authorization needs consent
again. See [token exchange](https://business-api.tiktok.com/portal/docs?id=1833997638479041) and [token
renewal](https://business-api.tiktok.com/portal/docs/renew-a-short-term-access-token/v1.3).

## Prepare an approved Post

A TikTok destination uses the existing Post source and selected Social Account. Its description is the video caption. It
must declare `vertical_video: true` and an explicit `tiktok_ai_generated: true` or `false`. Missing disclosure blocks
publishing; it never silently becomes `false`. An edit changes the editorial digest and requires review of its schedule.

Before publishing, Navigator checks the uploaded video's measured portrait dimensions and owning Brand, and reads
[current account settings](https://business-api.tiktok.com/portal/docs?id=1816387951979521). The account must support
`PUBLIC_TO_EVERYONE`. The adapter measures MP4 duration and frame timing; unsupported timing blocks publishing.
Navigator currently limits this preflight to 64 MiB MP4 videos, narrower than TikTok's limits.

The [Publish API](https://business-api.tiktok.com/portal/docs/publish-a-public-video-post-to-an-owned-account/v1.3)
requires:

- A verified public video URL; MP4, MOV, or WebM at most 1 GB.
- At least 360 pixels on each side and 23 to 60 frames per second.
- Duration from 3 to 600 seconds, no longer than the account's current maximum.
- A caption of at most 2,200 UTF-16 units and 30 mentions. Navigator accepts no separate paid-partnership flow.
- Own-business promotional-content disclosure. Navigator sends `is_brand_organic: true` and `is_branded_content: false`,
  records the explicit AI-content choice, and disables comments, duet, and stitch.

TikTok imposes six video posts per minute and 15 per day per account. Provider refusal does not bypass the publication
receipt guard. Only production with real matters can contact the publishing provider. Simulated or other deployments
remain blocked, and this setup does not authorize a live test post.

## Confirm the result

The response’s `share_id` is a task receipt. Navigator records it as `provider_task_id`, then polls [publishing
status](https://business-api.tiktok.com/portal/docs?id=1816387106635778) through journaled Restate steps. Processing,
missing public ids, a draft, and an unknown outcome never count as a public post. An interrupted worker resumes polling
the retained task without another publish request. An ambiguous request without a receipt requires manual reconciliation
before another send.

Only `PUBLISH_COMPLETE` with a public post id confirms publication. Navigator then reads the owned account's [video
list](https://business-api.tiktok.com/portal/docs?id=1762228421622786) for its actual public `share_url`. It never
constructs a URL from a task id. Confirmed receipts and links appear in the same social dashboard and calendar as the
other providers; a delayed public URL remains pending and cannot trigger another send.

## Manual handoff while setup is pending

For `neon`, `vesta`, and `cyber-injury-law`, use a separate approved video and profile owner per Brand:

1. Confirm account ownership, approved content, measured portrait video, and the AI and commercial disclosures.
2. The owner downloads the approved Asset while signed in to Navigator and publishes in TikTok themselves.
3. Choose the intended privacy and interaction settings, review music rights, and disclose own-business promotion.
4. Copy the actual URL after TikTok confirms the post. Keep the pending or confirmed handoff record in
   Navigator-managed storage with the Brand, Post, Asset, approving person, profile owner, and publication time.

Never record a pending handoff as a confirmed Social Publication or manufacture a permalink. API publishing stays
unavailable until the production app is configured and that Brand's permission and identity checks succeed.

Official documentation verified October 9, 2026. Provider-console approval, ownership, consent, and URL-property
verification require the authorized operator; no local fake-provider test establishes those external facts.

## Public comment inbox

Navigator receives public text comments through `POST /webhook/tiktok` and sends approved public replies from
`/app/inbox`. Publishing consent alone does not enable this integration. Configure and verify these additional steps:

1. Enable TikTok Accounts > Account Comment permissions for the developer app. Reconnect each Brand with
   `comment.list` for reading and `comment.list.manage` for replies. Check the provider's [token inspector
   response](https://business-api.tiktok.com/portal/docs?id=1765927978092545). After approval of the expanded app
   permissions, replace `NAVIGATOR_TIKTOK_BUSINESS_AUTHORIZATION_URL` with the console's newly generated account holder
   authorization URL, then reconnect each Brand. Navigator preserves that URL's scopes and stores all inspector-verified
   grants. A URL pinned to publishing-only scopes cannot request the new grants. Publishing checks still apply; comment
   read and manage grants are additionally checked when the inbox uses them.
2. Register the HTTPS callback ending in `/webhook/tiktok` with the app-level [Webhook
   API](https://business-api.tiktok.com/portal/docs/create-or-update-a-tiktok-account-webhook-configuration/v1.3), using
   `event_type: COMMENT`. Inspect any existing `item_list`: a previously restricted subscription stays restricted when
   the field is omitted. Registration needs the app ID and secret; keep both in the approved secret workflow.
3. Confirm a real signed comment delivery reaches the intended Brand and an authorized operator sees it in the inbox.
   Fake-provider tests establish the adapter contract, not the live subscription or app approval.

The [subscription
workflow](https://business-api.tiktok.com/portal/docs/subscribe-to-tiktok-account-webhook-events-via-webhooks-api/v1.3)
documents signed HTTPS POST deliveries and a `200` acknowledgment; it does not document a Meta-style GET challenge.
Navigator verifies the [signature](https://business-api.tiktok.com/portal/docs/webhook-verification/v1.3), the app
identity, and a five-minute timestamp tolerance before looking up the connected account. It fetches the exact comment to
confirm that it is currently public and was written by an audience member. Brand-owned reply echoes are ignored. Comment
and video IDs retain integer precision, and repeated deliveries reuse the original Message.

Replies need both comment grants, the owned video ID and parent comment ID, contain at most 1,200 characters, and remain
public. Before sending, Navigator reads the parent again and refuses to post when the comment is missing, hidden,
belongs to another video, or cannot be verified. This protects historical transcript entries after moderation changes. A
confirmed provider reply ID establishes submission; ambiguous responses remain unknown and are never automatically
retried. Private TikTok messages, image replies, and historical comment backfill are not implemented. Hide, deletion,
and friends-only events are acknowledged without adding transcript text; existing Messages remain historical records of
what was received. Source changes must be deployed and the account grants and live subscription verified before this
integration is operational.
