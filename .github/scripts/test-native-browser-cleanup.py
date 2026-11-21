"""Owned shell-child regression; no Docker/KIND/network/Cargo execution."""
from pathlib import Path
import subprocess,tempfile,json
import sys
root=Path(__file__).parent
helper=Path(sys.argv[1]).resolve()
results=[]
for mode in ('secondary_failure', 'completed_teardown', 'unsupported_assignment'):
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
        script=task/'probe.sh';script.write_text('COPY_EXIT='+('73' if mode=='secondary_failure' else '0')+'\n'+prelude+functions+scenario)
        result=subprocess.run(['bash',str(script)],cwd=task,capture_output=True,timeout=10)
        public=task/'public';quick=public/'quickstart.log';cleanup=public/'cleanup.log'
        row={'mode':mode,'exit':result.returncode,'startup_exported':quick.exists(),'cleanup_exported':cleanup.exists(),'private_retained':(task/'private').exists(),'role_exported':(public/'role-private.log').exists()}
        assert result.returncode==1,row
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
