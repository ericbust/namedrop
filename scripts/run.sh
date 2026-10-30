#!/bin/sh
set -eu
namedrop_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
command=${1-}
# Herdr starts actions, popups and events as its own children, never under a
# pane's shell, and sets HERDR_PANE_ID for this exact invocation. The runtime
# refuses an inherited pane it cannot trace to the caller's shell, so pass
# Herdr's value as the explicit pane, including to activation's runtime call.
if [ "${NAMEDROP_HOST-}" = hc ] || { [ -z "${HERDR_SOCKET_PATH-}${HERDR_PANE_ID-}" ] && [ -n "${HC_TERMINAL_ID-}" ]; }; then
  if [ -x "$namedrop_root/bin/namedrop" ]; then
    exec "$namedrop_root/bin/namedrop" --host hc "$@"
  fi
  exec "$namedrop_root/target/aarch64-apple-darwin/release/namedrop" --host hc "$@"
fi
if [ -n "${HERDR_PANE_ID-}" ]; then
  set -- --herdr-pane "$HERDR_PANE_ID" "$@"
fi
if [ "$command" = activate ]; then
  exec python3 "$namedrop_root/scripts/runtime.py" "$namedrop_root" "$@"
fi
if [ -n "${HERDR_PLUGIN_STATE_DIR-}" ] && [ -x "$HERDR_PLUGIN_STATE_DIR/runtime/namedrop" ]; then
  exec "$HERDR_PLUGIN_STATE_DIR/runtime/namedrop" "$@"
fi
if [ -x "$namedrop_root/bin/namedrop" ]; then
  exec "$namedrop_root/bin/namedrop" "$@"
fi
exec "$namedrop_root/target/aarch64-apple-darwin/release/namedrop" "$@"
