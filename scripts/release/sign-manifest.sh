#!/usr/bin/env bash
# Writes the update manifest edownloader reads at launch (src/core/update.rs):
# the installer's address and its minisign signature, under a trusted comment
# naming the version and the file ("edownloader 0.2.0 <file>"), so that the
# manifest, which is not signed, cannot pass an installer off as another
# version.
#
#   sign-manifest.sh <packages dir> <base url> <manifest out>
#
# <base url> is where the installer is published: the GitHub release in
# release.yml, the R2 copy in .gitlab-ci.yml. Run by both.
#
# Environment:
#   UPDATE_KEY   the release key: the rsign secret key file, no password
#   VERSION      the version released, as in Cargo.toml
#   LINUX_OPTIONAL  set when the pipeline builds no AppImage (.gitlab-ci.yml):
#                the Linux platforms are then left out instead of failing
#
# Needs rsign (the rsign2 crate) and jq.
set -euo pipefail

dist="${1:?usage: sign-manifest.sh <packages dir> <base url> <manifest out>}"
base="${2:?usage: sign-manifest.sh <packages dir> <base url> <manifest out>}"
out="${3:?usage: sign-manifest.sh <packages dir> <base url> <manifest out>}"
: "${VERSION:?VERSION is required}"
if [ -z "${UPDATE_KEY:-}" ]; then
  echo "::error::EDOWNLOADER_UPDATE_KEY is required to publish a release" >&2
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
key="$work/update.key"
(umask 077 && printf '%s\n' "$UPDATE_KEY" >"$key")

manifest='{"platforms":{}}'

# sign <platform> <glob> [optional]: exactly one file must match, or none
# for an optional platform (a pipeline that builds only some of them).
sign() {
  local platform="$1" optional="${3:-}"
  # shellcheck disable=SC2086 # the glob is meant to expand
  set -- $2
  if [ -n "$optional" ] && [ $# -eq 1 ] && [ ! -e "$1" ]; then
    echo "no package for $platform: left out of the manifest"
    return
  fi
  if [ $# -ne 1 ] || [ ! -f "$1" ]; then
    echo "::error::expected one package for $platform" >&2
    exit 1
  fi
  local name
  name="$(basename "$1")"
  rsign sign -W -s "$key" -t "edownloader $VERSION $name" -x "$work/$name.minisig" "$1" </dev/null
  manifest="$(jq --arg p "$platform" --arg url "${base%/}/$name" --rawfile sig "$work/$name.minisig" \
    '.platforms[$p] = {url: $url, signature: $sig}' <<<"$manifest")"
}

sign windows-x86_64 "$dist/*_x64-setup.exe"
# Linux updates itself from the AppImage only: a .deb or .rpm is the package
# manager's to update, and those copies are only told a version is out.
sign linux-x86_64 "$dist/*_x86_64.AppImage" ${LINUX_OPTIONAL:+optional}
sign linux-aarch64 "$dist/*_aarch64.AppImage" ${LINUX_OPTIONAL:+optional}
jq --arg v "$VERSION" --arg date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  '.version = $v | .pub_date = $date' <<<"$manifest" >"$out"
cat "$out"
