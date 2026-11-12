#!/usr/bin/env bash

# Cut a release: tag origin/main and push the tag, which starts
# .github/workflows/release.yml (builds the macOS .dmgs, then publishes the GitHub Release).
#
#   ./scripts/release.sh           # next patch after the newest tag on origin
#   ./scripts/release.sh v0.2.0    # an explicit version
#
# Refuses unless the checkout is clean and HEAD is origin/main, so the tests
# run here are the tests of the tagged commit. A local tag of the same name
# that never reached origin is moved to HEAD; one already on origin is an
# error, since the release for it has been published.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

latest="$(git ls-remote --tags --refs origin 'v*' \
  | sed 's#.*refs/tags/##' | sort -V | tail -1)"

if [[ $# -ge 1 ]]; then
  version="$1"
elif [[ -n "$latest" ]]; then
  IFS=. read -r major minor patch <<<"${latest#v}"
  version="v$major.$minor.$((patch + 1))"
else
  echo "error: origin has no v* tags; pass a version, e.g. $0 v0.1.0" >&2
  exit 1
fi

if [[ ! "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: version must look like v1.2.3, got '$version'" >&2
  exit 1
fi

if git ls-remote --exit-code --tags origin "refs/tags/$version" >/dev/null; then
  echo "error: $version is already on origin" >&2
  exit 1
fi

echo "==> Checking HEAD is a clean origin/main"
git fetch --quiet origin main
if ! git diff --quiet HEAD; then
  echo "error: uncommitted changes; commit or stash them first" >&2
  exit 1
fi
if [[ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]]; then
  echo "error: HEAD is not origin/main; check out main and pull first" >&2
  exit 1
fi

echo "==> Running tests"
cargo test --release

echo "==> Tagging $(git rev-parse --short HEAD) as $version (previous: ${latest:-none})"
git tag -f "$version" HEAD
git push origin "refs/tags/$version"

echo "==> Pushed $version. CI stamps the .app with ${version#v}. Follow the build with:"
echo "    gh run watch \$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
