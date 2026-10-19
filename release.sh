#!/bin/sh
set -eu
cd "$(dirname "$0")"

if [ "$#" -ne 0 ]; then
    printf '%s\n' 'Usage: ./release.sh (uses the version in Cargo.toml)' >&2
    exit 1
fi
for dependency in curl python3; do
    command -v "$dependency" >/dev/null 2>&1 || {
        printf 'release.sh requires %s.\n' "$dependency" >&2
        exit 1
    }
done
if [ -n "$(git status --porcelain)" ]; then
    printf '%s\n' 'Commit or stash all changes before releasing.' >&2
    exit 1
fi
remote=$(git config --local --get remote.origin.url)
case "$remote" in
    https://*:*@github.com/*)
        credentials=${remote#https://}
        credentials=${credentials%%@*}
        token=${credentials#*:}
        repo=${remote##*@github.com/}
        repo=${repo%.git}
        ;;
    *)
        printf '%s\n' 'Expected a GitHub HTTPS origin with a token in .git/config.' >&2
        exit 1
        ;;
esac
if [ -z "$token" ]; then
    printf '%s\n' 'The GitHub token in .git/config is empty.' >&2
    exit 1
fi
commit=$(git rev-parse HEAD)
branch=$(git symbolic-ref --quiet --short HEAD) || {
    printf '%s\n' 'Check out a pushed branch before releasing.' >&2
    exit 1
}
printf 'Authorization: Bearer %s\n' "$token" | curl --fail --silent --show-error \
    --header @- --output /dev/null "https://api.github.com/repos/$repo/commits/$commit"
payload=$(python3 -c 'import json, sys; print(json.dumps({"ref": sys.argv[1], "inputs": {"commit": sys.argv[2]}}))' "$branch" "$commit")
printf 'Authorization: Bearer %s\n' "$token" | curl --fail --silent --show-error \
    --header @- --header 'Accept: application/vnd.github+json' \
    --header 'Content-Type: application/json' --header 'X-GitHub-Api-Version: 2026-03-10' \
    --data "$payload" --output /dev/null \
    "https://api.github.com/repos/$repo/actions/workflows/release.yml/dispatches"
printf 'Release workflow requested for %s. Follow it at https://github.com/%s/actions/workflows/release.yml\n' "$commit" "$repo"
