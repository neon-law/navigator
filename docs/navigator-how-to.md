# Navigator how-to

Use Navigator from an AI assistant through MCP or from a terminal with the Navigator CLI. In a Project repository,
Jujutsu (`jj`) manages local changes and publishes them to the Project's Git forge for review.

## Connect to Navigator from chat

For public Template discovery, add a custom **Streamable HTTP** MCP server in your assistant and enter
`https://neonlaw.com/app/mcp`. Ask “List Navigator’s public Templates.” The connector offers public Template metadata
without sign-in once the deployment serves the public catalog. An HTTP 401 from an anonymous request means that
deployment still requires credentials.

For signed-in tools in local Codex, run these commands on the same computer as the desktop app.

```sh
navigator site login --host www.neonlaw.com
navigator site whoami --host www.neonlaw.com
codex mcp add navigator -- navigator-deploy site mcp --host www.neonlaw.com
```

Restart the desktop app and type `/mcp` to check the connection. The local bridge reads the saved session from
`~/.navigator.json` on each call. Sign in again when the session expires. Keep the credential file on your computer and
out of chat and source control. For Claude setup, see the [MCP client guide](claude-mcp-client.md).

Try “List my Projects” after signing in. Navigator applies your account's access rules. A hosted assistant needs its own
supported authentication flow because it cannot read the credential file on your computer.

## Set up a Project checkout with Jujutsu

On macOS, get the repository URL from the Project’s Navigator page. Replace the example URL and code in these commands
with the Project’s values.

```sh
git clone https://forge.example/firm/sample-project sample-project
cd sample-project
navigator project repository jj
jj status
```

The setup command installs Jujutsu through Homebrew if needed and colocates it with Git. It copies the checkout's Git
identity and signing settings.

[Jujutsu Git compatibility guide](https://docs.jj-vcs.dev/latest/git-compatibility/)

Start a change from the refreshed `main@origin` in a clean checkout. If the Project uses a different Git remote name,
substitute it in the commands.

```sh
jj git fetch --remote origin
jj new main@origin
```

Edit `navigator.yaml`, reusable Drafts under `drafts/`, or document Pointers under `blobs/`. Preview changed Matter
Pages and run the Project gate before publishing.

```sh
navigator-deploy preview
```

Stop the preview when done, then run:

```sh
jj status
navigator project gate
jj diff
jj describe -m "Describe the Project source change"
```

Before publishing, refresh the base and rebase the single change if `main` advanced. Resolve conflicts, rerun the gate,
and verify the final change carries the required signature. Then publish only the reviewed change.

```sh
jj git push --remote origin --change @
```

Open a pull request from the printed bookmark to `main` on the Project's forge. Follow the required `ci` check and merge
policy. Review client-facing Matter Page lenses and document Pointer visibility before merge.

## Give agents the Project rules

Preserve the Project's generated `AGENTS.md` and approved local instructions when adding the Jujutsu workflow.

[Jujutsu Project example](examples/jujutsu-project/AGENTS.md)

The `navigator project repository sync-skills` command restores the canonical `AGENTS.md`. Keep a copy of any Jujutsu
instructions you want to reapply. See the [Project repository contract](project-repositories.md).
