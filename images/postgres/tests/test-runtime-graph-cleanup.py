"""Exercise the real cleanup function against a recording Docker double; no services."""

import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
SOURCE = (ROOT / "images/postgres/tests/runtime-graph-roles.sh").read_text()
CLEANUP = SOURCE[SOURCE.index("cleanup() {"):SOURCE.index("trap cleanup EXIT")]
CONTAINER = "a" * 64
IMAGE = "sha256:" + "b" * 64
MOUNTS = [
    {"Type": "bind", "Source": "/synthetic/repository", "Destination": "/probe",
     "Mode": "ro", "RW": False, "Propagation": "rprivate"},
    {"Type": "bind", "Source": "/synthetic/secrets", "Destination": "/etc/navigator/postgres",
     "Mode": "ro", "RW": False, "Propagation": "rprivate"},
]
DUPLICATE_KEY = '[{"Type":"bind","Destination":"/probe","RW":false,"RW":true}]'
STUB = r'''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
args=sys.argv[1:]
with Path(os.environ['STUB_CALLS']).open('a') as calls:
    calls.write(json.dumps(args)+'\n')
if args[0]=='inspect':
    field=args[2]
    if os.environ.get('FAIL_INSPECT') in ('all',field): sys.exit(23)
    values={'{{.Id}}':'ACTUAL_ID','{{index .Config.Labels "codex.operation"}}':'ACTUAL_OPERATION',
      '{{index .Config.Labels "codex.task"}}':'ACTUAL_TASK','{{.Image}}':'ACTUAL_IMAGE',
      '{{json .Mounts}}':'ACTUAL_MOUNTS'}
    print(os.environ[values[field]])
elif args[0]=='logs': print('synthetic PostgreSQL evidence')
elif args[0]=='rm': pass
else: sys.exit(99)
'''


def changed(key, value):
    result = copy.deepcopy(MOUNTS)
    result[0][key] = value
    return json.dumps(result)


def run_case(name, *, current=None, expected=None, updates=None, remove=False):
    with tempfile.TemporaryDirectory(prefix="role-cleanup-regression-") as directory:
        scratch = Path(directory)
        docker = scratch / "docker"
        docker.write_text(STUB)
        docker.chmod(0o700)
        script = scratch / "probe.sh"
        script.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + CLEANUP
                          + "\ntrap cleanup EXIT\nexit 0\n")
        env = dict(os.environ, PATH=str(scratch) + os.pathsep + os.environ["PATH"],
                   root=str(ROOT), work=str(scratch), nonce="synthetic-operation",
                   container=CONTAINER, image_id=IMAGE, owned_mounts=expected or json.dumps(MOUNTS),
                   ACTUAL_ID=CONTAINER, ACTUAL_OPERATION="synthetic-operation",
                   ACTUAL_TASK="runtime-graph-roles", ACTUAL_IMAGE=IMAGE,
                   ACTUAL_MOUNTS=current if current is not None else json.dumps(MOUNTS),
                   STUB_CALLS=str(scratch / "calls.jsonl"))
        env.update(updates or {})
        result = subprocess.run(["bash", str(script)], env=env, capture_output=True, text=True,
                                timeout=10)
        calls = [json.loads(row) for row in (scratch / "calls.jsonl").read_text().splitlines()]
        removed = [call for call in calls if call[0] == "rm"]
        assert removed == ([["rm", "-f", "-v", CONTAINER]] if remove else []), (name, removed)
        assert result.returncode == (0 if remove else 1), (name, result.returncode)
        assert all(call[-1] == env["container"] for call in calls), (name, calls)
        if not remove:
            assert not any(call[0] == "logs" for call in calls), (name, calls)
            assert "Ownership/volume uncertainty" in result.stderr, (name, result.stderr)
        if name in ("foreign_id", "invalid_captured_id"):
            assert "id=false" in result.stderr, name
        if name == "foreign_operation":
            assert "operation=false" in result.stderr, name
        if name == "foreign_task":
            assert "task=false" in result.stderr, name
        if name == "foreign_image":
            assert "image=false" in result.stderr, name
        if name == "changed_source":
            assert "mounts=false" in result.stderr, name
        assert "/synthetic/secrets" not in result.stderr, name
        return name


cases = [
    run_case("unchanged", remove=True),
    run_case("reordered_mounts_and_keys", current=json.dumps([
        dict(reversed(list(mount.items()))) for mount in reversed(MOUNTS)]), remove=True),
    run_case("changed_source", current=changed("Source", "/foreign/source")),
    run_case("changed_access", current=changed("RW", True)),
    run_case("changed_propagation", current=changed("Propagation", "shared")),
    run_case("changed_unknown_field", current=changed("FutureField", {"strict": True})),
    run_case("extra_mount", current=json.dumps(MOUNTS + [
        {"Type": "tmpfs", "Destination": "/extra", "RW": True}])),
    run_case("missing_mount", current=json.dumps(MOUNTS[:1])),
    run_case("duplicate_mount", current=json.dumps(MOUNTS + [MOUNTS[0]])),
    run_case("duplicate_expected_identity", expected=json.dumps(MOUNTS + [MOUNTS[0]]),
             current=json.dumps(MOUNTS + [MOUNTS[0]])),
    run_case("malformed_mounts", current="not-json"),
    run_case("malformed_expected", expected="not-json"),
    run_case("wrong_shape", current='{}', expected='{}'),
    run_case("invalid_access_type", current=changed("RW", 0), expected=changed("RW", 0)),
    run_case("duplicate_object_key", current=DUPLICATE_KEY, expected=DUPLICATE_KEY),
    run_case("empty_mount_set", current="[]", expected="[]"),
    run_case("invalid_source_type", current=changed("Source", None), expected=changed("Source", None)),
    run_case("invalid_captured_id", updates={"container": "short-id", "ACTUAL_ID": "short-id"}),
    run_case("foreign_id", updates={"ACTUAL_ID": "c" * 64}),
    run_case("foreign_operation", updates={"ACTUAL_OPERATION": "foreign"}),
    run_case("foreign_task", updates={"ACTUAL_TASK": "foreign"}),
    run_case("foreign_image", updates={"ACTUAL_IMAGE": "sha256:" + "c" * 64}),
]
for field in ("all", "{{.Id}}", '{{index .Config.Labels "codex.operation"}}',
              '{{index .Config.Labels "codex.task"}}', "{{.Image}}", "{{json .Mounts}}"):
    cases.append(run_case("inspect_failure_" + field, updates={"FAIL_INSPECT": field}))
print(f"Runtime role cleanup: {len(cases)} recording-Docker scenarios passed")
