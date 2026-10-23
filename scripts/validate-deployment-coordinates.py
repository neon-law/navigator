#!/usr/bin/env python3
"""Check the source tree for values supplied by an external denylist.

The denylist is operator-owned and must live outside the repository. Matches
are reported as a count only: neither coordinates nor file paths are written
to logs.
"""

import argparse
import contextlib
import io
from pathlib import Path
import os
import subprocess
import sys
import tempfile
from unittest import mock


DENYLIST_ENV = "NAVIGATOR_DEPLOYMENT_COORDINATES_DENYLIST"


def load_values(path):
    try:
        values = [line.strip() for line in path.read_text(encoding="utf-8").splitlines()]
    except (OSError, UnicodeError) as error:
        raise ValueError("external denylist is unreadable or not UTF-8") from error
    return {value for value in values if value and not value.startswith("#")}


def source_files(root):
    """Yield paths in Git's local index, including staged additions and symlinks."""
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "ls-files", "--cached", "-z"],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise ValueError("tracked source inventory could not be read") from error
    for relative in result.stdout.split(b"\0"):
        if relative:
            yield root / os.fsdecode(relative)


def find_matches(root, values):
    matched_files = 0
    needles = [value.encode("utf-8") for value in values]
    for path in source_files(root):
        try:
            if path.is_symlink():
                content = os.readlink(path).encode("utf-8", errors="surrogateescape")
            else:
                content = path.read_bytes()
        except OSError as error:
            raise ValueError("a source file could not be read") from error
        if any(value in content for value in needles):
            matched_files += 1
    return matched_files


def validate(root, denylist):
    root = root.resolve()
    denylist = denylist.resolve()
    if not root.is_dir():
        raise ValueError("source root is missing or not a directory")
    if denylist == root or root in denylist.parents:
        raise ValueError("denylist must be stored outside the repository")
    if not denylist.is_file():
        raise ValueError("external denylist file is missing")
    values = load_values(denylist)
    if not values:
        raise ValueError("external denylist contains no coordinate values")
    return find_matches(root, values)


def self_test():
    with tempfile.TemporaryDirectory() as temp:
        base = Path(temp)
        root = base / "source"
        external = base / "private"
        root.mkdir()
        external.mkdir()
        subprocess.run(["git", "init", "--quiet", str(root)], check=True)
        denylist = external / "coordinates.txt"
        probe = "synthetic-coordinate-probe.invalid"
        denylist.write_text(probe + "\n", encoding="utf-8")
        ordinary = root / "ordinary.txt"
        ordinary.write_text("synthetic source\n", encoding="utf-8")
        ignored_private = root / "node_modules" / "private.txt"
        ignored_private.parent.mkdir()
        ignored_private.write_text(probe, encoding="utf-8")
        assert validate(root, denylist) == 0
        tracked_build = root / "target" / "planted.txt"
        tracked_build.parent.mkdir()
        tracked_build.write_text(probe + "\n", encoding="utf-8")
        (root / "link-text").symlink_to(probe)
        outside = external / "outside-secret.txt"
        outside.write_text(probe, encoding="utf-8")
        (root / "outside-link").symlink_to(os.path.relpath(outside, root))
        subprocess.run(["git", "-C", str(root), "add", "ordinary.txt", "target/planted.txt", "link-text", "outside-link"], check=True)
        assert validate(root, denylist) == 2
        (root / "binary.bin").write_bytes(b"\x00" + probe.encode("utf-8") + b"\xff")
        subprocess.run(["git", "-C", str(root), "add", "binary.bin"], check=True)
        assert validate(root, denylist) == 3
        assert all(path.name != "private.txt" for path in source_files(root))
        assert any(path == tracked_build for path in source_files(root))
        try:
            validate(root, root / "ordinary.txt")
        except ValueError:
            pass
        else:
            raise AssertionError("a repository-local denylist must be rejected")

        source = root / "unreadable.txt"
        source.write_text("source", encoding="utf-8")
        subprocess.run(["git", "-C", str(root), "add", "unreadable.txt"], check=True)
        original_read_bytes = Path.read_bytes

        def fail_source_read(path):
            if path.resolve() == source.resolve():
                raise OSError("private path must not be emitted")
            return original_read_bytes(path)

        with mock.patch.object(Path, "read_bytes", fail_source_read):
            try:
                validate(root, denylist)
            except ValueError as error:
                assert str(error) == "a source file could not be read"
            else:
                raise AssertionError("source read errors must fail closed")

        for invalid_root in (root / "missing", root / "ordinary.txt"):
            try:
                validate(invalid_root, denylist)
            except ValueError as error:
                assert str(error) == "source root is missing or not a directory"
            else:
                raise AssertionError("a missing or non-directory source root must fail")

        with mock.patch.object(
            subprocess,
            "run",
            side_effect=subprocess.CalledProcessError(128, "git ls-files"),
        ):
            try:
                validate(root, denylist)
            except ValueError as error:
                assert str(error) == "tracked source inventory could not be read"
            else:
                raise AssertionError("Git inventory errors must fail closed")

        stderr = io.StringIO()
        with mock.patch.dict(os.environ, {DENYLIST_ENV: str(external / "missing-private-name.txt")}), \
             mock.patch.object(sys, "argv", ["scanner", "--root", str(root)]), \
             contextlib.redirect_stderr(stderr):
            assert main() == 2
        assert "missing-private-name.txt" not in stderr.getvalue()

        stderr = io.StringIO()
        invalid_root = base / "source-private-root"
        with mock.patch.dict(os.environ, {DENYLIST_ENV: str(denylist)}), \
             mock.patch.object(sys, "argv", ["scanner", "--root", str(invalid_root)]), \
             contextlib.redirect_stderr(stderr):
            assert main() == 2
        assert "source-private-root" not in stderr.getvalue()

        stderr = io.StringIO()
        with mock.patch.dict(os.environ, {DENYLIST_ENV: str(denylist)}), \
             mock.patch.object(sys, "argv", ["scanner", "--root", str(root)]), \
             mock.patch.object(
                 subprocess,
                 "run",
                 side_effect=subprocess.CalledProcessError(128, "git ls-files"),
             ), \
             contextlib.redirect_stderr(stderr):
            assert main() == 2
        assert "git ls-files" not in stderr.getvalue()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        print("deployment-coordinate scanner self-test passed")
        return 0
    configured = os.environ.get(DENYLIST_ENV)
    if not configured:
        print("set NAVIGATOR_DEPLOYMENT_COORDINATES_DENYLIST to an external file", file=sys.stderr)
        return 2
    try:
        matches = validate(args.root, Path(configured))
    except (OSError, ValueError):
        print("deployment-coordinate scan could not run (invalid input or source read failure)", file=sys.stderr)
        return 2
    if matches:
        print(f"deployment-coordinate denylist matched {matches} repository file(s)", file=sys.stderr)
        return 1
    print("deployment-coordinate denylist scan passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
