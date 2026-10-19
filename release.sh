#!/bin/sh
set -eu
cd "$(dirname "$0")"

if [ "$#" -ne 0 ]; then
    printf '%s\n' 'Usage: ./release.sh (uses the version in Cargo.toml)' >&2
    exit 1
fi
command -v gh >/dev/null 2>&1 || {
    printf '%s\n' 'Install GitHub CLI: https://cli.github.com/.' >&2
    exit 1
}
if [ -n "$(git status --porcelain)" ]; then
    printf '%s\n' 'Commit or stash all changes before releasing.' >&2
    exit 1
fi
remote=$(git config --local --get remote.origin.url)
case "$remote" in
    https://*:*@github.com/*)
        credentials=${remote#https://}
        credentials=${credentials%%@*}
        GH_TOKEN=${credentials#*:}
        repo=${remote##*@github.com/}
        repo=${repo%.git}
        ;;
    *)
        printf '%s\n' 'Expected a GitHub HTTPS origin with a token in .git/config.' >&2
        exit 1
        ;;
esac
if [ -z "$GH_TOKEN" ]; then
    printf '%s\n' 'The GitHub token in .git/config is empty.' >&2
    exit 1
fi
GH_HOST=github.com
export GH_TOKEN GH_HOST
commit=$(git rev-parse HEAD)
branch=$(git symbolic-ref --quiet --short HEAD) || {
    printf '%s\n' 'Check out a pushed branch before releasing.' >&2
    exit 1
}
gh api "repos/$repo/commits/$commit" --silent
gh workflow run release.yml --repo "$repo" --ref "$branch" -f "commit=$commit"
printf 'Release workflow requested for %s. Follow it at https://github.com/%s/actions/workflows/release.yml\n' "$commit" "$repo"
