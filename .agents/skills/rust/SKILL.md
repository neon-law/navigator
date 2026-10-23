---
name: rust
description: >
  Workspace Rust guardrails. Trigger on the sharpest moments: a change that adds `unsafe`, `unwrap`, `expect`, or
  `panic!` outside `main()`/tests; introducing a new public API, error type, or module; reaching for a different web
  framework, store client, or async runtime (we standardize on Axum + PostgreSQL + Tokio, migrating off SurrealDB); or
  wiring a new binary's `main()`. Read [`docs/rust-programming.md`](../../../docs/rust-programming.md) before acting; it
  is authoritative.
---

# Rust guardrails

- **No `unwrap` or `panic!` outside `main` and tests.** Use `?` with `anyhow` (binaries) or `thiserror` (libraries);
  `expect("invariant: …")` only when a reader can verify the invariant in one line.
- **`unsafe_code = "forbid"`** in `[workspace.lints.rust]`; every member crate declares `[lints] workspace = true`.
- **One stack: Axum, Tokio, and the current store.** SurrealDB runs today; PostgreSQL replaces it (see
  [`postgres`](../postgres/SKILL.md)). Add no other store client, web framework, or async runtime; extend the existing
  router, entity, and runtime.
- **Target macOS and Linux only.** Add no `cfg(windows)`
  arm, `.exe` name, `%USERPROFILE%` fallback, or Windows CI or release job; `cfg(unix)` covers every host we ship.
- **Use `portal::hosting::shutdown_signal`** (SIGTERM + SIGINT); no inline `ctrl_c().await.unwrap()` in `main`.
- **Iterate on failure-only output.** When installed, run agent-facing `cargo build`, `check`, `clippy`, `test`, and
  `nextest` through RTK; it keeps only failures, warnings, and exit codes. Use plain Cargo for `cargo fmt`, coverage,
  project gates, `--message-format` consumers, and raw diagnostics. Run the `features` harness with `rtk cargo test -p
  features`.
- **Comments and tests describe the present.** No "we used to…" or "legacy" narration. Delete old paths, flags, and
  aliases; git holds the past. Keep only the *why* behind a live invariant or migration.
- **A removal removes the whole seam.** In the same change, delete the types, catalog and manifest entries, fixtures,
  CSS, tests, and docs that existed only for the removed caller, plus any enum variant or constant left with no member.
  Then `git grep -n '<removed-name>'` must return nothing but the guard test asserting it is gone.
- **Opportunistic file pass:** [`random-refactor`](../random-refactor/SKILL.md).

Everything else, including where we differ from Google's Rust Style Guide, is in
[`docs/rust-programming.md`](../../../docs/rust-programming.md).
