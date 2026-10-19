#!/usr/bin/env bash
# Select ordinary candidate jobs unless the comparison is known to be docs-only.
set -euo pipefail

select_candidates() {
  echo "$1"
  exit 0
}

if [[ $# != 1 || ! "$1" =~ ^[a-f0-9]{40}\.\.\.?[a-f0-9]{40}$ ]]; then
  select_candidates true
fi

changed_paths="$(mktemp)"
trap 'rm -f "$changed_paths"' EXIT
# Disable rename detection so moving source into docs still includes its deletion.
if ! git diff --name-only --no-renames -z "$1" -- > "$changed_paths"; then
  select_candidates true
fi

while IFS= read -r -d '' changed_path; do
  case "$changed_path" in
    docs/*.md | .claude/*.md | .github/*.md | README.md | CONTRIBUTING.md | CHANGELOG.md) ;;
    *) select_candidates true ;;
  esac
done < "$changed_paths"
select_candidates false
