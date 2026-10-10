#!/bin/sh
# sh capture.sh <workspace-id> <out-dir> [seconds]
# Snapshot every pane in the workspace as ANSI every few seconds, as <epoch>-<pane>.ansi, until killed.
ws=$1; out=$2; every=${3:-2}
mkdir -p "$out"
while :; do
  ts=$(date +%s)
  herdr pane list --workspace "$ws" |
    python3 -c 'import json,sys; [print(p["pane_id"]) for p in json.load(sys.stdin)["result"]["panes"]]' |
    while read -r pane; do herdr pane read "$pane" --source visible --ansi > "$out/$ts-${pane#*:}.ansi"; done
  sleep "$every"
done
