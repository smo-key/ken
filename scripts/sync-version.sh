#!/usr/bin/env bash
# sync-version.sh — mirror package.json's version into the Rust/Tauri manifests.
#
# package.json is the source of truth (release-gate.sh reads it). This propagates
# a version into the two files that also carry it plus the lockfile, so the built
# app, its updater metadata, and cargo all agree.
#
# Usage: sync-version.sh <version>
set -euo pipefail

version="${1:?usage: sync-version.sh <version>}"

# --- src-tauri/tauri.conf.json -------------------------------------------------
# Targeted edit of the top-level `"version"` line only. A JSON round-trip
# (JSON.parse + JSON.stringify) would reformat unrelated content (e.g. expand
# inline arrays), so replace just that one line with sed and preserve the rest
# byte-for-byte. The file has a single 2-space-indented `"version"` key.
sed -i.bak -E "s/^(  \"version\": \")[^\"]*(\",)$/\1${version}\2/" src-tauri/tauri.conf.json
rm -f src-tauri/tauri.conf.json.bak
if ! grep -qE "^  \"version\": \"${version}\",$" src-tauri/tauri.conf.json; then
  echo "error: failed to set \"version\" in src-tauri/tauri.conf.json" >&2
  exit 1
fi
echo "synced src-tauri/tauri.conf.json -> ${version}"

# --- Cargo.toml [workspace.package] version -----------------------------------
# Ken's version lives in the workspace root Cargo.toml under [workspace.package]
# (line 6). Replace only the FIRST `^version = ` line — that one. awk (rather
# than sed's GNU-only `0,/re/` address) so it targets exactly the first match
# portably across GNU and BSD sed hosts.
awk -v v="$version" '
  !done && /^version = / { sub(/^version = .*/, "version = \"" v "\""); done=1 }
  { print }
' Cargo.toml >Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml
# Verify exactly the workspace version now reads the target value.
if ! grep -qE "^version = \"${version}\"$" Cargo.toml; then
  echo "error: failed to set [workspace.package] version in Cargo.toml" >&2
  exit 1
fi
echo "synced Cargo.toml [workspace.package] -> ${version}"

# --- Cargo.lock ----------------------------------------------------------------
# Refresh the workspace members' versions in the lockfile. --offline keeps this a
# pure dependency-graph update (no registry fetch, no build).
cargo update --workspace --offline
echo "synced Cargo.lock (workspace members) -> ${version}"
