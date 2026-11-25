#!/bin/sh
# Independent native/bootstrap approval precedes release-tool execution.
set -eu
exec /bin/sh "$(dirname "$0")/bootstrap.sh" rust "$@"
