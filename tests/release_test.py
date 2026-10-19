"""Run: python3 tests/release_test.py. No network or Rust build."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap


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
    printf '%s\\n' "${TEST_BRANCH:-main}" ;;
*) exit 99 ;;
esac''',
    }
    for name, body in stubs.items():
        executable = commands / name
        executable.write_text("#!/bin/sh\nset -eu\n" + body + "\n")
        executable.chmod(0o755)
    curl = commands / "curl"
    curl.write_text(f"#!{sys.executable}\n" + r'''import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
url = args[-1]
headers = [args[i + 1] for i, arg in enumerate(args) if arg == "--header"]
if "@-" in headers:
    assert sys.stdin.read() == "Authorization: Bearer test-token-never-print\n"
    assert not any("test-token-never-print" in arg for arg in args)
else:
    assert "Authorization: Bearer test-token-never-print" in headers
if "/commits/" in url:
    assert url == "https://api.github.com/repos/owner/repo/commits/0123456789012345678901234567890123456789"
    sys.exit(bool(os.getenv("TEST_API_FAIL")))
if url.endswith("/dispatches"):
    payload = json.loads(args[args.index("--data") + 1])
    Path("workflow-call").write_text(json.dumps({"url": url, "payload": payload}))
    sys.exit(bool(os.getenv("TEST_DISPATCH_FAIL")))
if "/git/ref/tags/" in url:
    assert url.endswith("/git/ref/tags/v0.2.3")
    print(os.getenv("TEST_TAG_STATUS", "404"), end="")
    sys.exit(0)
if url.endswith("/releases"):
    payload = json.loads(args[args.index("--data") + 1])
    assert payload["draft"] is True and payload["generate_release_notes"] is True
    assert payload["tag_name"] == "v0.2.3"
    assert payload["target_commitish"] == "0123456789012345678901234567890123456789"
    Path("created").touch()
    if os.getenv("TEST_CREATE_FAIL"):
        sys.exit(22)
    print(json.dumps({"id": 123, "upload_url": "https://uploads.github.com/repos/owner/repo/releases/123/assets{?name,label}"}))
    sys.exit(0)
if url.startswith("https://uploads.github.com/"):
    from urllib.parse import parse_qs, urlsplit
    asset = Path(args[args.index("--data-binary") + 1][1:])
    assert parse_qs(urlsplit(url).query)["name"] == [asset.name]
    assert asset.read_bytes()
    if os.getenv("TEST_UPLOAD_FAIL"):
        sys.exit(22)
    with Path("uploaded").open("a") as uploaded:
        uploaded.write(asset.name + "\n")
    sys.exit(0)
if url.endswith("/releases/123"):
    assert args[args.index("--request") + 1] == "PATCH"
    assert json.loads(args[args.index("--data") + 1]) == {"draft": False}
    Path("published").touch()
    sys.exit(0)
raise AssertionError(url)
''')
    curl.chmod(0o755)
    environment = {**os.environ, "PATH": f"{commands}:{os.environ['PATH']}"}

    def run(**overrides):
        result = subprocess.run([str(root / "release.sh")], cwd="/",
                                env={**environment, **overrides},
                                capture_output=True, text=True)
        assert token not in result.stdout + result.stderr
        return result

    result = run()
    assert result.returncode == 0, result.stderr
    call = root / "workflow-call"
    assert json.loads(call.read_text()) == {
        "url": "https://api.github.com/repos/owner/repo/actions/workflows/release.yml/dispatches",
        "payload": {"ref": "main", "inputs": {"commit": commit}},
    }
    branch = 'release/with-"quotes"'
    result = run(TEST_BRANCH=branch)
    assert result.returncode == 0, result.stderr
    assert json.loads(call.read_text())["payload"]["ref"] == branch
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
    workflow = (source / ".github/workflows/release.yml").read_text()
    assert "gh " not in workflow

    def workflow_step(name):
        section = workflow.split(f"      - name: {name}\n", 1)[1]
        lines = section.split("        run: |\n", 1)[1].splitlines()
        body = []
        for line in lines:
            if line.strip() and not line.startswith("          "):
                break
            body.append(line)
        return textwrap.dedent("\n".join(body))

    workflow_env = {**environment, "GITHUB_TOKEN": token, "GITHUB_REPOSITORY": "owner/repo",
                    "GITHUB_API_URL": "https://api.github.com", "TAG": "v0.2.3", "COMMIT": commit,
                    "GITHUB_OUTPUT": str(root / "outputs")}
    (root / "Cargo.toml").write_text('[package]\nversion = "0.2.3"\n')
    for status, success in (("404", True), ("200", False), ("401", False), ("500", False)):
        result = subprocess.run(["bash", "-eu", "-c", workflow_step("Read version and reject an existing tag")],
                                cwd=root, env={**workflow_env, "TEST_TAG_STATUS": status},
                                capture_output=True, text=True)
        assert (result.returncode == 0) == success, result.stderr

    dist = root / "dist"
    dist.mkdir()
    for arch in ("arm64", "x86_64"):
        archive = dist / f"EverythingMac-v0.2.3-macos-{arch}.zip"
        archive.write_bytes(b"test archive")
        archive.with_suffix(".zip.sha256").write_text(
            f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
    publish = workflow_step("Verify checksums and publish release")
    for failure in (None, "TEST_CREATE_FAIL", "TEST_UPLOAD_FAIL"):
        for marker in ("created", "uploaded", "published"):
            (dist / marker).unlink(missing_ok=True)
        result = subprocess.run(["bash", "-eu", "-c", publish], cwd=dist,
                                env={**workflow_env, **({failure: "1"} if failure else {})},
                                capture_output=True, text=True)
        assert (result.returncode == 0) == (failure is None), result.stderr
        assert (dist / "published").exists() == (failure is None)
        if failure is None:
            assert len((dist / "uploaded").read_text().splitlines()) == 4
    (dist / "created").unlink()
    archive.write_bytes(b"corrupted")
    result = subprocess.run(["bash", "-eu", "-c", publish], cwd=dist, env=workflow_env,
                            capture_output=True, text=True)
    assert result.returncode != 0
    assert not (dist / "created").exists()
    print("REST dispatch, token privacy, tag checks, checksums, draft uploads, and failures passed.")
