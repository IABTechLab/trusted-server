#!/usr/bin/env bash
set -euo pipefail

npm --prefix crates/trusted-server-js/lib ci
npm --prefix crates/trusted-server-js/lib run build
