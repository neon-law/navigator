---
name: pr-image-upload
description: >
  Embed a local screenshot or GIF into a `github.com` PR (or issue) body so it actually RENDERS, driven from the CLI: no
  drag-drop, no committing the file, no image-hosting branch, no release/tag. One `curl` uploads the `/tmp` capture to
  the `user-attachments` store using nothing but `gh auth token`, and returns a real
  `https://github.com/user-attachments/assets/…` URL to drop into the body. Trigger as the embed half of [[create-pr]]'s
  walkthrough step (after [[web-preview]] captures the visual), when a reviewer comment on a PR asks for a "live
  walkthrough"/screenshot during [[review-pr]], or any time you have a `/tmp` image that must appear in a PR/issue body
  or comment. Capture lives in [[web-preview]] (a walkthrough defaults to a GIF of the real interaction, a still only
  for a genuinely static change); this skill only hosts and embeds it.
---

# Embedding screenshots in a PR body from the CLI

An `<img src="/tmp/…">` in a `gh`-created body renders **broken** (it resolves to `https://github.com/tmp/…`, a 404).
`AGENTS.md` rules out committing the capture, pushing an image-hosting branch, or cutting a release/tag to host it.

Use the **user-attachments** store instead, reached with one authenticated request. It needs an OAuth token with push
access to the target repository, which is what `gh auth token` returns.

Do not use the `drogers0/gh-image` extension. It replays a browser-cookie upload flow and cannot target this repository;
the token route below skips that flow entirely.

## The recipe

```bash
# 1. Capture to /tmp first, never into the repo tree. Default to a GIF of the real interaction;
#    use a still only for a genuinely static change.
# 2. Look at it yourself: Read the PNG/GIF so it renders inline, and confirm it shows the change.
# 3. Upload. Keep the token out of `ps` by passing it through a curl config file, not `-H` in argv.
REPO_ID=$(gh api repos/neon-law/navigator --jq .id)
umask 077
gh auth token | sed 's/.*/header = "Authorization: Bearer &"/' > /tmp/.curlcfg
URL=$(curl -sS -K /tmp/.curlcfg -X POST \
  -H "Accept: application/json" -H "Content-Type: image/gif" \
  --data-binary @/tmp/navigator-screenshots/walkthrough.gif \
  "https://uploads.github.com/user-attachments/assets?name=walkthrough.gif&content_type=image%2Fgif&repository_id=$REPO_ID" \
  | jq -r .url)
rm -f /tmp/.curlcfg

# 4a. New PR: reference $URL in the body you pass to `gh pr create` (an <img> tag or ![alt]($URL)).
# 4b. Existing PR: splice it into the current body and update.
gh pr view <N> --json body --jq .body > /tmp/body.md
printf '\n\n## Walkthrough\n\n<img alt="…" src="%s" />\n' "$URL" >> /tmp/body.md
gh pr edit <N> --body-file /tmp/body.md
```

A `201` returns `{"url":"https://github.com/user-attachments/assets/<uuid>"}`; `url` is the only field.

Upload **all** of a PR's images in one pass, then run a single `gh pr edit`.

## Verify the render, not the asset URL

**Do not verify by curling the asset URL with the token.** An OAuth token is not a web session, so the request follows a
redirect and returns about 39 KB of `text/html`. That looks like a broken upload but is not one.

Ask GitHub's renderer instead. A working asset resolves to an `<img>` on `objects-origin.github.com` with a signed
`X-Amz-Signature` and `response-content-type=image/…`:

```bash
gh api /markdown -X POST -f mode=gfm -f context=neon-law/navigator \
  -f text="![probe]($URL)" | head -3
```

## Rules and caveats

- **Images and video only.** Anything else returns `422 content_type is not included in the list of allowed content
  types`, and there is no token route for it. Deliver that file to the user with `SendUserFile` and say why it is not
  embedded.
- **URL-encode `content_type`.** A bare `+` in `image/svg+xml` arrives as a space and is rejected.
- **Upload to `uploads.github.com`**, not the web host.
- **Assets inherit repository visibility.** On a private or internal repository, an anonymous fetch gets a 302; that is
  correct, not a failure.
- The token needs **push access** to `repository_id`. A `404` from the upload endpoint means the token lacks it (or the
  parameter is missing), not that the URL is wrong.
- **Never** commit the capture, push an image-hosting branch, or create a release/tag to host it (see [[web-preview]]
  and `AGENTS.md`).

## When a review comment asks for a walkthrough

A [[review-pr]] finding such as "missing live walkthrough artifact" is fixed here: capture the changed states
([[web-preview]]), embed them in the PR body with this skill, then **reply to the thread and resolve it**, noting what
the capture shows.
