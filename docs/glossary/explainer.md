---
title: "Explainer"
description: "An Explainer is a Brand's public page that explains a process or prepares questions for a legal review."
---

An Explainer is a [Brand](brand.md)'s public educational page. It introduces a process or helps a visitor organize
questions and documents before a legal review. It states its jurisdictional scope when describing legal procedure, links
authoritative sources for concrete legal facts, and distinguishes general information from advice about a matter.

Explainers use the existing typed marketing-page catalog and renderer. A Brand declares their routes and catalog stems
through `BrandKey::explainer_pages`; no separate database table is created. The route gate, sitemap, and `llms.txt`
index use that declaration so the page belongs to the Brand that publishes it. Its contact action uses the Brand's
configured consultation URL.

- Copy contract: [marketing copy](../marketing-copy.md).
- Catalogs: [Aegis](../../neon/locales/en/aegis/) and [Groundwork](../../neon/locales/en/groundwork/).
