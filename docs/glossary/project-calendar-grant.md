---
title: "Project Calendar Grant"
description: "A Project Calendar Grant records a direct reader grant managed by Navigator."
---

A [Project Calendar](project-calendar.md) reader grant that the membership job manages. The PostgreSQL row keys the
grant by Project, Firm, Calendar ID, and address. It records intent before inserting the reader grant. On retry, the job
reads the Google access list and preserves existing manual grants.

The table is `project_calendar_grant`.
