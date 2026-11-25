# Connect Claude to Navigator

Add a remote MCP server in a client that supports Streamable HTTP. For Claude Code, run:

```sh
claude mcp add --transport http navigator https://www.neonlaw.com/app/mcp
```

The deployment answers `tools/list` with the tools available for this connection. Anonymous connections offer
`list_public_pages` and `list_templates`. `list_public_pages` returns neonlaw.com URLs so a model can cite the public
site, including the home page and `/lawyers`. For signed-in access, use a client that can securely send the deployment's
bearer credential in the HTTPS Authorization header. Refresh it when it expires. Use the public catalog for hosted
ChatGPT.

## Find a Template in chat

Ask Claude to list Navigator Templates or describe the document you want. The read-only `list_templates` tool returns
the codes and titles of public Templates in the bundled shared catalog, with descriptions where supplied. Claude can use
that metadata to suggest a Template.

When the request is “create a work in progress,” pass the wording as `intent`. The tool returns a clarification asking
whether the user wants a Work In Progress summary for a matter or wants to record time or a cost. Ask the question
before suggesting a Template or taking action.

## Signed-in tools

Ask for your Projects, deadlines, or other tools available to your account. The server verifies the caller and applies
the tool's role and Project rules. A write requires a lawyer-tier Person. Navigator records the actor's ID, role, tool,
and tier decision. Actions that require explicit lawyer approval take place in Navigator.

Which lens depends on the tier, and the two answer different questions, so they return different shapes. The `lens`
field on the response says which one came back.

| Tier | Lens | What comes back | Predicate |
| --- | --- | --- | --- |
| `lawyer`, `clerk`, `client` | `membership` | the matters the caller is on, with `id` | `access::visible_projects` |
| `owner`, `admin` | `directory` | `code`, `name`, `status`, `lawyer_dris` | `projects::matter_directory` |

The membership lens is the same predicate `/app/projects` renders from, and it carries the `id` the write tools take.

Owner and Admin are not invited to matters, so they hold no participation row and the membership lens would correctly
show them nothing. The directory lens is oversight instead: it answers which matters exist and who is accountable for
each, and carries nothing a matter *contains*. There is no `id` on it deliberately — `code` is the stable handle, and
anything a matter holds is membership's to disclose. A `lawyer` does not get this lens.

An authenticated email with no `persons` row lists nothing, and neither does a caller whose tier holds no matching
participation row. Sign-in does not create a Person, so a stranger with a valid token is exactly the caller who must
reach nothing.

`show_person` searches the firm's own people directory, so it is firm-side: `owner`, `admin`, and `lawyer` read it,
while `clerk` and `client` are refused. A refusal rather than an empty list, because nothing the model could type would
make the read succeed, and "no matches" would teach it the directory is empty.

## Opening a matter end to end

Everything the onboarding chain needs is in the offered set, so this is one conversation:

1. **The organization and its people.** Hand Claude the contact list and ask it to load them. It calls
   `bulk_import` with one document; find-or-create means a re-run changes nothing. The payload contract is
   [`bulk-contact-import.md`](bulk-contact-import.md).
2. **The matter.** `create_project` against the entity that import created. It needs a `code`, stored exactly as
   given — later the base of its repository name. A code is chosen once at matter-open and never changes; give Claude
   the exact code rather than a stem for it to slug, since a code already in use by another matter is refused, not
   disambiguated. It also requires the opening attorney's conflict `attestation`, and refuses without it.
3. **Who is on it.** `link_person_project` per participant.

`create_project` provisions the matter's repository itself, best-effort: `store::project_surfaces::reconcile_after_open`
runs right after the matter opens and, when forge credentials are configured, creates or adopts the GitHub repository
named for the code and records its URL on `projects.repository_url`. Missing forge configuration skips that step rather
than failing the open, so an unconfigured deployment still records the coordinate with no repository behind it yet. An
operator reconciles later:

```bash
navigator project setup <code>
```
