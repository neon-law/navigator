# Navigator how-to

Use Navigator from an AI assistant through MCP or from a terminal with the Navigator CLI. In a Project repository,
Jujutsu (`jj`) manages local changes and publishes them to the Project's Git forge for review.

## Connect to Navigator from chat

For public Template discovery, add a custom **Streamable HTTP** MCP server in your assistant and enter
`https://www.neonlaw.com/app/mcp`. Ask “List Navigator’s public Templates.” You can browse without signing in.

To register the public catalog in Codex, run:

```sh
codex mcp add navigator-public --url https://www.neonlaw.com/app/mcp
```

For signed-in tools, configure your client's secret store to send your deployment's short-lived access token as an HTTPS
bearer credential. In Codex, supply the token through its launch environment as `NAVIGATOR_MCP_TOKEN`, then run:

```sh
codex mcp add navigator --url https://www.neonlaw.com/app/mcp --bearer-token-env-var NAVIGATOR_MCP_TOKEN
```

Start a new Codex session and ask “List my Projects.” Navigator applies your account's access rules. Refresh the
credential when it expires. Keep tokens out of chat, command arguments, and repository files.

## Check your calendar and billing

Run `navigator calendar` for your agenda and `navigator billing` for the claims that need your attention, followed by
your time. Both use your signed-in account, including inside a Project checkout. Run `navigator project calendar` for
the matter's shared calendar or `navigator project billing` for its timekeepers, fees and claims. Outside the checkout,
use `--project <code>` on either Project command.

## Set up a Project checkout with Jujutsu

On macOS, get the repository URL from the Project’s Navigator page. Replace the example URL and code in these commands
with the Project’s values.

```sh
git clone https://forge.example/firm/sample-project sample-project
cd sample-project
navigator jj setup
jj status
```

The setup command installs Jujutsu through Homebrew if needed and colocates it with Git. It copies the checkout's Git
identity and signing settings. To inspect the remote repository's review policy, run `navigator jj policy --dry-run`.

[Jujutsu Git compatibility guide](https://docs.jj-vcs.dev/latest/git-compatibility/)

Start a change from the refreshed `main@origin` in a clean checkout. The delivery command uses the Project's `origin`
remote.

```sh
jj git fetch --remote origin
jj new main@origin
```

Edit `navigator.yaml`, reusable Drafts under `drafts/`, or document Pointers under `blobs/`. Preview changed Matter
Pages and run the Project gate before publishing.

```sh
hudson preview
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
navigator jj push
```

The command opens or resumes a pull request to `main` on the configured forge, follows the required check and merge, and
reports the deployment run for the exact merge commit. Re-run with the same revision to resume after an interruption.
Review client-facing Matter Page lenses and document Pointer visibility before pushing.

## Give agents the Project rules

Preserve the Project's generated `AGENTS.md` and approved local instructions when adding the Jujutsu workflow.

[Jujutsu Project example](examples/jujutsu-project/AGENTS.md)

The `navigator project repository sync-skills` command restores the canonical `AGENTS.md`. Keep a copy of any Jujutsu
instructions you want to reapply. See the [Project repository contract](project-repositories.md).
