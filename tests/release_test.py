"""Run on macOS: python3 tests/release_test.py. No network or Rust build."""
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import zipfile


source = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="everythingmac-release-") as temporary:
    root = Path(temporary)
    for name in ("release.sh", "build.sh", "THIRD_PARTY_NOTICES.md"):
        shutil.copy2(source / name, root / name)
    commands = root / "bin"
    commands.mkdir()
    stubs = {
        "uname": 'if [ "$1" = -s ]; then echo "${TEST_OS:-Darwin}"; else echo arm64; fi',
        "git": '''case "$1" in
status) printf '%s' "${TEST_DIRTY:-}" ;;
rev-parse) echo 0123456789012345678901234567890123456789 ;;
ls-remote) printf '%s' "${TEST_TAG:-}" ;;
*) exit 99 ;;
esac''',
        "rustup": '''case "$4" in
pkgid) echo 'path+file:///test#everythingmac@0.2.3' ;;
build)
    [ -z "${TEST_BUILD_FAIL:-}" ] || exit 1
    mkdir -p target/release
    cp /bin/echo target/release/everythingmac ;;
*) exit 99 ;;
esac''',
        "gh": '''case "$1 $2" in
"auth status") exit 0 ;;
"api "*) [ -z "${TEST_API_FAIL:-}" ] ;;
"release create")
    printf '%s\\n' "$@" > release-call
    shasum -a 256 -c "$5"
    [ -z "${TEST_UPLOAD_FAIL:-}" ] ;;
*) exit 99 ;;
esac''',
    }
    for name, body in stubs.items():
        path = commands / name
        path.write_text("#!/bin/sh\nset -eu\n" + body + "\n")
        path.chmod(0o755)
    environment = {**os.environ, "PATH": f"{commands}:{os.environ['PATH']}"}
    result = subprocess.run([str(root / "release.sh")], cwd="/", env=environment,
                            capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
    output = root / "target/release"
    archive = output / "EverythingMac-v0.2.3-macos-arm64.zip"
    with zipfile.ZipFile(archive) as package:
        plist = plistlib.loads(package.read("Everything Mac.app/Contents/Info.plist"))
        assert plist["CFBundleShortVersionString"] == "0.2.3"
        assert plist["CFBundleVersion"] == "0.2.3"
        assert package.read("Everything Mac.app/Contents/Resources/ThirdPartyNotices.md")
    call = (output / "release-call").read_text().splitlines()
    assert call[:4] == ["release", "create", "v0.2.3", archive.name]
    assert call[call.index("--target") + 1] == "0123456789012345678901234567890123456789"
    assert "--generate-notes" in call
    for variable in ("TEST_DIRTY", "TEST_TAG", "TEST_API_FAIL", "TEST_BUILD_FAIL",
                     "TEST_UPLOAD_FAIL", "TEST_OS"):
        (output / "release-call").unlink(missing_ok=True)
        result = subprocess.run([str(root / "release.sh")], env={**environment, variable: "1"},
                                capture_output=True, text=True)
        assert result.returncode != 0, variable
        assert (output / "release-call").exists() == (variable == "TEST_UPLOAD_FAIL"), variable
    print("Release packaging, version, checksum, commit, and failure checks passed.")
