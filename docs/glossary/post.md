---
title: "Post"
description: "A Post is one Brand's article, stored as a database row and presented at /blog on that Brand's host."
---

An article owned by one registered [`Brand`](brand.md). The `post` table is the authority: the row holds the article's
Markdown, its schedule, its per-provider captions, and its social links. `/blog` is the Post's public presentation: the
Brand's own host lists it at `/blog` and serves it at `/blog/{slug}`, rendering the stored Markdown through the
sanitizer when it is served. A Post is public once it is not a draft and its time has passed, whatever its social
delivery outcome, and its publication time cannot move once public.

The Posts Neon Law has already published are seed data, in
[`store/seeds/neon/Post.yaml`](../../store/seeds/neon/Post.yaml). A [Brand Seed](brand-seed.md) creates a Post whose
Brand and slug are new and never overwrites one that exists, so an edit made in the database, or a link encoded after
the fact, survives every boot. A Post's copy is enforced where it is written: a kebab-case slug, a one-line title of at
most 120 characters, a one-line description of at most 200, a body, and captions with no control characters. A Brand
must be a registered Brand.

A Post's social links are nullable. A link names a provider and the Brand's [Social Handle](social-handle.md) on it,
which must already exist; its `post_id` and `permalink` stay empty until the provider post exists, and are encoded
afterward. The permalink of an X post is derived from the handle and the post id. A link with no permalink is not shown
publicly. A link names the Brand's handle, never a token, and recording one never sends.

Public articles and homepage video cards also read confirmed [Social Publication](social-publication.md) receipts for
that Post. Valid explicit links take precedence for the same provider; receipts fill missing providers without an
invented handle or another copy of the link. Unconfirmed receipts and receipts with no public permalink stay hidden.

The admin catalog, social dashboard, and calendar at `/app/admin/blog` and `/app/social` show a publishing schedule at
the top: what will go out, soonest first, and what went out, most recent first. The schedule reads only the `post` and
[`Social Publication`](social-publication.md) rows, so it does not depend on which durable workflow engine does the
sending. The calendar and admin catalog also provide a native form to approve copy, choose connected destinations, and
queue immediate or future publication through the same durable command as the CLI. A refused submission preserves the
copy for correction. Cancel holds a future schedule before it is sent.

- Schema, seed, and links: [`store::posts`](../../store/src/posts.rs)
- Rendering: [`portal::blog`](../../portal/src/blog.rs)
- Commands: `navigator site blog preflight`, `publish`, `record`, `status`, `handle`, and `link` use the site login.

`preflight` checks the requested destinations' stored Social Accounts and deployment configuration before an upload. It
returns setup blockers and the Brand's connection page without sending. Provider delivery still needs a confirmed Social
Publication receipt.

`navigator site blog publish post.yaml` queues approved copy and explicitly selected providers through the durable
Social Publisher. A future `publish_at` schedules delivery; omission requests immediate delivery. The server checks
Brand authority, connected accounts, approved provider `account_ids`, and uploaded video dimensions. Repeating a
scheduled request retains its publication identity; an already-public Post cannot be sent again. `status` reads
receipts; `link` records a public provider permalink without sending. See
[publish-social-post](../../.agents/skills/publish-social-post/SKILL.md).

`navigator site blog record post.yaml` creates an already-published website Post without scheduling social delivery. It
requires approved website copy and an actual publication time no later than now. An optional uploaded video is measured
and checked against the owning Brand. Provider connections are unnecessary; verified public links can be recorded
afterward. An existing Post is refused so a retry cannot replace content or change its delivery schedule.
