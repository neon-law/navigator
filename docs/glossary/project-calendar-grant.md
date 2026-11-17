---
title: "Project Calendar Grant"
description: "A Project Calendar Grant records a direct reader grant managed by Navigator."
---

A [Project Calendar](project-calendar.md) reader grant that the membership job manages. The PostgreSQL row keys the
grant by Project, Firm, Calendar ID, and delivery address. It records intent before requesting the Google invitation.
Readback after a lost provider response avoids sending the invitation again. The notification field records whether
Google acknowledged the request; it does not record delivery or acceptance.

The table is `project_calendar_grant`.
