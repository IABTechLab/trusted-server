#!/usr/bin/env bash
set -euo pipefail
umask 077

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
output_dir=${1:-"$script_dir/../.dev-mtls"}

mkdir -p -- "$output_dir"
CDPATH= cd -- "$output_dir"
shopt -s nullglob dotglob
files=(*)
if (( ${#files[@]} != 0 )); then
  printf 'Refusing to write to a non-empty directory: %s\n' "$PWD" >&2
  exit 1
fi

printf 'Development only. Do not use these certificates or keys in production.\n'

openssl genrsa -out ca.key 2048
openssl req -new -x509 -config /dev/null -key ca.key -out ca.pem -days 730 \
  -subj '/CN=PBS example development CA' \
  -addext 'basicConstraints = critical,CA:TRUE' \
  -addext 'keyUsage = critical,keyCertSign,cRLSign'

openssl genrsa -out client.key 2048
openssl req -new -config /dev/null -key client.key \
  -subj '/CN=PBS example development client' |
  openssl x509 -req -CA ca.pem -CAkey ca.key -out client.pem -days 365 \
    -extfile /dev/fd/3 3<<'EXTENSIONS'
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = clientAuth
EXTENSIONS

cp -- ca.pem ca-bundle.pem
printf 'Wrote development certificates to %s\n' "$PWD"
