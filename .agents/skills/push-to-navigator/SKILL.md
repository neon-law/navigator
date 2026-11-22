---
name: push-to-navigator
description: Land a Project repository change and follow the deployment of the exact merge commit to a verdict.
---

# Push to Navigator

Follow the **Find the term first** guidance in `AGENTS.md` when naming Project concepts.

Follow [push-changes](../push-changes/SKILL.md) from the Project checkout. Run `navigator jj push` and report the review
URL, merge commit, and deployment result. The Project's origin selects GitHub or Forgejo. For a Forgejo checkout, the
trusted host supplies `NAVIGATOR_FORGEJO_ROOT_URL` and `FORGEJO_TOKEN`.

A successful check on a review head proves that revision passed CI. Confirm the separate post-merge deployment run on
the exact merge commit. Inspect its jobs before reporting what reached the portal. A publish job that skipped its work
provides no evidence of a client-visible change.
