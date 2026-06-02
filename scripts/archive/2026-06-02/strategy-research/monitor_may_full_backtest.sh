#!/bin/bash
set -euo pipefail

DIR="data/runs/back_to_explore_may_full_focused"

echo "=== Monitoring the full focused May 2.7k world-class backtest (with 5% daily loss cap) ==="
echo "Log: $DIR/log.txt"
echo "Summary checkpoints in $DIR/summary.json (updated every 200 markets)"
echo ""

if [ -f "$DIR/summary.json" ]; then
  echo "Latest summary:"
  python3 -c '
import json
with open("data/runs/back_to_explore_may_full_focused/summary.json") as f:
    data = json.load(f)
bt = data["per_strategy"]["back_to_explore"]
print(f"  Markets: {data[\"markets_attempted\"]}")
print(f"  Equity: ${bt[\"last_end_equity_usdc\"]:.2f}")
print(f"  PnL: ${bt[\"total_pnl_usdc\"]:.2f}")
print(f"  Max DD: {bt[\"path_max_drawdown_pct\"]:.2f}%")
print(f"  Return: {bt[\"compounded_return_pct\"]:.2f}%")
print(f"  Daily loss cap: {data.get(\"run_config\", {}).get(\"daily_loss_cap_pct\", 1.0)*100:.1f}%")
  '
else
  echo "No summary yet (run just started)."
fi

echo ""
echo "To analyze daily PnL + leading signals (key for opt):"
echo "  python scripts/daily_pnl_and_leading_signals.py --markets $DIR/markets.jsonl --decision-log $DIR/decision_log.jsonl --log $DIR/log.txt"
echo ""
echo "To analyze when checkpoint ready:"
echo "  python scripts/lively_run_profile.py --markets-jsonl $DIR/markets.jsonl --strategy back_to_explore --out $DIR/profile.json"
echo "  python scripts/test_back_to_explore_vs_ground_truth.py --summary $DIR/summary.json --profile $DIR/profile.json"
