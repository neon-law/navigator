"""Owned shell-child regression; no Docker/KIND/network/Cargo execution."""
from pathlib import Path
import subprocess,tempfile,json,re
import sys
root=Path(__file__).parent
helper=Path(sys.argv[1]).resolve()
results=[]
for mode in ('secondary_failure', 'completed_teardown', 'unsupported_assignment',
             'image_secondary_failure', 'unowned_image_secondary_failure',
             'success_secondary_failure', 'failed_browser_teardown'):
    source=helper.read_text()
    functions=source[source.index('resources() {'):source.index('trap cleanup EXIT')]
    with tempfile.TemporaryDirectory(prefix='native-cleanup-regression-') as directory:
        task=Path(directory);(task/'.devx').mkdir();(task/'private').mkdir();(task/'public').mkdir()
        # Leading whitespace is valid shell assignment syntax, not permission to skip a secret.
        (task/'.devx/env').write_text("  NAVIGATOR_SYNTHETIC_TOKEN='synthetic-secret-value'\n")
        if mode == 'unsupported_assignment':
            (task/'.devx/env').write_text("  export NAVIGATOR_SYNTHETIC_TOKEN = 'synthetic-secret-value'\n")
        hudson = task/'target/debug/hudson'
        hudson.parent.mkdir(parents=True)
        hudson.write_text('#!/bin/sh\nrm -- .devx/env\nprintf "synthetic teardown complete\\n"\n')
        hudson.chmod(0o700)
        (task/'.devx/port-forwards.log').write_text('synthetic forward status')
        (task/'private/role-private.log').write_text('role-private-marker\n')
        prelude='''set -Eeuo pipefail
root="$PWD"; private="$PWD/private"; public="$PWD/public"
supervisor=''; host_pid=''; fixture_started=false; image_started=false
image_container_id=''; volume_created=false; volume_initializer=''; browser_supervisor=''
volume_operation=synthetic; repo_volume=synthetic; startup_started=true
cleanup_failed=false; groups_drained=true; cleanup_complete=false
role_pending="$private/role-pending.json"; fixture=synthetic
kubectl() { return 0; }
cp() { return "$COPY_EXIT"; }
'''
        scenario='''
trap cleanup EXIT
bash -c 'printf "%s\\n" "synthetic startup failure" "synthetic-secret-value" "postgresql://untracked-user:untracked-password@localhost/synthetic"; exit 23' > "$private/quickstart.log" 2>&1 &
supervisor=$!
wait "$supervisor" || true
startup_alive
'''
        if mode in ('image_secondary_failure', 'unowned_image_secondary_failure',
             'success_secondary_failure'):
            prelude += "\nimage_started=true; image_container_id=synthetic-owned-image\n"
            prelude += "IMAGE_OWNER=" + ('foreign' if mode.startswith('unowned') else 'synthetic') + "\n"
            prelude += r'''docker() {
  printf '%s\n' "$1" >> "$private/docker.calls"
  case "$1" in
    inspect)
      if [[ "$*" == *browser-fixture* ]]; then printf '%s\n' git-runtime
      else printf '%s\n' "$IMAGE_OWNER"; fi ;;
    logs) printf '%s\n' 'synthetic image failure' 'synthetic-secret-value' ;;
    *) return 98 ;;
  esac
}
'''
            # Force a failure BEFORE the original image-log producer, after early export.
            scenario = "stop_browser() { return 73; }\n" + scenario
        if mode == 'success_secondary_failure':
            scenario = "trap cleanup EXIT\nprintf 'synthetic healthy startup\\n' > \"$private/quickstart.log\"\nexit 0\n"
        if mode == 'failed_browser_teardown':
            scenario = "trap cleanup EXIT\nprintf 'synthetic failed browser\\n' > \"$private/quickstart.log\"\nexit 1\n"
        script=task/'probe.sh';script.write_text('COPY_EXIT='+('73' if mode in ('secondary_failure', 'success_secondary_failure') else '0')+'\n'+prelude+functions+scenario)
        result=subprocess.run(['bash',str(script)],cwd=task,capture_output=True,timeout=10)
        public=task/'public';quick=public/'quickstart.log';cleanup=public/'cleanup.log'
        row={'mode':mode,'exit':result.returncode,'startup_exported':quick.exists(),'cleanup_exported':cleanup.exists(),'private_retained':(task/'private').exists(),'role_exported':(public/'role-private.log').exists()}
        assert result.returncode==1,row
        if cleanup.exists() and 'Cleanup interrupted' in cleanup.read_text():
            assert re.search(r'^failure_line=[1-9][0-9]*$', cleanup.read_text(), re.MULTILINE), row
        if mode == 'failed_browser_teardown':
            assert not row['private_retained'], row
            assert not (task/'.devx/env').exists(), row
            results.append(row)
            continue
        if mode == 'success_secondary_failure':
            assert cleanup.exists() and row['private_retained'] and not row['role_exported'], row
            text = cleanup.read_text()
            assert 'primary_exit=0' in text and 'secondary_exit=73' in text, text
            assert 'last_cleanup_phase=copy_forward_logs' in text, text
            results.append(row)
            continue
        if mode in ('image_secondary_failure', 'unowned_image_secondary_failure',
             'success_secondary_failure'):
            image = public/'image.log'
            calls_file = task/'private/docker.calls'
            calls = calls_file.read_text().splitlines() if calls_file.exists() else []
            assert row['private_retained'] and cleanup.exists(), row
            assert 'primary_exit=1' in cleanup.read_text() and 'secondary_exit=73' in cleanup.read_text()
            assert 'last_cleanup_phase=drain_browser' in cleanup.read_text()
            assert not row['role_exported'], row
            if mode == 'image_secondary_failure':
                assert image.exists() and 'synthetic image failure' in image.read_text()
                assert 'synthetic-secret-value' not in image.read_text() and '[REDACTED]' in image.read_text()
                assert calls == ['inspect', 'inspect', 'logs'], calls
            else:
                assert not image.exists(), 'foreign image output must never be read or exported'
                assert calls == ['inspect'], calls
            row['image_exported'] = image.exists()
            results.append(row)
            continue
        if mode == 'unsupported_assignment':
            assert not quick.exists(), 'unsupported discovery cannot publish after env deletion'
            assert not (task/'.devx/env').exists(), 'normal teardown must have removed env'
            assert row['private_retained'], 'failed discovery preserves private evidence'
            assert not row['role_exported'],row
            results.append(row)
            continue
        assert quick.exists() and 'synthetic startup failure' in quick.read_text(),row
        assert 'child_exit=23' in quick.read_text(),row
        assert 'synthetic-secret-value' not in quick.read_text(),row
        assert '[REDACTED]' in quick.read_text(),row
        assert 'untracked-user' not in quick.read_text() and 'untracked-password' not in quick.read_text(),row
        assert '[USERINFO REDACTED]' in quick.read_text(),row
        assert not row['role_exported'],row
        if mode == 'secondary_failure':
            assert cleanup.exists() and 'primary_exit=1' in cleanup.read_text() and 'secondary_exit=73' in cleanup.read_text(),row
            assert 'last_cleanup_phase=copy_forward_logs' in cleanup.read_text()
            assert row['private_retained'],row
            cache = task/'private/.redaction-secrets.json'
            assert cache.stat().st_mode & 0o777 == 0o600
            assert 'synthetic-secret-value' in json.loads(cache.read_text())
        else:
            assert not (task/'.devx/env').exists(), 'fake owned Hudson must really remove generated env'
            assert not row['private_retained'], 'completed cleanup must remove owned private supports'
        assert not (public/'.redaction-secrets.json').exists(), 'private cache must never be exported'
        results.append(row)

print(json.dumps(results))
