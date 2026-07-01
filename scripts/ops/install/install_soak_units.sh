#!/usr/bin/env bash
# Install/refresh systemd user units for the paper soak stack and enable
# on-boot start. Replaces the fragile nohup launches for shadow-final and the
# paper executor. Safe to re-run.
set -euo pipefail

REPO="${REPO:-$HOME/pm-backtest}"
UNIT_DIR="$HOME/.config/systemd/user"
mkdir -p "$UNIT_DIR"

cp "$REPO/scripts/ops/systemd/pm-shadow-final.service" "$UNIT_DIR/"
cp "$REPO/scripts/ops/systemd/pm-shadow-exec-paper.service" "$UNIT_DIR/"

# Kill any nohup-launched instances so systemd owns the processes.
pkill -TERM -f "pm-app shadow --edge-threshold" 2>/dev/null || true
pkill -TERM -f "target/release/shadow_exec_tail" 2>/dev/null || true
sleep 2

systemctl --user daemon-reload
systemctl --user enable --now pm-shadow-final.service
systemctl --user enable --now pm-shadow-exec-paper.service
loginctl enable-linger "$USER" 2>/dev/null || true

systemctl --user --no-pager status pm-shadow-final.service pm-shadow-exec-paper.service | grep -E "service|Active"
echo "NOTE: shadow-final restart means a fresh JSONL and ~1h vol3600 warmup."
