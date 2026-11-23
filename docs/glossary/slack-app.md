---
title: "Slack App"
description: "A workspace-installed Slack integration authorizing Navigator to call Slack APIs."
---

A Slack App is a provider-side integration installed in a Slack workspace. It authorizes a [Firm](firm.md) to call Slack
APIs through Navigator. The Firm stores the installation's bot token as its `slack_bot_token` [Firm Integration
Secret](firm-integration-secret.md); every Slack call Navigator makes uses one Firm's bot. A Project's notices use its
owning Firm's bot. Notices with no Firm in context use the deployment's Slack Firm: `SLACK_FIRM_ID`, or the Firm wearing
Neon when unset. The workspace's display name and installed identifiers are separate values. New workspace installations
use **Navigator** as the display name; a workspace administrator changes that display name in Slack. It is provider
configuration, not a Navigator row or a Person's [External System Identity](external-system-identity.md).
