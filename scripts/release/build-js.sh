#!/bin/sh
# An isolated interpreter prevents caller Python import/configuration injection.
set -eu
exec /usr/bin/python3 -I -B -c '
import runpy, sys
if sys.version_info < (3, 11):
    sys.exit("release qualification pending: authenticated Python 3.11+ required")
sys.argv = sys.argv[1:]
runpy.run_path(sys.argv[0], run_name="__main__")
' "${0%/*}/prepare.py" "$@"
