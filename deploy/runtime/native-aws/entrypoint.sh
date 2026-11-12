#!/bin/sh
set -eu

# This input contains provider identifiers and references, never secret values.
if [ -z "${TS_NATIVE_BINDINGS_TOML:-}" ]; then
    printf '%s\n' 'TS_NATIVE_BINDINGS_TOML is required' >&2
    exit 1
fi

runtime_dir='/run/trusted-server'
if [ ! -d "$runtime_dir" ] || [ ! -w "$runtime_dir" ] || [ ! -x "$runtime_dir" ]; then
    printf '%s\n' 'Native bindings directory must be writable' >&2
    exit 1
fi

umask 077
temporary=$(mktemp "$runtime_dir/.native-bindings.XXXXXX")
trap 'rm -f "$temporary"' 0
trap 'exit 1' 1 2 15
# printf treats the entire input as data. Never source or evaluate these bytes.
printf '%s' "$TS_NATIVE_BINDINGS_TOML" > "$temporary"
chmod 0600 "$temporary"
mv -fT -- "$temporary" "$runtime_dir/native-bindings.toml"
trap - 0 1 2 15

# An absent profile keeps Geo Off. An explicitly empty profile is an error,
# not an implicit fallback. The app owns typed parsing and provider selection.
if [ "${TS_NATIVE_GEO_PROFILE_TOML+x}" = x ]; then
    if [ -z "$TS_NATIVE_GEO_PROFILE_TOML" ]; then
        printf '%s\n' 'TS_NATIVE_GEO_PROFILE_TOML must not be empty' >&2
        exit 1
    fi
    temporary=$(mktemp "$runtime_dir/.native-geo.XXXXXX")
    trap 'rm -f "$temporary"' 0
    trap 'exit 1' 1 2 15
    printf '%s' "$TS_NATIVE_GEO_PROFILE_TOML" > "$temporary"
    chmod 0600 "$temporary"
    mv -fT -- "$temporary" "$runtime_dir/native-geo.toml"
    trap - 0 1 2 15
    TS_NATIVE_GEO_PROFILE_FILE="$runtime_dir/native-geo.toml"
    export TS_NATIVE_GEO_PROFILE_FILE
else
    # Do not let an inherited pointer or a previous volume file select Geo.
    unset TS_NATIVE_GEO_PROFILE_FILE
fi

# The base image's local roots need not exist when both stores are remote.
unset EDGEZERO__ADAPTER__CONFIG_DIR EDGEZERO__ADAPTER__DATA_DIR
unset TS_NATIVE_BINDINGS_TOML TS_NATIVE_GEO_PROFILE_TOML
exec /usr/local/bin/app "$@"
