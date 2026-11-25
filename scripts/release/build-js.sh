#!/bin/sh
# Use the same independently approved bootstrap boundary as the Rust recipe.
set -eu
exec /bin/sh "$(dirname "$0")/bootstrap.sh" js "$@"
