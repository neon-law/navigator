# Navigator editor integration

`navigator-lsp` brings Navigator's Markdown and Notation diagnostics to any editor that supports the Language Server
Protocol. It provides the same rule results and safe fixes as `navigator project gate`, over local JSON-RPC with no
telemetry.

It serves attorneys and engineers who review or edit repository Markdown outside the web editor. The integration is
necessary so local feedback, pull-request review, and CI enforce one rulebook rather than discovering structural errors
at different stages.

Install the `navigator-lsp` binary, then configure the editor to launch it for Markdown files. The server contract lives
in the [`lsp` crate](../../lsp/README.md); editor-specific configuration belongs in the editor or extension.

## Where to get the binary

The normal way a human gets `navigator-lsp` locally is `navigator lsp`: it resolves the platform-matching
`navigator-lsp-<tag>-<platform>` archive for **the running `navigator` binary's own version** (the same tag `navigator
--version` reports), downloads it from the public distribution host's GitHub Releases
([`neon-law-source-code/homebrew-navigator`](https://github.com/neon-law-source-code/homebrew-navigator/releases) — this
repository's `deploy.yml` mirrors every archive it attaches to its own Release there too, tag for tag, so the download
keeps resolving anonymously independent of this repository's own visibility), and writes a working `navigator-lsp`
executable to the Downloads directory. A version mismatch or missing release asset is a clear error naming the tag it
looked for — never a silent fallback to "latest".

```bash
navigator lsp
```

Every tagged release attaches a `navigator-lsp-<tag>-<platform>` archive to its own GitHub Release and mirrors it onto
[`neon-law-source-code/homebrew-navigator`'s
Release](https://github.com/neon-law-source-code/homebrew-navigator/releases), alongside the `navigator` CLI archives —
`navigator-lsp-<tag>-macos.tar.gz` and `-linux.tar.gz`. Each carries the executable beside `LICENSE`. `navigator lsp`
downloads directly from these archives. They are also the source an editor extension should resolve against, since each
is a per-version asset on an immutable tag; `navigator ops lsp publish` instead mirrors the binaries to the site's
public assets bucket at a "latest" key for the site's own use, which is not a stable target for an extension pinned to a
release, and is an operator-only command distinct from both of the above.

`navigator-lsp` is MIT OR Apache-2.0, like the rest of the workspace. Its distributed archives carry the license and
attribution notices. An editor extension's own code and dependencies need their own verified licensing and notices;
licensing the language server does not establish rights in a separately maintained extension.
