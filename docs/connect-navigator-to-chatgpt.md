# Use Neon Law Navigator in ChatGPT

Use the public catalog to find an LLC or business agreement template. You can browse it without a Neon Law account. You
enter private details and start the paperwork on Neon Law’s website.

## Connect the public catalog

1. Sign in to [ChatGPT Plugins](https://chatgpt.com/plugins). Select **+**, then **Add custom MCP server**. If the
   option is missing, ask your workspace administrator whether your account can install custom plugins.
2. Enter **Navigator** for the name and **Find public legal templates from Neon Law** for the description. Paste
   `https://www.neonlaw.com/app/mcp` as the server URL, including `www`. Choose **No authentication**. If your client
   asks for a transport, select **Streamable HTTP**.
3. Review the connection details, select **Create as a plugin**, then **Connect Navigator** and **Try in chat**. In a
   new chat, type **@** and select **Navigator**. Ask “List Navigator’s public templates.” Look for **Nevada LLC
   Formation** in its response.

OpenAI documents the [custom MCP connection and installation
steps](https://developers.openai.com/plugins/deploy/connect-chatgpt).

## Start a Nevada LLC

Ask “Help me form a Nevada LLC. Show me the formation template and where to start.” Choose **Nevada LLC Formation**
(`nv__llc_formation`). You can [preview the template](https://www.neonlaw.com/notations/nevada-llc-formation) before you
[start the questionnaire](https://www.neonlaw.com/notations/nevada-llc-formation/start). Sign in to Navigator and choose
**Start**. Navigator saves each business as its own formation in your personal Project. Use the same link to resume the
business by name or choose **Start a separate Nevada LLC**. **Move this formation to its own Project** keeps that
draft’s answers and gives the business its own Project under the same account.

The questionnaire asks about the proposed company, registered agent, management, and formation date. Your answers save
on Navigator. You file the paperwork with the state. Nevada forms the LLC when the Articles of Organization filing takes
effect. Check the filing confirmation for the effective date. [Nevada Revised Statutes, Chapter
86](https://www.leg.state.nv.us/NRS/NRS-086.html)

An operating agreement governs how the company runs. Ask “I’m the only owner of a member-managed Nevada LLC and own it
personally. Is there an operating agreement I can start?” Use the matching agreement if Navigator lists it. For other
ownership or management arrangements, [contact Neon Law](https://www.neonlaw.com/contact) about the appropriate draft.
To resume an existing matter, open [My Projects](https://www.neonlaw.com/app/projects) and sign in there.

## Verify the connection

This read-only request exercises the same public tool that ChatGPT calls. It requires no Neon Law credentials.

```sh
curl --fail-with-body --silent --show-error https://www.neonlaw.com/app/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_templates","arguments":{}}}'
```

Look for `nv__llc_formation` in the returned templates. To check the web handoff, open the questionnaire in a private
browser window. It should take you to Navigator sign-in with a return path to intake.

## Publish in the ChatGPT directory

The [Navigator plugin package](../plugins/neon-law-navigator/) includes the public MCP connection, onboarding
instructions, and review prompts. Install it in a test workspace and exercise the prompts against the hosted catalog.
For public distribution, the publisher signs in to the OpenAI developer platform, verifies the domain, uploads the
package, provides a walkthrough for reviewers, and submits it for review. Follow the [OpenAI submission
guide](https://developers.openai.com/plugins/deploy/submission).
