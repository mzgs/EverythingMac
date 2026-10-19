#!/bin/sh
set -eu
cd "$(dirname "$0")"

if [ "$#" -ne 0 ]; then
    printf '%s\n' 'Usage: ./release.sh (uses the version in Cargo.toml)' >&2
    exit 1
fi
if [ "$(uname -s)" != Darwin ]; then
    printf '%s\n' 'release.sh requires macOS.' >&2
    exit 1
fi
command -v gh >/dev/null 2>&1 || {
    printf '%s\n' 'Install GitHub CLI (brew install gh), then run gh auth login.' >&2
    exit 1
}
if [ -n "$(git status --porcelain)" ]; then
    printf '%s\n' 'Commit or stash all changes before releasing.' >&2
    exit 1
fi
gh auth status
commit=$(git rev-parse HEAD)
gh api "repos/{owner}/{repo}/commits/$commit" --silent
package=$(rustup run stable cargo pkgid --locked)
version=${package##*@}
tag="v$version"
remote_tag=$(git ls-remote origin "refs/tags/$tag")
if [ -n "$remote_tag" ]; then
    printf 'Tag %s already exists; bump the version in Cargo.toml first.\n' "$tag" >&2
    exit 1
fi

./build.sh
archive="EverythingMac-$tag-macos-$(uname -m).zip"
cd target/release
ditto -c -k --sequesterRsrc --keepParent 'Everything Mac.app' "$archive"
shasum -a 256 "$archive" > "$archive.sha256"
gh release create "$tag" "$archive" "$archive.sha256" \
    --target "$commit" --title "Everything Mac $tag" --generate-notes \
    --notes 'Built for the architecture named in the ZIP. Ad-hoc signed; not Developer ID signed or notarized.'
