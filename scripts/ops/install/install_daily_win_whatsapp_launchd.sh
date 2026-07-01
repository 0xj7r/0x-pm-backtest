#!/usr/bin/env bash
# Mac LaunchAgent: 08:00 Europe/Dublin green-day summary via local WhatsApp bridge.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RELAY="${REPO_ROOT}/scripts/shadow_daily_win_whatsapp_relay.py"
ENV_DIR="$HOME/.config/polymarket-watchdog"
ENV_FILE="$ENV_DIR/whatsapp.env"
PLIST="$HOME/Library/LaunchAgents/com.polymarket.daily-win-whatsapp.plist"
TZ_NAME="${DAILY_SUMMARY_TZ:-Europe/Dublin}"
HOUR="${DAILY_SUMMARY_HOUR:-8}"
LOG_DIR="$HOME/data/pm-alpha/week_monitor"

mkdir -p "$ENV_DIR" "$LOG_DIR"

if [[ ! -f "$ENV_FILE" ]]; then
  BRIDGE_PLIST="$HOME/Library/LaunchAgents/com.whatsapp.bridge.plist"
  API_KEY=""
  if [[ -f "$BRIDGE_PLIST" ]]; then
    API_KEY=$(/usr/libexec/PlistBuddy -c "Print :EnvironmentVariables:WHATSAPP_BRIDGE_API_KEY" "$BRIDGE_PLIST" 2>/dev/null || true)
  fi
  RECIPIENT="${WHATSAPP_RECIPIENT:-447703833707@s.whatsapp.net}"
  cat > "$ENV_FILE" <<EOF
# Local whatsapp-bridge (same as stock-agent)
export WHATSAPP_BRIDGE_URL=http://127.0.0.1:8080
export WHATSAPP_BRIDGE_API_KEY=${API_KEY}
export WHATSAPP_RECIPIENT=${RECIPIENT}
EOF
  chmod 600 "$ENV_FILE"
  echo "Created $ENV_FILE (review recipient + API key)"
fi

cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.polymarket.daily-win-whatsapp</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/bin/env</string>
        <string>TZ=${TZ_NAME}</string>
        <string>python3</string>
        <string>${RELAY}</string>
    </array>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key>
        <integer>${HOUR}</integer>
        <key>Minute</key>
        <integer>0</integer>
    </dict>
    <key>StandardOutPath</key>
    <string>${LOG_DIR}/daily_win_whatsapp_relay.log</string>
    <key>StandardErrorPath</key>
    <string>${LOG_DIR}/daily_win_whatsapp_relay.err</string>
</dict>
</plist>
EOF

launchctl bootout "gui/$(id -u)/com.polymarket.daily-win-whatsapp" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
launchctl enable "gui/$(id -u)/com.polymarket.daily-win-whatsapp"

echo "Installed LaunchAgent: $PLIST"
echo "  Runs daily at ${HOUR}:00 (system local TZ; set TZ=${TZ_NAME} in ProgramArguments)"
echo "  Logs: ${LOG_DIR}/daily_win_whatsapp_relay.log"
echo ""
echo "Disable Dublin Telegram duplicate:"
echo "  ssh Dublin 'crontab -l | sed \"s|python3 .*shadow_daily_win_summary.py|python3 ~/scripts/shadow_daily_win_summary.py --notify none|\" | crontab -'"