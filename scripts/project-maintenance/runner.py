#!/usr/bin/env python3
"""Apply a published Project release, or ask the isolated coding runner to repair it."""

import fcntl
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from datetime import date
from pathlib import Path
from urllib.parse import urlsplit
from uuid import UUID

sys.modules.setdefault("runner", sys.modules[__name__])


class Blocked(Exception):
    """The recorded source, credential, or operator configuration needs attention."""


class Repairable(Exception):
    """The deterministic release changes or repository gate failed."""


def run(args, cwd=None, *, timeout=900):
    result = subprocess.run(
        args,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=timeout,
        check=False,
        env={**os.environ, "GIT_TERMINAL_PROMPT": "0"},
    )
    if result.returncode:
        raise RuntimeError(result.stdout[-12000:] or f"{Path(args[0]).name} failed")
    return result.stdout.strip()


def release_date(tag):
    if not re.fullmatch(r"[0-9]{2}\.[1-9][0-9]?\.[1-9][0-9]?", tag):
        raise Blocked("invalid stable release")
    year, month, day = map(int, tag.split("."))
    return date(2000 + year, month, day)


def canonical(value):
    if value.startswith("git@github.com:"):
        value = "https://github.com/" + value[len("git@github.com:") :]
    elif value.startswith("ssh://git@ssh.github.com:443/"):
        value = "https://github.com/" + value[len("ssh://git@ssh.github.com:443/") :]
    elif value.startswith("ssh://git@github.com/"):
        value = "https://github.com/" + value[len("ssh://git@github.com/") :]
    parsed = urlsplit(value)
    path = parsed.path.rstrip("/").removesuffix(".git")
    if (
        parsed.scheme != "https"
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
        or not re.fullmatch(r"/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", path)
        or any(x in (".", "..") for x in path.split("/"))
    ):
        raise Blocked("invalid forge URL")
    return "https://" + parsed.netloc + path


def main_ref(url):
    fields = run(["git", "ls-remote", "--exit-code", url, "refs/heads/main"]).split()
    if (
        len(fields) != 2
        or fields[1] != "refs/heads/main"
        or not re.fullmatch("[a-f0-9]{40}", fields[0])
    ):
        raise Blocked("main ref unavailable")
    return fields[0]


def source_pin(path):
    text = path.read_text()
    match = re.search(
        r"^version:[ \t]*(['\"]?)(v?[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?)\1[ \t]*(?:#[^\n]*)?$",
        text,
        re.MULTILINE,
    )
    if not match:
        raise Blocked("stable manifest pin unavailable")
    return text, match


def exact_cli(release, cache, work):
    installed = shutil.which("navigator")
    if installed and run([installed, "--version"]) == "navigator " + release:
        return installed
    target = cache / release / "navigator"
    if not target.exists():
        arch = (platform.system(), platform.machine())
        systems = {("Darwin", "arm64"): "macos", ("Linux", "x86_64"): "linux"}
        if arch not in systems:
            raise Blocked("unsupported CLI platform")
        asset = f"navigator-{release}-{systems[arch]}.tar.gz"
        target.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=work) as dl:
            run(
                [
                    "gh",
                    "release",
                    "download",
                    release,
                    "--repo",
                    "neon-law/navigator",
                    "--dir",
                    dl,
                    "--pattern",
                    asset,
                    "--pattern",
                    asset + ".sha256",
                ]
            )
            archive = Path(dl) / asset
            digest = (Path(dl) / (asset + ".sha256")).read_text().split()[0]
            if hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
                raise Blocked("release checksum mismatch")
            with tarfile.open(archive, "r:gz") as tar:
                members = [
                    m for m in tar if m.isfile() and Path(m.name).name == "navigator"
                ]
                if len(members) != 1:
                    raise Blocked("unexpected release archive")
                with tempfile.NamedTemporaryFile(
                    dir=target.parent, delete=False
                ) as output:
                    shutil.copyfileobj(tar.extractfile(members[0]), output)
                    os.chmod(output.name, 0o700)
                os.replace(output.name, target)
    if run([str(target), "--version"]) != "navigator " + release:
        raise Blocked("release executable mismatch")
    return str(target)


def owned(path):
    return (
        path in ("navigator.yaml", "AGENTS.md")
        or path.startswith(
            (".agents/skills/", ".github/workflows/", ".forgejo/workflows/")
        )
        or Path(path).name in ("package.json", "pnpm-lock.yaml")
    )


def update_pins(repo, old, new):
    manifest = repo / "navigator.yaml"
    text, match = source_pin(manifest)
    manifest.write_text(text[: match.start(2)] + new + text[match.end(2) :])
    pattern = re.compile(
        r"(\bneon-law/navigator(?:/[\w./-]+)?@)" + re.escape(old) + r"(?![\w.-])"
    )
    for name in (".github", ".forgejo"):
        for path in (repo / name / "workflows").glob("*.y*ml"):
            if path.is_symlink():
                raise Blocked("workflow is a symlink")
            text = path.read_text()
            changed = pattern.sub(lambda m: m[1] + new, text)
            if changed != text:
                path.write_text(changed)
    package_pattern = re.compile(
        r'("@(?:neon-law|neonlaw)/navigator[^"]*"\s*:\s*"[^"0-9]*)'
        + re.escape(old)
        + r'(")'
    )
    lock_dirs = set()
    tracked = run(["git", "ls-files", "-z", "--", "*package.json"], repo)
    for name in filter(None, tracked.split("\0")):
        path = repo / name
        if path.is_symlink():
            raise Blocked("package path is a symlink")
        text = path.read_text()
        changed = package_pattern.sub(lambda m: m[1] + new + m[2], text)
        if changed != text:
            path.write_text(changed)
        if changed != text or run(["git", "diff", "HEAD", "--", name], repo):
            lock_dir = next(
                (
                    p
                    for p in [path.parent, *path.parent.parents]
                    if p.is_relative_to(repo) and (p / "pnpm-lock.yaml").is_file()
                ),
                None,
            )
            if lock_dir:
                lock_dirs.add(lock_dir)
    for lock in sorted(lock_dirs):
        run(
            [
                "pnpm",
                "install",
                "--lockfile-only",
                "--ignore-scripts",
                "--ignore-pnpmfile",
            ],
            lock,
        )


def tracked_changes(repo):
    tracked = run(["git", "diff", "HEAD", "--name-only", "-z"], repo).split("\0")
    untracked = run(
        ["git", "ls-files", "--others", "--exclude-standard", "-z"], repo
    ).split("\0")
    return set(filter(None, tracked + untracked))


def verify_private_pair(job, primary, mirror):
    import urllib.request

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None

    site = os.environ["NAVIGATOR_MAINTENANCE_SITE"]
    live = json.loads(
        run(["navigator", "project", "status", job["code"], "--host", site, "--json"])
    )
    if live.get("code") != job["code"] or live.get("project_status") not in (
        "open",
        "pitch",
    ):
        raise Blocked("Project is no longer eligible")
    remotes = live["repository_remotes"]
    if canonical(remotes["origin"]) != primary:
        raise Blocked("live primary changed")
    github, forgejo = (
        (primary, mirror)
        if urlsplit(primary).hostname == "github.com"
        else (mirror, primary)
    )
    if github == primary and (
        remotes.get("forgejo_status") != "verified"
        or canonical(remotes["forgejo"]) != forgejo
    ):
        raise Blocked("live mirror changed")
    if primary == forgejo:
        enrolled = json.loads(
            os.environ.get("NAVIGATOR_MAINTENANCE_GITHUB_MIRRORS", "{}")
        )
        if canonical(enrolled.get(job["project"], "")) != github:
            raise Blocked("GitHub mirror enrollment changed")
    ghpath = urlsplit(github).path.strip("/")
    gh = json.loads(run(["gh", "api", "repos/" + ghpath]))
    if (
        gh.get("full_name", "").lower() != ghpath.lower()
        or gh.get("private") is not True
        or gh.get("archived") is not False
    ):
        raise Blocked("GitHub repository is not an active private destination")
    path = urlsplit(forgejo).path.strip("/")
    root = os.environ["NAVIGATOR_FORGEJO_ROOT_URL"].rstrip("/")
    if canonical(root + "/" + path) != forgejo:
        raise Blocked("Forgejo root mismatch")
    req = urllib.request.Request(
        root + "/api/v1/repos/" + path,
        headers={"Authorization": "token " + os.environ["FORGEJO_TOKEN"]},
    )
    with urllib.request.build_opener(NoRedirect()).open(req, timeout=20) as resp:
        fj = json.load(resp)
    if (
        fj.get("full_name") != path
        or fj.get("private") is not True
        or fj.get("archived") is not False
    ):
        raise Blocked("Forgejo repository is not an active private destination")


def safe_tree(repo):
    for entry in run(["git", "ls-files", "-s", "-z"], repo).split("\0"):
        if entry.startswith("120000 "):
            path = repo / entry.split("\t", 1)[1]
            if not path.resolve().is_relative_to(repo.resolve()):
                raise Blocked("checkout contains an external symlink")


def checkout(job, repo, primary, mirror):
    branch = "maintenance/release-" + job["release"]
    if not repo.exists():
        run(["git", "clone", "--no-checkout", "--", primary, str(repo)])
        run(["git", "switch", "-c", branch, job["base"]], repo)
        secondary = (
            "forgejo" if urlsplit(primary).hostname == "github.com" else "github"
        )
        run(["git", "remote", "add", secondary, mirror], repo)
        if secondary == "forgejo":
            for dest in (primary, mirror):
                run(
                    ["git", "remote", "set-url", "--add", "--push", "origin", dest],
                    repo,
                )
    if canonical(run(["git", "remote", "get-url", "origin"], repo)) != primary:
        raise Blocked("existing worktree identity changed")
    if run(["git", "branch", "--show-current"], repo) != branch:
        raise Blocked("existing maintenance branch changed")
    return branch


def delivery_commit(output):
    # The landing CLI prints a JSON LaneReport after its progress messages.
    for line_start in reversed(
        [m.start() for m in re.finditer(r"^\{", output, re.MULTILINE)]
    ):
        try:
            report, _ = json.JSONDecoder().raw_decode(output[line_start:])
        except ValueError:
            continue
        merge = report.get("merge_commit", "")
        deployment = report.get("deployment") or {}
        if (
            re.fullmatch("[a-f0-9]{40}", merge)
            and deployment.get("commit") == merge
            and deployment.get("conclusion") == "success"
        ):
            return merge
    raise Blocked("delivery report has no successful deployment for the merge")


def finish_mirror(job, repo, primary, mirror, delivery):
    verify_private_pair(job, primary, mirror)
    if main_ref(primary) != delivery:
        raise Blocked("primary has advanced since the verified deployment")
    run(["git", "fetch", "--no-tags", primary, "refs/heads/main"], repo)
    if run(["git", "rev-parse", "FETCH_HEAD"], repo) != delivery:
        raise Blocked("primary moved during reconciliation")
    run(["git", "merge-base", "--is-ancestor", job["base"], delivery], repo)
    manifest = run(["git", "show", delivery + ":navigator.yaml"], repo)
    if not re.search(
        r"^version:[ \t]*['\"]?" + re.escape(job["release"]) + r"['\"]?[ \t]*(?:#.*)?$",
        manifest,
        re.MULTILINE,
    ):
        raise Blocked("merged manifest did not reach the queued release")
    observed = main_ref(mirror)
    if observed != delivery:
        if observed != job["base"]:
            raise Blocked("mirror changed since parity verification")
        run(["git", "push", "--porcelain", mirror, delivery + ":refs/heads/main"], repo)
    if main_ref(primary) != delivery or main_ref(mirror) != delivery:
        raise Blocked("delivery awaits mirror reconciliation")


def execute(job, mode):
    code = job["code"]
    if not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", code) or not re.fullmatch(
        "[0-9a-f]{40}", job["base"]
    ):
        raise Blocked("invalid Project coordinates")
    release_date(job["release"])
    primary, mirror = map(canonical, (job["primary"], job["mirror"]))
    forge = urlsplit(os.environ["NAVIGATOR_FORGEJO_ROOT_URL"]).netloc
    if {urlsplit(x).netloc for x in (primary, mirror)} != {"github.com", forge} or any(
        urlsplit(x).path.split("/")[-1] != code for x in (primary, mirror)
    ):
        raise Blocked("source and mirror bindings differ from the queued Project")
    root = Path(os.environ["NAVIGATOR_MAINTENANCE_WORKDIR"]).resolve()
    work = root / str(UUID(job["project"])) / job["release"] / job["base"]
    work.mkdir(mode=0o700, parents=True, exist_ok=True)
    with (work / "lock").open("a+") as lease:
        fcntl.flock(lease, fcntl.LOCK_EX)
        repo = work / "repo"
        receipt = work / "delivered"
        verify_private_pair(job, primary, mirror)
        verified_merge = work / "verified-merge"
        if verified_merge.exists():
            delivery = verified_merge.read_text().strip()
            finish_mirror(job, repo, primary, mirror, delivery)
            receipt.write_text(delivery)
            return
        observed = main_ref(primary)
        if observed != main_ref(mirror):
            raise Blocked("forge parity changed")
        if receipt.exists() and receipt.read_text().strip() == observed:
            return
        if observed != job["base"]:
            raise Blocked("primary moved; next daily check must use fresh evidence")
        published = json.loads(
            run(
                [
                    "gh",
                    "release",
                    "view",
                    job["release"],
                    "--repo",
                    "neon-law/navigator",
                    "--json",
                    "tagName,isDraft,isPrerelease,publishedAt,body",
                ]
            )
        )
        if (
            published["tagName"] != job["release"]
            or published["isDraft"]
            or published["isPrerelease"]
            or not published["publishedAt"]
        ):
            raise Blocked("queued release is not published and stable")
        (work / "release-notes").write_text(published["body"])
        cli = exact_cli(job["release"], root / ".releases", work)
        branch = checkout(job, repo, primary, mirror)
        safe_tree(repo)
        import yaml

        manifest = yaml.safe_load((repo / "navigator.yaml").read_text())
        if manifest["project"]["host"] != os.environ["NAVIGATOR_MAINTENANCE_SITE"]:
            raise Blocked("manifest host differs from worker site")
        baseline = work / "base-manifest"
        baseline.write_text(run(["git", "show", job["base"] + ":navigator.yaml"], repo))
        pin = source_pin(baseline)[1][2]
        if release_date(pin.removeprefix("v").split("-rc.")[0]) > release_date(
            job["release"]
        ):
            raise Blocked("release would downgrade the Project")
        try:
            if mode == "repair":
                repair_project(job, repo, work)
            update_pins(repo, pin, job["release"])
            run([cli, "project", "repository", "sync-skills"], repo)
            run([cli, "project", "gate", "--check"], repo)
        except RuntimeError as error:
            (work / "diagnostic").write_text(str(error))
            raise Repairable from error
        changes = tracked_changes(repo)
        from repair import approved_sources

        permitted = approved_sources(repo) if mode == "repair" else set()
        if any(not owned(path) and path not in permitted for path in changes):
            raise Blocked("gate produced a change outside release maintenance")
        verify_private_pair(job, primary, mirror)
        if main_ref(primary) != job["base"] or main_ref(mirror) != job["base"]:
            raise Blocked("base changed before review")
        output = run(
            [
                cli,
                "project",
                "repository",
                "push",
                "--branch",
                branch,
                "--title",
                "Update Navigator to " + job["release"],
                "--body",
                "Update the published Navigator release and verify the repository gate.",
            ],
            repo,
            timeout=5400,
        )
        delivery = delivery_commit(output)
        (work / "verified-merge").write_text(delivery)
        finish_mirror(job, repo, primary, mirror, delivery)
        receipt.write_text(delivery)


def repair_project(job, repo, work):
    from repair import propose_repair

    propose_repair(repo, work, job["release"])


if __name__ == "__main__":
    os.umask(0o077)
    try:
        (mode,) = sys.argv[1:]
        if mode not in ("update", "repair"):
            raise Blocked("unknown maintenance operation")
        data = json.load(sys.stdin)
        execute(data["job"] if mode == "repair" else data, mode)
    except (
        Blocked,
        RuntimeError,
        OSError,
        ValueError,
        KeyError,
        subprocess.TimeoutExpired,
    ):
        sys.exit(75)
    except Repairable:
        sys.exit(1)
