<!-- navigator: workspace AGENTS.md. `navigator sync` restores this file; put Firm rules in the Firm Library. -->

# Navigator workspace

This folder is a Navigator workspace root. Each child folder is one Project repository, named for its Project code.
`navigator sync` maintains everything else here.

## Rules from each Firm

Each Firm keeps its own rules and skills in its private Firm Library repository. `navigator sync` checks it out at
`.firm/<firm-slug>/`. When you work in a Project, Codex loads its combined Project and Firm instructions from
`AGENTS.override.md`. Claude loads the Firm rules from `.claude/rules/AGENTS.md`. Both files are local to the checkout
and maintained by sync. Never apply one Firm's rules, accounts, vendors, or skills to another Firm's Project.

## Layers

| Path | Holds | Owner |
| -- | -- | -- |
| `AGENTS.md` | This file | The `navigator` release |
| `.agents/skills/<skill>/` | Generic skills shared by every Firm | The `navigator` release |
| `.agents/skills/<firm-slug>/<skill>` | Links to one Firm's skills | `navigator sync` |
| `.firm/<firm-slug>/` | The Firm's rules and skills | The Firm, through reviewed changes |
| `<code>/.claude/skills/` | Generic skills plus that Project's Firm skills | `navigator sync` |
| `<code>/AGENTS.override.md` | The Project contract with its Firm rules, loaded by Codex | `navigator sync` |
| `<code>/.claude/rules/AGENTS.md` | A link to that Project's Firm's rules | `navigator sync` |

Edit a generic skill in Navigator, a Firm skill in its Firm Library, and a matter's own rules in its Project repository.
Edits made here are overwritten by the next `navigator sync`.

## Always

- The installed `navigator` CLI is the authority. Check `navigator <verb> --help` before running a verb a skill names.
- Email stays a draft. The lawyer reviews and sends it.
- Legal documents, notes, and mail never enter Git. A Project repository commits pointers, not bytes.
- Do not name one matter inside another matter's repository, or in a commit, branch, or pull request.
- Linear issues carry no personal information: no client, matter, Project code, email, phone, or address.
