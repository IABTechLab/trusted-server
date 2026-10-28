#!/usr/bin/env bash
# Fail if a release WASM carries any permission policy file from
# config/permissions. A deployment's policy is the publisher's own file,
# supplied when the service is deployed, so no build carries one.
#
# The check looks for two pieces of text each policy file carries and no code
# does, being its first comment line and its `name:` value. Text shorter than
# MIN_MARKER_LENGTH characters, such as a bare `#`, could be in any WASM, so
# it is not looked for. The check reads the artifact itself, so it sees what
# the linker kept rather than what the source says. CI runs it on the Fastly
# and Spin release WASMs the test workflow builds, and on the Cloudflare
# release WASM the integration workflow builds with worker-build.
#
# Usage: scripts/check-no-compiled-policy.sh <release.wasm>...
set -euo pipefail

MIN_MARKER_LENGTH=16

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <release.wasm>..." >&2
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
shopt -s nullglob
policies=("$root"/config/permissions/*.yaml)
if [ "${#policies[@]}" -eq 0 ]; then
  echo "error: $root/config/permissions holds no policy file to look for" >&2
  exit 2
fi

status=0
for wasm in "$@"; do
  if [ ! -f "$wasm" ]; then
    echo "error: $wasm does not exist, so it was not checked" >&2
    status=1
    continue
  fi
  for policy in "${policies[@]}"; do
    file=${policy#"$root"/}
    first_comment=$(grep -m1 -E '^#' "$policy" || true)
    name=$(grep -m1 -E '^name:' "$policy" | sed -E 's/^name:[[:space:]]*//' || true)
    markers=()
    for marker in "$first_comment" "$name"; do
      if [ "${#marker}" -ge "$MIN_MARKER_LENGTH" ]; then
        markers+=("$marker")
      fi
    done
    if [ "${#markers[@]}" -eq 0 ]; then
      echo "error: $file has no comment line or name of $MIN_MARKER_LENGTH characters or more to look for" >&2
      status=1
      continue
    fi
    for marker in "${markers[@]}"; do
      found=0
      LC_ALL=C grep -a -q -F -- "$marker" "$wasm" || found=$?
      case "$found" in
        0)
          echo "error: $wasm carries text from $file: $marker" >&2
          status=1
          ;;
        1) ;;
        *)
          echo "error: $wasm could not be read" >&2
          status=1
          ;;
      esac
    done
  done
done

if [ "$status" -eq 0 ]; then
  echo "no permission policy is compiled into $*"
fi
exit "$status"
