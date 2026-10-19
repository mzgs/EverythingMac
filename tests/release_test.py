"""Run: python3 tests/release_test.py. No network or Rust build."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


source = Path(__file__).resolve().parents[1]
commit = "0123456789012345678901234567890123456789"
token = "test-token-never-print"
with tempfile.TemporaryDirectory(prefix="everythingmac-release-") as temporary:
    root = Path(temporary)
    shutil.copy2(source / "release.sh", root / "release.sh")
    commands = root / "bin"
    commands.mkdir()
    stubs = {
        "git": '''case "$1" in
status) printf '%s' "${TEST_DIRTY:-}" ;;
config) printf '%s\\n' "${TEST_ORIGIN-https://user:test-token-never-print@github.com/owner/repo.git}" ;;
rev-parse) echo 0123456789012345678901234567890123456789 ;;
symbolic-ref)
    [ -z "${TEST_DETACHED:-}" ] || exit 1
    echo main ;;
*) exit 99 ;;
esac''',
        "gh": '''[ "$GH_TOKEN" = test-token-never-print ] || exit 98
[ "$GH_HOST" = github.com ] || exit 98
case "$1 $2" in
"api repos/owner/repo/commits/0123456789012345678901234567890123456789")
    [ -z "${TEST_API_FAIL:-}" ] ;;
"workflow run")
    printf '%s\\n' "$@" > workflow-call
    [ -z "${TEST_DISPATCH_FAIL:-}" ] ;;
*) exit 99 ;;
esac''',
    }
    for name, body in stubs.items():
        executable = commands / name
        executable.write_text("#!/bin/sh\nset -eu\n" + body + "\n")
        executable.chmod(0o755)
    environment = {**os.environ, "PATH": f"{commands}:{os.environ['PATH']}",
                   "GH_TOKEN": "unrelated-environment-token", "GH_HOST": "example.com"}

    def run(**overrides):
        result = subprocess.run([str(root / "release.sh")], cwd="/",
                                env={**environment, **overrides},
                                capture_output=True, text=True)
        assert token not in result.stdout + result.stderr
        return result

    result = run()
    assert result.returncode == 0, result.stderr
    call = root / "workflow-call"
    assert call.read_text().splitlines() == [
        "workflow", "run", "release.yml", "--repo", "owner/repo", "--ref", "main",
        "-f", f"commit={commit}",
    ]
    for variable in ("TEST_DIRTY", "TEST_DETACHED", "TEST_API_FAIL", "TEST_DISPATCH_FAIL"):
        call.unlink(missing_ok=True)
        result = run(**{variable: "1"})
        assert result.returncode != 0, variable
        assert call.exists() == (variable == "TEST_DISPATCH_FAIL"), variable
    for origin in ("https://github.com/owner/repo.git", "git@github.com:owner/repo.git",
                   "https://user:@github.com/owner/repo.git",
                   "https://user:secret@example.com/owner/repo.git"):
        call.unlink(missing_ok=True)
        result = run(TEST_ORIGIN=origin)
        assert result.returncode != 0, origin
        assert not call.exists(), origin
    result = subprocess.run([str(root / "release.sh"), "unexpected"], env=environment,
                            capture_output=True, text=True)
    assert result.returncode != 0
    print("Token authentication, workflow dispatch, exact commit, and failure checks passed.")
