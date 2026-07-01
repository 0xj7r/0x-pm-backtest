#!/usr/bin/env bash
# Redeem all pending positions from shadow_redeem_ledger.json via redeem_once.
set -euo pipefail

LEDGER="${LEDGER:-$HOME/data/pm-alpha/shadow_redeem_ledger.json}"
WALLET_ENV="${WALLET_ENV:-$HOME/.config/polymarket-exec/wallet.env}"
REDEEM_BIN="${REDEEM_BIN:-$HOME/deploy-main/polymarket-agent/target/release/redeem_once}"

if [[ ! -f "$LEDGER" ]]; then
  echo "no ledger at $LEDGER" >&2
  exit 0
fi

if [[ ! -x "$REDEEM_BIN" ]]; then
  echo "redeem_once not found: $REDEEM_BIN" >&2
  exit 1
fi

set -a
# shellcheck disable=SC1090
source "$WALLET_ENV"
set +a

python3 - "$LEDGER" <<'PY' | while IFS=$'\t' read -r cid slug index_sets; do
import json, sys
from datetime import datetime, timezone

ledger = json.loads(open(sys.argv[1]).read())
now = int(datetime.now(timezone.utc).timestamp())
for row in ledger:
    slug = row.get("slug", "")
    cid = row.get("condition_id", "")
    close_ts = int(row.get("close_ts_s", 0))
    sets = row.get("index_sets") or [1, 2]
    if not cid:
        continue
    if now < close_ts + 60:
        print(f"SKIP\t{slug}\tnot due (close+60s)", file=sys.stderr)
        continue
    print(f"{cid}\t{slug}\t{','.join(str(x) for x in sets)}")
PY
  echo "redeem $slug ($cid) index_sets=$index_sets"
  if "$REDEEM_BIN" --condition-id "$cid" --index-sets "$index_sets"; then
    echo "  OK $slug"
  else
    echo "  FAIL $slug" >&2
  fi
  sleep 2
done

echo "bulk_redeem_pending done"