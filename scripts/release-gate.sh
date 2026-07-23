#!/usr/bin/env bash
# release-gate.sh — decide whether the current tree warrants a release.
#
# Compares package.json's version to the latest v* tag and writes the decision
# to $GITHUB_OUTPUT (or stdout when run locally). The release workflow reads
# should_release/version/tag from here. Idempotent: a version whose tag already
# exists never re-releases, so re-running the pipeline on the same commit (or
# the gate's own [skip ci] sync commit slipping through) is a no-op.
set -euo pipefail

# Version is the single source of truth in package.json; sync-version.sh mirrors
# it into the Rust/Tauri manifests.
version="$(node -p "require('./package.json').version")"
tag="v${version}"

# Latest existing release tag by semantic order (empty on a fresh repo).
latest_tag="$(git tag -l 'v*' --sort=-v:refname | head -1 || true)"

out="${GITHUB_OUTPUT:-/dev/stdout}"

if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null 2>&1; then
  # The tag already exists: this version was already released. Do not repeat it.
  echo "Tag ${tag} already exists; not releasing (idempotency guard)."
  should_release=false
elif [ -z "$latest_tag" ]; then
  # No v* tags at all — bootstrap the very first release.
  echo "No v* tags found; bootstrapping first release ${tag}."
  should_release=true
else
  # Release only when package.json's version is strictly greater than the
  # latest tag's version. sort -V puts the higher version last.
  latest_version="${latest_tag#v}"
  highest="$(printf '%s\n%s\n' "$latest_version" "$version" | sort -V | tail -1)"
  if [ "$version" != "$latest_version" ] && [ "$highest" = "$version" ]; then
    echo "Version ${version} > latest tag ${latest_tag}; releasing ${tag}."
    should_release=true
  else
    echo "Version ${version} is not greater than latest tag ${latest_tag}; not releasing."
    should_release=false
  fi
fi

{
  echo "should_release=${should_release}"
  echo "version=${version}"
  echo "tag=${tag}"
} >>"$out"
