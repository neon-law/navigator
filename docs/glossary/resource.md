---
title: "Resource"
description: "A Resource is one of the private or client-shared places where work on a Project happens."
---

One of the places work on a [Project](project.md) happens: a private Slack channel, a private Notion page, a private
Google Drive folder, the firm-side [Project Calendar](project-calendar.md), and — optionally — a Slack channel shared
with the client, a Notion page shared with the client, the client's [Booking Link](booking-link.md), and the matter's
[Portal](portal.md). Rendered as the matter page's *Resources* panel, each row opening on its mark.

A resource is **firm-only or shared, and its name says which.** That split is the point rather than a label: the private
Notion page holds firm work product and the private channel holds lawyer-only chatter, so a client who could see either
would be reading the other side of their own matter. `webapp::project_resources::visible_resources` is the single place
the split is applied, and it filters by *audience* — a firm-only resource is never built for a client, so its URL
reaches neither the markup nor the hydration payload.

**An unset resource is absent, never an empty slot.** A matter with no shared Notion page and a matter whose firm keeps
one privately look identical to the client, which is the same toggle-blindness a [Module](module.md) gets from having no
row. Reading the private half is every firm tier ([Clerk](role.md) included); *configuring* any of them is the lawyer
tiers, through the matter edit form — the panel renders no inputs of its own, so there is one write path. Slack and
Notion each render as their own card there, private field beside a "Share a separate resource with the client" toggle
that reveals the shared field: the toggle is a real submitted checkbox the handler reads directly, so unchecking it and
saving is what clears the shared column, whatever text is left in the field it hid.

Five are stored URLs on the `project` row (`internal_slack_channel_url`, `external_slack_channel_url`,
`private_notion_page_url`, `shared_notion_page_url`, `booking_url`), each validated by
[`store::projects::is_valid_resource_url`](../../store/src/projects.rs) because each is rendered as an `href`. The Drive
row is derived from `drive_folder_id`, the calendar row from `calendar_id`, and the portal row is configured by the
matter existing rather than by a column. The calendar id and the booking link are set with `navigator project set`, not
the edit form.

Navigator stores addresses, not permissions. Who may open a Notion page or a Drive folder is governed by that service's
own sharing, which Navigator neither reads nor enforces — so a page named "private" here is only private if it was
shared that way in Notion.

- Rendering and the audience filter: [`webapp::project_resources`](../../webapp/src/project_resources.rs) · marks:
  [`webapp::components::resource_mark`](../../webapp/src/components/resource_mark.rs) · columns:
  [`store/src/schema/navigator.surql`](../../store/src/schema/navigator.surql)
