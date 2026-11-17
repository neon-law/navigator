#!/usr/bin/env python3
"""Select the workspace tests that do not touch SurrealDB, for the PR gate.

SurrealDB is being retired (ENG-1356..1358). The application may still talk to
it, but CI no longer tests it: the embedded engine costs about a second per
test, was ~96% of the suite's run time, and made the gate cost ~6x more under
SurrealDB 4. Every test defined in a file that starts a SurrealDB engine is left
out of the run. Every file that names SurrealDB at all (case-insensitive
`surreal` on a non-comment line) is left out of the coverage measurement, so the
floor measures the code CI still tests.

Writes three files into the directory given as the only argument:

  nextest-filter     a nextest filterset excluding those tests (`-E`)
  coverage-ignore    a regex for `cargo llvm-cov report --ignore-filename-regex`
  features-args      `cargo test -p features` targets that touch no SurrealDB

`store::surreal::test_support` refuses to start an engine under
`NAVIGATOR_FORBID_SURREAL=1`, so a test this selection misses fails loudly
instead of quietly bringing SurrealDB back into CI.

`--self-test` checks the module-path mapping against small fixtures.
"""

import pathlib
import re
import sys
import tomllib

SURREAL = re.compile(r"surreal", re.IGNORECASE)
# The engine-starting helpers (`cli/tests/embedded_surreal_ratchet.rs` counts
# the same ones), plus a bare `mem()` and a direct `mem://` connection.
ENGINE = re.compile(
    r"(mem_surreal|shared_surreal|embedded_surreal|server_surreal|unmigrated)\(|(?<![\w.:])mem\(\)|test_support::mem\(|\"mem://\""
)
PATH_ATTR = re.compile(r'#\[path\s*=\s*"([^"]+)"\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;')
SKIP_DIRS = {"target", "vendor", "node_modules"}


def code_lines(source: str):
    return [line for line in source.splitlines() if not line.lstrip().startswith("//")]


def names_surreal(source: str) -> bool:
    return any(SURREAL.search(line) for line in code_lines(source))


def starts_engine(source: str) -> bool:
    return any(ENGINE.search(line) for line in code_lines(source))


def module_path(src_relative: pathlib.PurePosixPath) -> str:
    """`a/b.rs` or `a/b/mod.rs` -> `a::b`; `lib.rs`/`main.rs` -> ``."""
    parts = list(src_relative.with_suffix("").parts)
    if parts and parts[-1] == "mod":
        parts.pop()
    if parts in (["lib"], ["main"]):
        return ""
    return "::".join(parts)


def path_attribute_modules(src: pathlib.Path) -> dict[pathlib.Path, str]:
    """Files mounted with `#[path = "..."] mod name;` -> their real module path."""
    mounted = {}
    for parent in src.rglob("*.rs"):
        text = parent.read_text(errors="replace")
        for target, name in PATH_ATTR.findall(text):
            parent_module = module_path(pathlib.PurePosixPath(parent.relative_to(src).as_posix()))
            # A `#[path]` is relative to the declaring file's directory for
            # mod.rs/lib.rs/main.rs, and to a same-named directory otherwise.
            base = parent.parent
            if parent.name not in ("mod.rs", "lib.rs", "main.rs"):
                base = parent.parent / parent.stem if (parent.parent / parent.stem).is_dir() else parent.parent
            resolved = (base / target).resolve()
            if not resolved.exists():
                resolved = (parent.parent / target).resolve()
            mounted[resolved] = f"{parent_module}::{name}" if parent_module else name
    return mounted


def test_targets(manifest: dict, crate: pathlib.Path) -> dict[pathlib.Path, str]:
    """Integration-test source file -> nextest binary name."""
    targets = {}
    for test in manifest.get("test", []):
        path = crate / test.get("path", f"tests/{test['name']}.rs")
        targets[path.resolve()] = test["name"]
    tests = crate / "tests"
    if manifest.get("package", {}).get("autotests", True) and tests.is_dir():
        for path in tests.glob("*.rs"):
            targets.setdefault(path.resolve(), path.stem)
        for main in tests.glob("*/main.rs"):
            targets.setdefault(main.resolve(), main.parent.name)
    return targets


def rust_files(crate: pathlib.Path):
    for path in crate.rglob("*.rs"):
        if not SKIP_DIRS.intersection(path.relative_to(crate).parts):
            yield path


def select(root: pathlib.Path):
    workspace = tomllib.loads((root / "Cargo.toml").read_text())
    exclusions, ignored, features = [], [], []
    for member in workspace["workspace"]["members"]:
        crate = root / member
        manifest = tomllib.loads((crate / "Cargo.toml").read_text())
        package = manifest["package"]["name"]
        targets = test_targets(manifest, crate)
        mounted = path_attribute_modules(crate / "src") if (crate / "src").is_dir() else {}
        surreal_targets = set()
        for path in sorted(rust_files(crate)):
            source = path.read_text(errors="replace")
            if not names_surreal(source):
                continue
            ignored.append(path.relative_to(root).as_posix())
            # A cucumber runner reaches the engine through a shared module
            # (`features::journey`, `features::portfolio_acceptance`), so
            # naming SurrealDB is enough there.
            if package != "features" and not starts_engine(source):
                continue
            resolved = path.resolve()
            relative = path.relative_to(crate)
            if relative.parts[0] == "src":
                if resolved in mounted:
                    module = mounted[resolved]
                else:
                    module = module_path(pathlib.PurePosixPath(*relative.parts[1:]))
                prefix = f"{module}::" if module else "tests::"
                if package != "features":
                    exclusions.append(f"(package({package}) & not kind(test) & test(/^{prefix}/))")
            elif resolved in targets:
                surreal_targets.add(targets[resolved])
            elif relative.parts[0] == "tests" and len(relative.parts) > 2:
                # A helper module under tests/<dir>/: the whole binary that
                # owns the directory, when there is one.
                owner = relative.parts[1]
                if owner in targets.values():
                    surreal_targets.add(owner)
        for name in sorted(surreal_targets):
            if package != "features":
                exclusions.append(f"(package({package}) & kind(test) & binary(={name}))")
        if package == "features":
            engine_modules = {
                path.stem
                for path in (crate / "src").glob("*.rs")
                if path.stem != "lib" and starts_engine(path.read_text(errors="replace"))
            }
            for path, name in targets.items():
                runner = path.read_text(errors="replace")
                # `features::app_state` builds its world on `shared_surreal()`.
                if "app_state" in runner or any(
                    f"features::{module}::" in runner for module in engine_modules
                ):
                    surreal_targets.add(name)
            features = sorted(set(targets.values()) - surreal_targets)
    nextest = "not (" + " | ".join(exclusions) + ")" if exclusions else "all()"
    coverage = "(" + "|".join(path.replace(".", r"\.") for path in ignored) + ")$"
    # The library's own unit tests start no engine (the guard keeps it that
    # way), plus the cucumber runners that reach none. No libtest arguments:
    # the runners are `harness = false`.
    args = ["--lib"] + [f"--test={name}" for name in features]
    return nextest, coverage, args, len(ignored)


def self_test():
    assert module_path(pathlib.PurePosixPath("lib.rs")) == ""
    assert module_path(pathlib.PurePosixPath("a/b.rs")) == "a::b"
    assert module_path(pathlib.PurePosixPath("a/b/mod.rs")) == "a::b"
    assert starts_engine("let db = store::surreal::test_support::mem().await;")
    assert starts_engine("let db = mem().await;")
    assert not starts_engine("fn f(_db: &store::surreal::SurrealDb) {}\nlet m = self.mem();")
    assert names_surreal("fn f(_db: &store::surreal::SurrealDb) {}")
    assert not names_surreal("// SurrealDB is retired\nlet x = 1;")
    assert PATH_ATTR.search('#[cfg(test)]\n#[path = "x_tests.rs"]\nmod tests;').groups() == ("x_tests.rs", "tests")
    print("surreal-free-tests self-test passed")


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return
    if len(sys.argv) != 2:
        sys.exit("usage: surreal-free-tests.py <output-dir> | --self-test")
    out = pathlib.Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    root = pathlib.Path(__file__).resolve().parent.parent
    nextest, coverage, args, files = select(root)
    (out / "nextest-filter").write_text(nextest)
    (out / "coverage-ignore").write_text(coverage)
    (out / "features-args").write_text("\n".join(args))
    print(f"{files} files that name SurrealDB left out of the coverage measurement")
    print(f"cargo test -p features {' '.join(args)}")


if __name__ == "__main__":
    main()
