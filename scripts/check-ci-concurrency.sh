#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/.." && pwd)
root=$repo_root
yq_bin=${YQ_BIN:-yq}

usage() {
  printf 'usage: check-ci-concurrency.sh [--root ROOT] [--yq PATH]\n' >&2
}

while (($#)); do
  case "$1" in
    --root)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      root=$2
      shift 2
      ;;
    --yq)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      yq_bin=$2
      shift 2
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

workflow_dir=$root/.github/workflows
reference=$workflow_dir/test.yml
if [[ ! -d "$workflow_dir" || ! -f "$reference" ]]; then
  printf 'error: expected workflow directory and test.yml under %s\n' "$root" >&2
  exit 1
fi
if ! command -v "$yq_bin" >/dev/null 2>&1; then
  printf 'error: yq v4 is required to check workflow policies\n' >&2
  exit 1
fi

workflow_prefix='${{ github.workflow }}'
expected_workflow_group="${workflow_prefix}-\${{ github.event.pull_request.number || github.run_id }}"
expected_author_suffix="\${{ github.event_name == 'pull_request' && github.event.pull_request.user.type != 'Bot' && github.event.pull_request.user.login || github.run_id }}"
matrix_prefix="\${{ matrix."
matrix_suffix=' }}-'
matrix_key_pattern='^[[:alnum:]_.-]+$'

status=0

report_error() {
  printf '::error file=%s::%s\n' "$1" "$2"
  status=1
}

has_pull_request_trigger() {
  "$yq_bin" -r '
    .on
    | ((tag == "!!map" and has("pull_request"))
       or (tag == "!!str" and . == "pull_request")
       or (tag == "!!seq" and ([.[] | select(. == "pull_request")] | length > 0)))
  ' "$1"
}

job_value() {
  local file=$1
  local job=$2
  local expression=$3
  YQ_JOB=$job "$yq_bin" -r ".jobs[strenv(YQ_JOB)].${expression}" "$file"
}

validate_matrix_segment() {
  local remaining=$1
  local matrix_key

  while [[ "$remaining" == "$matrix_prefix"* ]]; do
    remaining=${remaining#"$matrix_prefix"}
    [[ "$remaining" == *"$matrix_suffix"* ]] || return 1
    matrix_key=${remaining%%"$matrix_suffix"*}
    [[ "$matrix_key" =~ $matrix_key_pattern ]] || return 1
    remaining=${remaining#*"$matrix_suffix"}
  done

  [[ -z "$remaining" ]]
}

validate_job_group() {
  local file=$1
  local job=$2
  local group=$3
  local has_matrix=$4
  local prefix="${workflow_prefix}-${job}-"
  local remainder
  local matrix_segment

  if [[ "$group" != "$prefix"* ]]; then
    report_error "$file" "Job '$job' concurrency group must start with the workflow name and its own job ID."
    return
  fi
  remainder=${group#"$prefix"}
  if [[ "$remainder" != *"$expected_author_suffix" ]]; then
    report_error "$file" "Job '$job' concurrency group must use the pull-request author for human PRs and the run ID otherwise."
    return
  fi
  matrix_segment=${remainder%"$expected_author_suffix"}

  if [[ "$has_matrix" == true ]]; then
    if [[ -z "$matrix_segment" ]] || ! validate_matrix_segment "$matrix_segment"; then
      report_error "$file" "Matrix job '$job' concurrency group must include each matrix leg before the per-author suffix."
    fi
  elif [[ -n "$matrix_segment" ]]; then
    report_error "$file" "Non-matrix job '$job' concurrency group must not add a shared queue segment."
  fi
}

check_workflow() {
  local file=$1
  local is_pr
  local group
  local cancel
  local queue
  local job
  local timeout
  local job_group
  local job_queue
  local has_matrix

  is_pr=$(has_pull_request_trigger "$file")
  [[ "$is_pr" == true ]] || return 0

  group=$("$yq_bin" -r '.concurrency.group // ""' "$file")
  cancel=$("$yq_bin" -r '.concurrency."cancel-in-progress" // false' "$file")
  queue=$("$yq_bin" -r '.concurrency.queue // ""' "$file")
  if [[ "$group" != "$expected_workflow_group" || "$cancel" != true || "$queue" == max ]]; then
    report_error "$file" "PR workflow concurrency group must be per-PR cancellation from test.yml, without queue: max."
  fi

  while IFS= read -r job; do
    [[ -n "$job" ]] || continue
    if [[ "$(job_value "$file" "$job" "uses")" != null ]]; then
      continue
    fi

    timeout=$(job_value "$file" "$job" '"timeout-minutes" // 0')
    if [[ ! "$timeout" =~ ^[1-9][0-9]*$ ]]; then
      report_error "$file" "Job '$job' must set a positive timeout-minutes value."
    fi

    job_group=$(job_value "$file" "$job" "concurrency.group")
    job_queue=$(job_value "$file" "$job" "concurrency.queue")
    if [[ "$job_queue" != max ]]; then
      report_error "$file" "Job '$job' must set queue: max on its per-author concurrency group."
    fi
    has_matrix=$(job_value "$file" "$job" "strategy.matrix != null")
    validate_job_group "$file" "$job" "$job_group" "$has_matrix"
  done < <("$yq_bin" -r '.jobs | to_entries[] | select(.value.uses == null) | .key' "$file")
}

while IFS= read -r -d '' workflow_file; do
  check_workflow "$workflow_file"
done < <(find "$workflow_dir" -type f \( -name '*.yml' -o -name '*.yaml' \) -print0)

exit "$status"
