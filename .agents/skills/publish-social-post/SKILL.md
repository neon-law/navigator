---
name: publish-social-post
description: >-
  Publish approved Brand Posts through Navigator's CLI, inspect delivery receipts, or link posts already published on
  social providers.
---

# Publish a social Post

Use the stored site login and the existing durable Social Publisher. Read [Post](../../../docs/glossary/post.md),
[Social Publication](../../../docs/glossary/social-publication.md), and [account
onboarding](../../../docs/social-account-onboarding.md). For TikTok, read its [publishing
requirements](../../../docs/tiktok-publishing.md).

Publish only when the user authorizes the content and destinations. If the user already published manually, record the
public links. A profile URL or Business Suite insights URL is not a post permalink.

## Publish

### Choose the environment and operation first

- **Staging stores Post records and uploaded media.** Use `site blog record` with an actual publication time no
  later than now for a website preview. Do not queue social destinations there. A staging Post is independent of the
  production Post; copying media and records does not copy social authorization.
- **Production sends to approved social destinations.** Run preflight, verify the intended accounts, then use
  `site blog publish` or the production calendar. Use `record` in production only for an approved website-only Post or
  an existing manual publication. It marks the Post public immediately, and `publish` refuses that Post afterward. Do
  not record first when the intended next step is social scheduling.
- **A website Post is not a provider receipt.** A published website record proves no Instagram, TikTok, or other
  social delivery. Report each destination from its confirmed Social Publication receipt.

The production calendar at `/app/social/calendar` schedules one Post per submission. To queue four or five approved
Posts, repeat the form with distinct slugs, uploaded video keys, verified destinations, and explicit times with UTC
offsets. Staging previews can use matching slugs because the deployments have separate records. Keep the production
Posts unpublished until scheduling; do not duplicate a public Post under another slug to bypass the resend guard.

The current calendar and CLI publish API put the shared caption in `description`, whose store limit is 200 characters.
`body_markdown` is website copy, not the social caption. Preserve longer approved wording in the website body, and
report the caption limitation before social scheduling; do not silently shorten approved social copy. The store and
worker support some per-provider captions, but this publish input currently does not expose them.

Scheduling currently uses Restate: `portal/src/blog_api.rs::queue_publication` starts `SocialPublisher`, whose durable
sleep waits for the publication time in `workflows-service/src/social_publisher.rs`. Its reconciler recovers scheduled
Posts through the same engine. DBOS does not yet run either social workflow; see
[`dbos-cutover-matrix.md`](../../../docs/dbos-cutover-matrix.md). Do not claim that enabling DBOS migrates this path.
Before a cutover, preserve authorization, stable publication identity, receipt reconciliation, and the production-only
egress gate, and prove restart/replay without duplicate provider sends.

These contracts are grounded in `portal/src/blog_api.rs` (`record`, `prepare`, `queue_publication`),
`webapp/src/brand_blog_posts.rs::schedule_post_form`, `store/src/posts.rs::validate_copy`, and
`workflows-service/src/social_publisher.rs` (including its zero-provider-call tests for nonproduction environments).

### Prepare and send

1. Check `navigator site whoami --host <host>`. The caller must be Owner or the Brand Firm's Admin DRI.
2. Run the read-only preflight before uploading:

   ```bash
   navigator site blog preflight <brand> <slug> --providers facebook,instagram,tiktok --host <host>
   ```

   Supply only the requested providers. A nonzero exit with `ready_to_prepare: false` reports setup blockers and a
   `setup_url` for the Brand's Navigator connection page. If the installed CLI lacks `preflight`, update it to the
   release that includes this command or use this checkout's `cargo run -p cli -- site blog preflight ...`. Inspect
   `blog status` when setup is blocked. Missing `connection_setup` or `publisher_configured` fields in that response
   require deploying the preflight-capable server before readiness can be established. An empty `accounts` list means
   this deployment has no stored OAuth connections for the Brand; it does not prove the provider profiles are absent or
   that publishing code is missing. Public handles and a signed-in provider website do not authorize Navigator.

   Complete provider app configuration using [account onboarding](../../../docs/social-account-onboarding.md), then have
   the authorized operator connect each intended account in Navigator. Provider approval, consent, runtime KMS, and
   deployment configuration remain prerequisites after a code release. Keep secrets in the approved secret flow. Re-run
   preflight and verify every returned destination belongs to the intended public profile. Pin its `account_ids` in the
   Post source; a mismatch is refused before writing or sending. Preflight checks stored configuration, connections,
   permissions and reconnect requirements. Media checks and actual provider delivery happen on publish.

   If the user prefers Navigator, explain concrete blockers and continue the authorized Navigator setup work. Browser
   fallback is optional; agree on that change of workflow before taking control of the user's browser. When the user
   takes over a provider, stop interacting with it and reconcile any submission already made before retrying.
3. For video, inspect the MP4 with `ffprobe`, preserve the original resolution where practical, and keep the upload
   at or below 64 MiB. Compare compressed frames with the original and check decoding before uploading under
   `img/<brand>/`:

   ```bash
   navigator site asset upload --host <host> --key img/<brand>/<video>.mp4 /absolute/path/video.mp4
   ```

   When the operator explicitly authorizes direct GCS uploads and supplies the target buckets, the signed-in `gcloud
   storage cp` path is also available. Preserve the same asset key and video MIME type in each requested bucket, then
   verify the uploaded bytes and public asset URL. Bucket access authorizes media storage; social publication still
   requires the Brand's provider connections. Keep bucket names and account details outside Git. If gcloud storage
   crashes with `ssl.PROTOCOL_TLSv1` missing, select an installed, supported Python runtime with OpenSSL 3 through
   `CLOUDSDK_PYTHON`. Verify that runtime with the storage command; do not patch SSL or weaken TLS.

4. Write the approved source outside Git when it contains account-specific or private operational information:

   ```yaml
   brand: example
   slug: video-update
   title: A video update
   description: The approved caption.
   body_markdown: The approved website Post text.
   video_asset: img/example/video-update.mp4
   providers: [instagram, threads, tiktok]
   account_ids:
     instagram: verified-instagram-account-id
     threads: verified-threads-account-id
     tiktok: verified-tiktok-account-id
   tiktok_ai_generated: false
   ```

   Include only the requested providers. Obtain the actual AI-content disclosure; do not assume `false`. Omit
   `video_asset` for text-only providers. Optional `publish_at` is a future RFC 3339 timestamp; omitting it queues
   publication now. Portrait orientation is measured from the uploaded MP4.

5. Publish and inspect the result:

   ```bash
   navigator site blog publish /absolute/path/post.yaml --host <host>
   navigator site blog status <brand> <slug> --host <host>
   ```

The website composer on `/app/social/calendar` submits the same approved Post command. Upload the video through
`/app/admin/blog`, select the Brand and connected destinations, enter the caption and website copy, and supply a
publication time with its UTC offset. A blank time requests immediate publication. A deployed composer and verified
account connections are prerequisites; source changes alone do not make production ready.

`queued` acknowledges durable submission. Only a confirmed provider receipt establishes publication; its permalink may
still be pending. Repeated schedules keep the same workflow identity. An already-public Post cannot be sent again by
this command. For an ambiguous provider outcome, reconcile the existing publication instead of making another slug.
Staging does not send to live providers. Keep credentials out of source files, command arguments, and output.

## Database-backed publication

Publishing through the app or CLI stores the Post in the database before submitting its durable workflow. Provider
receipts and verified permalinks are stored as Social Publications. Public article pages and homepage video cards read
confirmed receipts alongside explicit Post links, so future publications do not need a seed-file edit to display their
network links. Pending or unconfirmed links remain hidden. A manually recorded link takes precedence for its provider.

Deployment and verified account connections are still required. A provider-website upload alone does not create a
Navigator Post; use the supported recording flow for existing Post rows when publishing manually.

## Record an existing post

Use the exact public permalink. Record the Brand's verified public handle first if absent:

```bash
navigator site blog handle <brand> <provider> <handle> --host <host>
navigator site blog link <brand> <slug> <provider> <public-post-url> --host <host>
navigator site blog status <brand> <slug> --host <host>
```

Linking records display data and never sends. Verify the returned link belongs to the intended Brand and Post. A server
`404` for these API routes means the deployment needs the CLI publishing release; do not fall back to direct production
database writes.

Instagram videos publish as Reels. Stories are separate publications and this command does not create them.

## Provider website fallback

When the intended accounts are already signed in, an authorized operator can publish the approved video directly through
Meta Business Suite or TikTok Studio while Navigator account setup is incomplete. Use the original video when the
provider accepts it. Verify the exact Brand profile and include only the requested destinations; Meta may preselect
Stories or Threads in addition to Facebook and Instagram.

Enter captions using real keyboard input or a supported browser fill operation. A native accessibility value setter can
change the displayed text without updating the editor's saved value. Read back the caption after submission, and inspect
each destination separately before reporting completion. If a correction is needed, edit the existing post rather than
uploading another copy. TikTok Studio can refuse editing a commercial-content post and require the TikTok phone app;
report that concrete handoff instead of deleting and reposting.

A processing notice or content review is pending, not confirmed publication. Record only verified public post URLs
through the CLI link command; provider dashboards and insights URLs are not public permalinks.

## Comments and questions

Facebook and Instagram comments and messages use `/app/inbox` after the Brand's accounts and verified webhooks are
connected. Account OAuth alone does not establish webhook delivery. Test ingestion and replies with synthetic fixtures;
a fake provider test does not prove production subscriptions. Sending a live reply needs authorization for that message
and recipient. TikTok public text comments use the signed `/webhook/tiktok` callback and require verified `comment.list`
and `comment.list.manage` grants. After developer-app permission approval, reconnect each Brand and register its
`COMMENT` subscription through the Business Webhooks API. Follow the [TikTok setup
guide](../../../docs/tiktok-publishing.md) and verify an actual signed audience comment reaches the inbox before
declaring production ready. Replies are limited to 1,200 characters; TikTok direct messages and image replies are
unsupported.
