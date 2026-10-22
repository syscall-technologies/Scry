#!/usr/bin/env bash
# Phase 2 end-to-end check: drive a running `scry session` to open Notepad and type a line.
set -euo pipefail
API="${API:-http://127.0.0.1:7878}"

post() { curl -sf -X POST "$API/$1" -H 'Content-Type: application/json' -d "$2" >/dev/null; }

post key '{"combo":"win+r"}'
sleep 1.5
post type '{"text":"notepad\n"}'
sleep 3
post type '{"text":"Hello from scry-rework, typed by an agent.\n"}'
sleep 1
curl -sf "$API/screenshot" -o notepad-demo.png
echo "saved notepad-demo.png"
