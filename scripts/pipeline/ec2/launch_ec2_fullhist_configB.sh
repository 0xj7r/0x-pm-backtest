#!/usr/bin/env bash
# Launch one EC2 runner for a train-once convex-hedge validation backtest (or
# plain full-history baseline for cross-asset diversification).
#
# The instance:
#   1. syncs this repo from S3,
#   2. builds pm-app,
#   3. extracts/trains the meta-calibrator ONCE on the first N markets (on the
#      first combo), then freezes it,
#   4. reuses the frozen snapshot for every (profile, starting-cash) combo so the
#      baseline-vs-convex comparison is apples-to-apples,
#   5. uploads each combo's summary, market rows, logs, and model artifacts to S3,
#   6. terminates itself.
#
# This is a variant of launch_ec2_portfolio_grid.sh. Instead of sweeping clip
# fraction and gross cap, it sweeps (profile, starting-cash): the BonereaperV2
# profile TOML is authoritative for the strategy/convex knobs (it is applied
# last, overriding CLI br2 knobs), while sizing stays fixed across combos.
#
# CROSS-ASSET DIVERSIFICATION (ETH-5m, SOL-5m, etc.):
#   The engine already supports other assets via discovery.rs (infer_spot_symbol_from_slug).
#   You must stage per-asset markets lists (see scripts/prepare_asset_markets.py).
#   Then launch with the correct --slug-prefixes + --spot-symbol + staged keys.
#
#   Example for ETH (after running the prepare script):
#     AWS_PROFILE=visumlabs ./scripts/launch_ec2_fullhist_configB.sh \
#       --start-date 2026-02-18 --end-date 2026-05-20 \
#       --slug-prefixes "eth-updown-5m-" \
#       --spot-symbol ETHUSDT \
#       --markets-key markets/eth-5m-feb-may-2026/markets.jsonl \
#       --train-markets-key markets/eth-5m-feb-may-2026/markets-train.jsonl \
#       --train-markets 4500 \
#       --eval-min-train 0 \
#       --profile configs/bonereaper_v2_leader.toml \
#       --starting-cash 2800 \
#       --use-local-cache 1
#
#   This produces the standalone ETH PnL/Sharpe + (later) BTC+ETH correlation
#   for the portfolio Sharpe calculation. Repeat for SOL/XRP etc.
#
# Usage (BTC champion baseline, the reference):
#   AWS_PROFILE=visumlabs ./scripts/launch_ec2_fullhist_configB.sh \
#     --start-date 2026-02-12 --end-date 2026-05-20 \
#     --train-markets 20000 \
#     --profile configs/bonereaper_v2_leader.toml \
#     --starting-cash 2800
set -euo pipefail

REGION="${AWS_REGION:-us-east-1}"
INSTANCE_TYPE="${INSTANCE_TYPE:-c7i.4xlarge}"
ALLOW_ON_DEMAND_LARGE="${ALLOW_ON_DEMAND_LARGE:-0}"
RESULTS_BUCKET="${RESULTS_BUCKET:-pm-research-backtest-prod}"
SOURCE_BUCKET="${SOURCE_BUCKET:-pm-research-backtest-prod}"
SOURCE_PREFIX="${SOURCE_PREFIX:-source/polymarket-backtest}"
MARKETS_PARQUET_BUCKET="${MARKETS_PARQUET_BUCKET:-pm-research-backtest-prod}"
MARKETS_PARQUET_KEY="${MARKETS_PARQUET_KEY:-artifacts/markets-full.parquet}"
INSTANCE_PROFILE="${INSTANCE_PROFILE:-instanceRole}"
KEY_NAME="${KEY_NAME:-whale-pair-use1}"
SECURITY_GROUP_ID="${SECURITY_GROUP_ID:-sg-0714c4165723a894a}"
SUBNET_ID="${SUBNET_ID:-subnet-0c16e9b7f39d97feb}"
ROOT_VOLUME_GB="${ROOT_VOLUME_GB:-250}"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-fullhist-configB-$$"

MARKETS_KEY=""
TRAIN_MARKETS_KEY=""
# Train window = --min-train-markets (e.g. 4500). The cap must exceed the train
# window or the run errors with "leaves no markets"; the small remainder is a
# throwaway eval needed to flush the snapshot.
TRAIN_ONLY_MAX_MARKETS="4600"
START_DATE=""
END_DATE=""
SLUG_PREFIXES="btc-updown-5m-"
STRATEGIES="bonereaper_v2"
PROFILE_PATH=""
STARTING_CASH="1000"
MAX_CLIP="30"
MAX_ORDER_CLIP_MULTIPLIER="6.0"
MAX_PER_MARKET_EXPOSURE_FRAC="0.12"
KELLY="0.5"
CLIP_DRAWDOWN_SOFT_PCT="1.0"
CLIP_DRAWDOWN_HARD_PCT="1.0"
CLIP_DRAWDOWN_MIN_MULTIPLIER="0.0"
BR2_DISABLE_INTERNAL_MODEL_GATES="0"
BR2_PARTICIPATION_CLIP_FRAC="0.0"
BR2_PARTICIPATION_MAX_PAIR_COST="0.99"
BR2_PARTICIPATION_MAX_ORDERS_PER_LEG="500"
BR2_PARTICIPATION_MAX_INVENTORY_DELTA_SHARES="25.0"
BR2_PARTICIPATION_REPAIR_INVENTORY_DELTA_SHARES="5.0"
BR2_PARTICIPATION_REFRESH_SECS="0.50"
BR2_PARTICIPATION_STOP_SECS_BEFORE_CLOSE="20.0"
BR2_MIN_COMPOSITE_DIRECTION="0.10"
BR2_EARLY_CLIP_FRAC="0.00"
BR2_MID_CLIP_FRAC="0.00"
BR2_LATE_CLIP_FRAC="1.0"
BR2_LATE_MAX_FIRES="3"
BR2_LATE_CONFIRM_MIN_MODEL_CONFIDENCE="0.58"
BR2_LATE_CONFIRM_MAX_MODEL_RISK="0.80"
BR2_LATE_CONFIRM_MIN_MODEL_SIDE_P="0.58"
BR2_LATE_CONFIRM_MIN_MODEL_EDGE="0.02"
BR2_LATE_CONFIRM_MIN_BOOK_SKEW="0.06"
BR2_LATE_CONFIRM_MAX_WHIPSAW_SCORE="0.85"
BR2_LATE_CONFIRM_MIN_REALIZED_VOL_180S_BPS="0.0"
BR2_LATE_CONFIRM_MAX_OBSERVED_RANGE="1.0"
BR2_RECENT_REGIME_GATE_ENABLED="0"
BR2_RECENT_REGIME_GATE_MIN_EDGE="0.08"
BR2_HIGH_SKEW_CLIP_FRAC="0.60"
BR2_HIGH_SKEW_MAX_CLIPS="5"
BR2_HIGH_SKEW_MAX_WHIPSAW_SCORE="0.75"
BR2_HIGH_SKEW_MIN_REALIZED_VOL_180S_BPS="0.0"
BR2_LATE_FAVOURITE_START_SECS="180.0"
BR2_LATE_FAVOURITE_THRESHOLD="0.22"
BR2_LATE_FAVOURITE_MIN_ASK="0.70"
BR2_LATE_FAVOURITE_MAX_ASK="0.97"
BR2_LATE_FAVOURITE_CLIP_FRAC="1.00"
BR2_LATE_FAVOURITE_HIGH_CERT_CLIP_FRAC="1.00"
BR2_LATE_FAVOURITE_HIGH_CERT_FULL_CLIP_EDGE="0.04"
BR2_LATE_FAVOURITE_MAX_CLIPS="12"
BR2_LATE_FAVOURITE_MIN_SUSTAIN_SECS="0.0"
BR2_LATE_FAVOURITE_SWEEP_DEPTH="7"
BR2_LATE_FAVOURITE_MIN_MODEL_DIRECTION_ABS="0.0"
BR2_LATE_FAVOURITE_MIN_MODEL_CONFIDENCE="0.68"
BR2_LATE_FAVOURITE_MAX_MODEL_RISK="0.72"
BR2_LATE_FAVOURITE_MIN_MODEL_SIDE_P="0.62"
BR2_LATE_FAVOURITE_MIN_MODEL_EDGE="0.03"
BR2_LATE_FAVOURITE_HIGH_CERT_MIN_MODEL_EDGE="0.02"
BR2_LATE_FAVOURITE_HIGH_CERT_BYPASS_MODEL_EDGE="0"
BR2_LATE_FAVOURITE_MAX_WHIPSAW_SCORE="0.75"
BR2_LATE_FAVOURITE_MAX_REVERSAL_PRESSURE="1.0"
BR2_LATE_FAVOURITE_MIN_PATH_EFFICIENCY="0.0"
BR2_LATE_FAVOURITE_MIN_REALIZED_VOL_180S_BPS="0.0"
BR2_LATE_FAVOURITE_MAX_OBSERVED_RANGE="1.0"
BR2_LATE_FAVOURITE_RANGE_SOFT_THROTTLE="0.78"
BR2_LATE_FAVOURITE_RANGE_HARD_THROTTLE="0.98"
BR2_LATE_FAVOURITE_RANGE_EXTRA_EDGE="0.03"
BR2_LATE_FAVOURITE_RANGE_EXTRA_CONFIDENCE="0.08"
BR2_LATE_FAVOURITE_MAX_ADVERSE_FAST_MOMENTUM="1.0"
BR2_LATE_FAVOURITE_MAX_ADVERSE_BROAD_MOMENTUM="1.0"
BR2_LATE_FAVOURITE_MAX_ENTRY_PULLBACK="1.0"
BR2_LATE_FAVOURITE_MAX_AVG_ENTRY_DRAWDOWN="1.0"
BR2_TAIL_CLIP_FRAC="0.10"
BR2_TAIL_MAX_CLIPS="3"
BR2_TAIL_SWEEP_DEPTH="3"
BR2_TAIL_MIN_ASK="0.01"
BR2_TAIL_MAX_ASK="0.10"
BR2_TAIL_MIN_SECONDS_TO_CLOSE="10.0"
BR2_TAIL_MIN_FAVOURITE_UNREALIZED_EDGE="0.0"
BR2_TAIL_MIN_OBSERVED_RANGE="0.0"
BR2_TAIL_TARGET_FAVOURITE_LOSS_COVERAGE_FRAC="0.50"
BR2_TAIL_REVERSAL_COVERAGE_FRAC="0.00"
BR2_TAIL_REVERSAL_MIN_SECONDS_TO_CLOSE="10.0"
BR2_TAIL_REVERSAL_MAX_SECONDS_TO_CLOSE="35.0"
BR2_TAIL_REVERSAL_MIN_FAVOURITE_ASK="0.85"
BR2_TAIL_EXTREME_THRESHOLD="0.30"
BR2_TAIL_MIN_SKEW_STEP="0.02"
BR2_TAIL_BUDGET_FAVOURITE_SPEND_FRAC="0.20"
BR2_TAIL_BUDGET_FAVOURITE_UPSIDE_FRAC="0.25"
BR2_TAIL_REGIME_BOOST_COVERAGE_FRAC="0.0"
BR2_TAIL_REGIME_BOOST_BUDGET_SPEND_FRAC="0.0"
BR2_TAIL_REGIME_BOOST_BUDGET_UPSIDE_FRAC="0.0"
BR2_TAIL_REGIME_BOOST_MIN_WHIPSAW_SCORE="1.0"
BR2_TAIL_REGIME_BOOST_MIN_REVERSAL_PRESSURE="1.0"
BR2_TAIL_REGIME_BOOST_MIN_REALIZED_VOL_180S_BPS="1000000000.0"
BR2_TAIL_REGIME_BOOST_MAX_PATH_EFFICIENCY="0.0"
ENFORCE_MODEL_GATE="true"
MODEL_GATE_MIN_CONFIDENCE="0.68"
MODEL_GATE_MAX_RISK="0.72"
MODEL_GATE_MIN_EDGE="0.00"
ENABLE_MARKET_CONTEXT_FEATURES="0"
TRAIN_MARKETS="2880"
# Out-of-sample eval trims the first EVAL_MIN_TRAIN markets (062901 manifest used
# --min-train-markets 4500). The snapshot is reused, so this only excludes the
# 062901 training region from the evaluated set; it does not fit anything.
EVAL_MIN_TRAIN="4500"
META_EPOCHS="24"
META_LEARNING_RATE="0.04"
META_L2="0.001"
META_WEIGHT_CLIP="1.50"
META_MAX_FIT_SAMPLES="120000"
META_MAX_VALIDATION_SAMPLES="60000"
META_MAX_SAMPLES_PER_MARKET="64"
META_MAX_OOS_EVALUATION_SAMPLES="120000"
META_TRAIN_MIN_BASE_P="0.0"
META_TRAIN_MAX_EARLY_PENALTY="1.0"
META_TRAIN_MIN_MID_DISTANCE="0.0"
DISABLE_META_CALIBRATION="0"
FORBID_META_TRAINING="0"
CLIP_FRACTIONS="0.015,0.02,0.03"
GROSS_CAPS="250,500,750"
PROFILES="configs/bonereaper_v2_leader.toml,configs/bonereaper_v2_convex_scaled.toml,configs/bonereaper_v2_convex_reversal.toml"
STARTING_CASHES="1000,2800"
CLIP_FRACTION="0.015"
GROSS_CAP="250"
MAX_PARALLEL_COMBOS="4"
SPOT_SYMBOL="BTCUSDT"
MAX_CONCURRENT_FETCHES="32"
REPLAY_SAMPLE_MS="1000"
TAKER_LATENCY_MS="500"
USE_LOCAL_CACHE="1"
LABEL_SUFFIX=""
LOCAL_CACHE_DIR="/opt/pm/cache"
PREP_CACHE_MAX_CONCURRENT="64"
DISCOVERY_MAX_CONCURRENT="4"
PORTFOLIO_CHECKPOINT_EVERY_MARKETS="250"
SYNC_SOURCE="1"
SNAPSHOT_S3_URI=""
META_TRAINING_SAMPLES_CACHE_S3_URI=""
REUSE_ARTIFACTS_RUN_ID=""
PM_APP_BINARY_S3_URI=""

while [ $# -gt 0 ]; do
    case "$1" in
        --markets-key) MARKETS_KEY="$2"; shift 2 ;;
        --train-markets-key) TRAIN_MARKETS_KEY="$2"; shift 2 ;;
        --train-only-max-markets) TRAIN_ONLY_MAX_MARKETS="$2"; shift 2 ;;
        --start-date) START_DATE="$2"; shift 2 ;;
        --end-date) END_DATE="$2"; shift 2 ;;
        --slug-prefixes) SLUG_PREFIXES="$2"; shift 2 ;;
        --strategies) STRATEGIES="$2"; shift 2 ;;
        --profile) PROFILE_PATH="$2"; shift 2 ;;
        --starting-cash) STARTING_CASH="$2"; shift 2 ;;
        --max-clip) MAX_CLIP="$2"; shift 2 ;;
        --max-order-clip-multiplier) MAX_ORDER_CLIP_MULTIPLIER="$2"; shift 2 ;;
        --max-per-market-exposure-frac) MAX_PER_MARKET_EXPOSURE_FRAC="$2"; shift 2 ;;
        --kelly) KELLY="$2"; shift 2 ;;
        --clip-drawdown-soft-pct) CLIP_DRAWDOWN_SOFT_PCT="$2"; shift 2 ;;
        --clip-drawdown-hard-pct) CLIP_DRAWDOWN_HARD_PCT="$2"; shift 2 ;;
        --clip-drawdown-min-multiplier) CLIP_DRAWDOWN_MIN_MULTIPLIER="$2"; shift 2 ;;
        --br2-disable-internal-model-gates) BR2_DISABLE_INTERNAL_MODEL_GATES="1"; shift ;;
        --br2-participation-clip-frac) BR2_PARTICIPATION_CLIP_FRAC="$2"; shift 2 ;;
        --br2-participation-max-pair-cost) BR2_PARTICIPATION_MAX_PAIR_COST="$2"; shift 2 ;;
        --br2-participation-max-orders-per-leg) BR2_PARTICIPATION_MAX_ORDERS_PER_LEG="$2"; shift 2 ;;
        --br2-participation-max-inventory-delta-shares) BR2_PARTICIPATION_MAX_INVENTORY_DELTA_SHARES="$2"; shift 2 ;;
        --br2-participation-repair-inventory-delta-shares) BR2_PARTICIPATION_REPAIR_INVENTORY_DELTA_SHARES="$2"; shift 2 ;;
        --br2-participation-refresh-secs) BR2_PARTICIPATION_REFRESH_SECS="$2"; shift 2 ;;
        --br2-participation-stop-secs-before-close) BR2_PARTICIPATION_STOP_SECS_BEFORE_CLOSE="$2"; shift 2 ;;
        --br2-min-composite-direction) BR2_MIN_COMPOSITE_DIRECTION="$2"; shift 2 ;;
        --br2-early-clip-frac) BR2_EARLY_CLIP_FRAC="$2"; shift 2 ;;
        --br2-mid-clip-frac) BR2_MID_CLIP_FRAC="$2"; shift 2 ;;
        --br2-late-clip-frac) BR2_LATE_CLIP_FRAC="$2"; shift 2 ;;
        --br2-late-max-fires) BR2_LATE_MAX_FIRES="$2"; shift 2 ;;
        --br2-late-confirm-min-model-confidence) BR2_LATE_CONFIRM_MIN_MODEL_CONFIDENCE="$2"; shift 2 ;;
        --br2-late-confirm-max-model-risk) BR2_LATE_CONFIRM_MAX_MODEL_RISK="$2"; shift 2 ;;
        --br2-late-confirm-min-model-side-p) BR2_LATE_CONFIRM_MIN_MODEL_SIDE_P="$2"; shift 2 ;;
        --br2-late-confirm-min-model-edge) BR2_LATE_CONFIRM_MIN_MODEL_EDGE="$2"; shift 2 ;;
        --br2-late-confirm-min-book-skew) BR2_LATE_CONFIRM_MIN_BOOK_SKEW="$2"; shift 2 ;;
        --br2-late-confirm-max-whipsaw-score) BR2_LATE_CONFIRM_MAX_WHIPSAW_SCORE="$2"; shift 2 ;;
        --br2-late-confirm-min-realized-vol-180s-bps) BR2_LATE_CONFIRM_MIN_REALIZED_VOL_180S_BPS="$2"; shift 2 ;;
        --br2-late-confirm-max-observed-range) BR2_LATE_CONFIRM_MAX_OBSERVED_RANGE="$2"; shift 2 ;;
        --br2-recent-regime-gate-enabled) BR2_RECENT_REGIME_GATE_ENABLED="1"; shift ;;
        --br2-recent-regime-gate-min-edge) BR2_RECENT_REGIME_GATE_MIN_EDGE="$2"; shift 2 ;;
        --br2-high-skew-clip-frac) BR2_HIGH_SKEW_CLIP_FRAC="$2"; shift 2 ;;
        --br2-high-skew-max-clips) BR2_HIGH_SKEW_MAX_CLIPS="$2"; shift 2 ;;
        --br2-high-skew-max-whipsaw-score) BR2_HIGH_SKEW_MAX_WHIPSAW_SCORE="$2"; shift 2 ;;
        --br2-high-skew-min-realized-vol-180s-bps) BR2_HIGH_SKEW_MIN_REALIZED_VOL_180S_BPS="$2"; shift 2 ;;
        --br2-late-favourite-start-secs) BR2_LATE_FAVOURITE_START_SECS="$2"; shift 2 ;;
        --br2-late-favourite-threshold) BR2_LATE_FAVOURITE_THRESHOLD="$2"; shift 2 ;;
        --br2-late-favourite-min-ask) BR2_LATE_FAVOURITE_MIN_ASK="$2"; shift 2 ;;
        --br2-late-favourite-max-ask) BR2_LATE_FAVOURITE_MAX_ASK="$2"; shift 2 ;;
        --br2-late-favourite-clip-frac) BR2_LATE_FAVOURITE_CLIP_FRAC="$2"; shift 2 ;;
        --br2-late-favourite-high-cert-clip-frac) BR2_LATE_FAVOURITE_HIGH_CERT_CLIP_FRAC="$2"; shift 2 ;;
        --br2-late-favourite-high-cert-full-clip-edge) BR2_LATE_FAVOURITE_HIGH_CERT_FULL_CLIP_EDGE="$2"; shift 2 ;;
        --br2-late-favourite-max-clips) BR2_LATE_FAVOURITE_MAX_CLIPS="$2"; shift 2 ;;
        --br2-late-favourite-min-sustain-secs) BR2_LATE_FAVOURITE_MIN_SUSTAIN_SECS="$2"; shift 2 ;;
        --br2-late-favourite-sweep-depth) BR2_LATE_FAVOURITE_SWEEP_DEPTH="$2"; shift 2 ;;
        --br2-late-favourite-min-model-confidence) BR2_LATE_FAVOURITE_MIN_MODEL_CONFIDENCE="$2"; shift 2 ;;
        --br2-late-favourite-min-model-direction-abs) BR2_LATE_FAVOURITE_MIN_MODEL_DIRECTION_ABS="$2"; shift 2 ;;
        --br2-late-favourite-max-model-risk) BR2_LATE_FAVOURITE_MAX_MODEL_RISK="$2"; shift 2 ;;
        --br2-late-favourite-min-model-side-p) BR2_LATE_FAVOURITE_MIN_MODEL_SIDE_P="$2"; shift 2 ;;
        --br2-late-favourite-min-model-edge) BR2_LATE_FAVOURITE_MIN_MODEL_EDGE="$2"; shift 2 ;;
        --br2-late-favourite-high-cert-min-model-edge) BR2_LATE_FAVOURITE_HIGH_CERT_MIN_MODEL_EDGE="$2"; shift 2 ;;
        --br2-late-favourite-high-cert-bypass-model-edge) BR2_LATE_FAVOURITE_HIGH_CERT_BYPASS_MODEL_EDGE="1"; shift ;;
        --br2-late-favourite-max-whipsaw-score) BR2_LATE_FAVOURITE_MAX_WHIPSAW_SCORE="$2"; shift 2 ;;
        --br2-late-favourite-max-reversal-pressure) BR2_LATE_FAVOURITE_MAX_REVERSAL_PRESSURE="$2"; shift 2 ;;
        --br2-late-favourite-min-path-efficiency) BR2_LATE_FAVOURITE_MIN_PATH_EFFICIENCY="$2"; shift 2 ;;
        --br2-late-favourite-min-realized-vol-180s-bps) BR2_LATE_FAVOURITE_MIN_REALIZED_VOL_180S_BPS="$2"; shift 2 ;;
        --br2-late-favourite-max-observed-range) BR2_LATE_FAVOURITE_MAX_OBSERVED_RANGE="$2"; shift 2 ;;
        --br2-late-favourite-range-soft-throttle) BR2_LATE_FAVOURITE_RANGE_SOFT_THROTTLE="$2"; shift 2 ;;
        --br2-late-favourite-range-hard-throttle) BR2_LATE_FAVOURITE_RANGE_HARD_THROTTLE="$2"; shift 2 ;;
        --br2-late-favourite-range-extra-edge) BR2_LATE_FAVOURITE_RANGE_EXTRA_EDGE="$2"; shift 2 ;;
        --br2-late-favourite-range-extra-confidence) BR2_LATE_FAVOURITE_RANGE_EXTRA_CONFIDENCE="$2"; shift 2 ;;
        --br2-late-favourite-max-adverse-fast-momentum) BR2_LATE_FAVOURITE_MAX_ADVERSE_FAST_MOMENTUM="$2"; shift 2 ;;
        --br2-late-favourite-max-adverse-broad-momentum) BR2_LATE_FAVOURITE_MAX_ADVERSE_BROAD_MOMENTUM="$2"; shift 2 ;;
        --br2-late-favourite-max-entry-pullback) BR2_LATE_FAVOURITE_MAX_ENTRY_PULLBACK="$2"; shift 2 ;;
        --br2-late-favourite-max-avg-entry-drawdown) BR2_LATE_FAVOURITE_MAX_AVG_ENTRY_DRAWDOWN="$2"; shift 2 ;;
        --br2-tail-clip-frac) BR2_TAIL_CLIP_FRAC="$2"; shift 2 ;;
        --br2-tail-max-clips) BR2_TAIL_MAX_CLIPS="$2"; shift 2 ;;
        --br2-tail-sweep-depth) BR2_TAIL_SWEEP_DEPTH="$2"; shift 2 ;;
        --br2-tail-min-ask) BR2_TAIL_MIN_ASK="$2"; shift 2 ;;
        --br2-tail-max-ask) BR2_TAIL_MAX_ASK="$2"; shift 2 ;;
        --br2-tail-min-seconds-to-close) BR2_TAIL_MIN_SECONDS_TO_CLOSE="$2"; shift 2 ;;
        --br2-tail-min-favourite-unrealized-edge) BR2_TAIL_MIN_FAVOURITE_UNREALIZED_EDGE="$2"; shift 2 ;;
        --br2-tail-min-observed-range) BR2_TAIL_MIN_OBSERVED_RANGE="$2"; shift 2 ;;
        --br2-tail-target-favourite-loss-coverage-frac) BR2_TAIL_TARGET_FAVOURITE_LOSS_COVERAGE_FRAC="$2"; shift 2 ;;
        --br2-tail-reversal-coverage-frac) BR2_TAIL_REVERSAL_COVERAGE_FRAC="$2"; shift 2 ;;
        --br2-tail-reversal-min-seconds-to-close) BR2_TAIL_REVERSAL_MIN_SECONDS_TO_CLOSE="$2"; shift 2 ;;
        --br2-tail-reversal-max-seconds-to-close) BR2_TAIL_REVERSAL_MAX_SECONDS_TO_CLOSE="$2"; shift 2 ;;
        --br2-tail-reversal-min-favourite-ask) BR2_TAIL_REVERSAL_MIN_FAVOURITE_ASK="$2"; shift 2 ;;
        --br2-tail-extreme-threshold) BR2_TAIL_EXTREME_THRESHOLD="$2"; shift 2 ;;
        --br2-tail-min-skew-step) BR2_TAIL_MIN_SKEW_STEP="$2"; shift 2 ;;
        --br2-tail-budget-favourite-spend-frac) BR2_TAIL_BUDGET_FAVOURITE_SPEND_FRAC="$2"; shift 2 ;;
        --br2-tail-budget-favourite-upside-frac) BR2_TAIL_BUDGET_FAVOURITE_UPSIDE_FRAC="$2"; shift 2 ;;
        --br2-tail-regime-boost-coverage-frac) BR2_TAIL_REGIME_BOOST_COVERAGE_FRAC="$2"; shift 2 ;;
        --br2-tail-regime-boost-budget-spend-frac) BR2_TAIL_REGIME_BOOST_BUDGET_SPEND_FRAC="$2"; shift 2 ;;
        --br2-tail-regime-boost-budget-upside-frac) BR2_TAIL_REGIME_BOOST_BUDGET_UPSIDE_FRAC="$2"; shift 2 ;;
        --br2-tail-regime-boost-min-whipsaw-score) BR2_TAIL_REGIME_BOOST_MIN_WHIPSAW_SCORE="$2"; shift 2 ;;
        --br2-tail-regime-boost-min-reversal-pressure) BR2_TAIL_REGIME_BOOST_MIN_REVERSAL_PRESSURE="$2"; shift 2 ;;
        --br2-tail-regime-boost-min-realized-vol-180s-bps) BR2_TAIL_REGIME_BOOST_MIN_REALIZED_VOL_180S_BPS="$2"; shift 2 ;;
        --br2-tail-regime-boost-max-path-efficiency) BR2_TAIL_REGIME_BOOST_MAX_PATH_EFFICIENCY="$2"; shift 2 ;;
        --enforce-model-gate) ENFORCE_MODEL_GATE="$2"; shift 2 ;;
        --model-gate-min-confidence) MODEL_GATE_MIN_CONFIDENCE="$2"; shift 2 ;;
        --model-gate-max-risk) MODEL_GATE_MAX_RISK="$2"; shift 2 ;;
        --model-gate-min-edge) MODEL_GATE_MIN_EDGE="$2"; shift 2 ;;
        --enable-market-context-features) ENABLE_MARKET_CONTEXT_FEATURES="1"; shift ;;
        --train-markets) TRAIN_MARKETS="$2"; shift 2 ;;
        --eval-min-train) EVAL_MIN_TRAIN="$2"; shift 2 ;;
        --meta-epochs) META_EPOCHS="$2"; shift 2 ;;
        --meta-learning-rate) META_LEARNING_RATE="$2"; shift 2 ;;
        --meta-l2) META_L2="$2"; shift 2 ;;
        --meta-weight-clip) META_WEIGHT_CLIP="$2"; shift 2 ;;
        --meta-max-fit-samples) META_MAX_FIT_SAMPLES="$2"; shift 2 ;;
        --meta-max-validation-samples) META_MAX_VALIDATION_SAMPLES="$2"; shift 2 ;;
        --meta-max-samples-per-market) META_MAX_SAMPLES_PER_MARKET="$2"; shift 2 ;;
        --meta-max-oos-evaluation-samples) META_MAX_OOS_EVALUATION_SAMPLES="$2"; shift 2 ;;
        --meta-train-min-base-p) META_TRAIN_MIN_BASE_P="$2"; shift 2 ;;
        --meta-train-max-early-penalty) META_TRAIN_MAX_EARLY_PENALTY="$2"; shift 2 ;;
        --meta-train-min-mid-distance) META_TRAIN_MIN_MID_DISTANCE="$2"; shift 2 ;;
        --disable-meta-calibration) DISABLE_META_CALIBRATION="1"; shift ;;
        --forbid-meta-training) FORBID_META_TRAINING="1"; shift ;;
        --clip-fractions) CLIP_FRACTIONS="$2"; shift 2 ;;
        --gross-caps) GROSS_CAPS="$2"; shift 2 ;;
        --profiles) PROFILES="$2"; shift 2 ;;
        --starting-cashes) STARTING_CASHES="$2"; shift 2 ;;
        --clip-fraction) CLIP_FRACTION="$2"; shift 2 ;;
        --gross-cap) GROSS_CAP="$2"; shift 2 ;;
        --max-parallel-combos) MAX_PARALLEL_COMBOS="$2"; shift 2 ;;
        --spot-symbol) SPOT_SYMBOL="$2"; shift 2 ;;
        --max-concurrent-fetches) MAX_CONCURRENT_FETCHES="$2"; shift 2 ;;
        --replay-sample-ms) REPLAY_SAMPLE_MS="$2"; shift 2 ;;
        --taker-latency-ms) TAKER_LATENCY_MS="$2"; shift 2 ;;
        --use-local-cache) USE_LOCAL_CACHE="1"; shift ;;
        --label-suffix) LABEL_SUFFIX="$2"; shift 2 ;;
        --local-cache-dir) LOCAL_CACHE_DIR="$2"; shift 2 ;;
        --prep-cache-max-concurrent) PREP_CACHE_MAX_CONCURRENT="$2"; shift 2 ;;
        --discovery-max-concurrent) DISCOVERY_MAX_CONCURRENT="$2"; shift 2 ;;
        --portfolio-checkpoint-every-markets) PORTFOLIO_CHECKPOINT_EVERY_MARKETS="$2"; shift 2 ;;
        --instance-type) INSTANCE_TYPE="$2"; shift 2 ;;
        --allow-on-demand-large) ALLOW_ON_DEMAND_LARGE="1"; shift ;;
        --instance-profile) INSTANCE_PROFILE="$2"; shift 2 ;;
        --markets-parquet-bucket) MARKETS_PARQUET_BUCKET="$2"; shift 2 ;;
        --markets-parquet-key) MARKETS_PARQUET_KEY="$2"; shift 2 ;;
        --key-name) KEY_NAME="$2"; shift 2 ;;
        --security-group-id) SECURITY_GROUP_ID="$2"; shift 2 ;;
        --subnet-id) SUBNET_ID="$2"; shift 2 ;;
        --root-volume-gb) ROOT_VOLUME_GB="$2"; shift 2 ;;
        --snapshot-s3-uri) SNAPSHOT_S3_URI="$2"; shift 2 ;;
        --meta-training-samples-cache-s3-uri) META_TRAINING_SAMPLES_CACHE_S3_URI="$2"; shift 2 ;;
        --reuse-artifacts-run-id) REUSE_ARTIFACTS_RUN_ID="$2"; shift 2 ;;
        --pm-app-binary-s3-uri) PM_APP_BINARY_S3_URI="$2"; shift 2 ;;
        --no-source-sync) SYNC_SOURCE="0"; shift ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

if [ -z "$MARKETS_KEY" ] && { [ -z "$START_DATE" ] || [ -z "$END_DATE" ]; }; then
    echo "either --markets-key or both --start-date and --end-date are required" >&2
    exit 1
fi

if [ -n "$REUSE_ARTIFACTS_RUN_ID" ]; then
    if [ -z "$SNAPSHOT_S3_URI" ]; then
        SNAPSHOT_S3_URI="s3://${RESULTS_BUCKET}/results/${REUSE_ARTIFACTS_RUN_ID}/artifacts/meta-calibrator-snapshot.json"
    fi
    if [ -z "$META_TRAINING_SAMPLES_CACHE_S3_URI" ]; then
        META_TRAINING_SAMPLES_CACHE_S3_URI="s3://${RESULTS_BUCKET}/results/${REUSE_ARTIFACTS_RUN_ID}/artifacts/meta-training-samples.json"
    fi
fi

if [ "$FORBID_META_TRAINING" = "1" ] && [ "$DISABLE_META_CALIBRATION" != "1" ] && [ -z "$SNAPSHOT_S3_URI" ]; then
    echo "--forbid-meta-training requires --reuse-artifacts-run-id or --snapshot-s3-uri" >&2
    exit 1
fi

case "$INSTANCE_TYPE" in
    *.4xlarge|*.8xlarge|*.[0-9][0-9]xlarge) LARGE_INSTANCE="1" ;;
    *) LARGE_INSTANCE="0" ;;
esac
if [ "$LARGE_INSTANCE" = "1" ] && [ "$ALLOW_ON_DEMAND_LARGE" != "1" ]; then
    echo "Refusing on-demand $INSTANCE_TYPE launch without --allow-on-demand-large." >&2
    echo "Set ALLOW_ON_DEMAND_LARGE=1 or pass --allow-on-demand-large when this large runner is intentional." >&2
    exit 1
fi

if [ "$SYNC_SOURCE" = "1" ]; then
    echo "Syncing source to s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/"
    aws s3 rm "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/" --recursive --quiet
    for file in Cargo.toml Cargo.lock rust-toolchain.toml README.md .gitignore; do
        [ -f "$file" ] && aws s3 cp "$file" "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/${file}" --quiet
    done
    aws s3 sync crates "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/crates/" --delete --quiet
    aws s3 sync scripts "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/scripts/" --delete --quiet
    aws s3 sync configs "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/configs/" --delete --quiet
    [ -d docs ] && aws s3 sync docs "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/docs/" --delete --quiet
fi

SOURCE_GIT_SHA="$(git rev-parse HEAD 2>/dev/null || echo unknown)"

AMI=$(aws ssm get-parameter \
    --region "$REGION" \
    --name /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 \
    --query Parameter.Value --output text)

echo "AMI: $AMI"
echo "Run ID: $RUN_ID"
echo "Source git SHA: $SOURCE_GIT_SHA"
if [ -n "$SNAPSHOT_S3_URI" ]; then
    echo "Meta snapshot in: $SNAPSHOT_S3_URI"
fi
if [ "$FORBID_META_TRAINING" = "1" ]; then
    echo "Meta training forbidden: true"
fi
if [ -n "$META_TRAINING_SAMPLES_CACHE_S3_URI" ]; then
    echo "Meta samples cache in: $META_TRAINING_SAMPLES_CACHE_S3_URI"
fi
if [ -n "$PM_APP_BINARY_S3_URI" ]; then
    echo "pm-app binary in: $PM_APP_BINARY_S3_URI"
fi

USER_DATA=$(cat <<EOF
#!/bin/bash
set -euo pipefail
exec > >(tee -a /var/log/pm-bootstrap.log) 2>&1
echo "[\$(date -u)] starting pm portfolio grid run_id=${RUN_ID}"

dnf install -y git gcc gcc-c++ make openssl-devel pkgconf-pkg-config cmake clang jq
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | bash -s -- -y --default-toolchain 1.95
export HOME=/root
source /root/.cargo/env

mkdir -p /opt/pm/artifacts /opt/pm/results
aws s3 sync "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/" /opt/pm/ \
    --exclude "target/*" --exclude "data/*" --exclude ".git/*"
cd /opt/pm
export PM_SOURCE_GIT_SHA="${SOURCE_GIT_SHA}"
if [ -n "${PM_APP_BINARY_S3_URI}" ]; then
  mkdir -p /opt/pm/target/release
  aws s3 cp "${PM_APP_BINARY_S3_URI}" /opt/pm/target/release/pm-app
  chmod +x /opt/pm/target/release/pm-app
  echo "[\$(date -u)] using prebuilt pm-app from ${PM_APP_BINARY_S3_URI}"
else
  cargo build --release -p pm-app
fi

if [ -n "${MARKETS_KEY}" ]; then
  aws s3 cp "s3://${SOURCE_BUCKET}/markets/${MARKETS_KEY}" /opt/pm/markets.jsonl
  if [ -n "${TRAIN_MARKETS_KEY}" ]; then
    aws s3 cp "s3://${SOURCE_BUCKET}/markets/${TRAIN_MARKETS_KEY}" /opt/pm/markets-train.jsonl
  else
    cp /opt/pm/markets.jsonl /opt/pm/markets-train.jsonl
  fi
else
  aws s3 cp "s3://${MARKETS_PARQUET_BUCKET}/${MARKETS_PARQUET_KEY}" /opt/pm/markets-full.parquet
  PM_TELONEX_REGION="${REGION}" ./target/release/pm-app discover-markets-parquet \\
    --markets-parquet /opt/pm/markets-full.parquet \\
    --start-date "${START_DATE}" \\
    --end-date "${END_DATE}" \\
    --slug-prefix "${SLUG_PREFIXES}" \\
    --require-book-s3 \\
    --out /opt/pm/markets.jsonl
  aws s3 cp /opt/pm/markets.jsonl "s3://${RESULTS_BUCKET}/results/${RUN_ID}/artifacts/markets.jsonl"
fi

SNAPSHOT_IN=""
if [ -n "${SNAPSHOT_S3_URI}" ]; then
  SNAPSHOT_IN="/opt/pm/artifacts/meta-calibrator-snapshot-in.json"
  aws s3 cp "${SNAPSHOT_S3_URI}" "\${SNAPSHOT_IN}"
fi

if [ -n "${META_TRAINING_SAMPLES_CACHE_S3_URI}" ]; then
  aws s3 cp "${META_TRAINING_SAMPLES_CACHE_S3_URI}" /opt/pm/artifacts/meta-training-samples.json
fi

LOCAL_CACHE_ARGS=()
if [ "${USE_LOCAL_CACHE}" = "1" ]; then
  echo "[\$(date -u)] prewarming local Telonex cache at ${LOCAL_CACHE_DIR}"
  PM_TELONEX_REGION="${REGION}" ./target/release/pm-app prep-cache \\
    --markets /opt/pm/markets.jsonl \\
    --cache-dir "${LOCAL_CACHE_DIR}" \\
    --spot-symbol "${SPOT_SYMBOL}" \\
    --max-concurrent "${PREP_CACHE_MAX_CONCURRENT}"
  LOCAL_CACHE_ARGS=(--local-cache-dir "${LOCAL_CACHE_DIR}")
fi

MODEL_GATE_ARGS=()
if [ "${ENFORCE_MODEL_GATE}" != "true" ]; then
  MODEL_GATE_ARGS=(--disable-model-gate)
fi
MARKET_CONTEXT_FEATURE_ARGS=()
if [ "${ENABLE_MARKET_CONTEXT_FEATURES}" = "1" ]; then
  MARKET_CONTEXT_FEATURE_ARGS=(--enable-market-context-features)
fi
BR2_INTERNAL_MODEL_GATE_ARGS=()
if [ "${BR2_DISABLE_INTERNAL_MODEL_GATES}" = "1" ]; then
  BR2_INTERNAL_MODEL_GATE_ARGS=(--br2-disable-internal-model-gates)
fi
BR2_LATE_FAVOURITE_HIGH_CERT_BYPASS_MODEL_EDGE_ARGS=()
if [ "${BR2_LATE_FAVOURITE_HIGH_CERT_BYPASS_MODEL_EDGE}" = "1" ]; then
  BR2_LATE_FAVOURITE_HIGH_CERT_BYPASS_MODEL_EDGE_ARGS=(--br2-late-favourite-high-cert-bypass-model-edge)
fi
BR2_RECENT_REGIME_GATE_ARGS=()
if [ "${BR2_RECENT_REGIME_GATE_ENABLED}" = "1" ]; then
  BR2_RECENT_REGIME_GATE_ARGS=(--br2-recent-regime-gate-enabled)
fi
PER_MARKET_EXPOSURE_FRAC_ARGS=()
if [ -n "${MAX_PER_MARKET_EXPOSURE_FRAC}" ]; then
  PER_MARKET_EXPOSURE_FRAC_ARGS=(--max-per-market-exposure-frac "${MAX_PER_MARKET_EXPOSURE_FRAC}")
fi

CLIP_FRAC="${CLIP_FRACTION}"
GROSS_CAP="${GROSS_CAP}"
IFS=',' read -r -a CASH_LIST <<< "${STARTING_CASHES}"

# Arms = {baseline, configB}. The baseline command is the 062901 manifest
# verbatim (every knob is an explicit CLI flag; NO --profile). configB differs
# from baseline ONLY by the flow-conditional reversal-score flags plus a wider
# convex-tail max-ask (0.30 vs 0.08); it pays convex only on flagged-fragile
# loads instead of blanket-scaling coverage.
ARMS=(baseline configB)

# arm_tail_overrides <arm> -> MUTATES the baseline tail/reversal shell variables
# in place so each flag is emitted exactly once (clap rejects duplicate flags).
# Baseline leaves every tail var at its 062901-manifest value and disables the
# reversal-score path. configB widens tail-max-ask and turns on the
# flow-conditional reversal-score modulators with the frozen coeffs.
arm_tail_overrides() {
  # Reset to manifest-baseline values every call (vars are global; arms run in
  # subshells so mutation is local to each combo's process anyway).
  TAIL_TARGET_COV="0.50"
  TAIL_MAX_ASK="0.08"
  TAIL_REVERSAL_COV="0.00"
  TAIL_RB_COV="0.0"
  TAIL_RB_SPEND="0.0"
  TAIL_RB_UPSIDE="0.0"
  TAIL_RB_WHIP="1.0"
  TAIL_RB_REVP="1.0"
  TAIL_RB_VOL="1000000000.0"
  TAIL_RB_PATHEFF="0.0"
  # Reversal-score (flow-conditional) args, off by default.
  REVERSAL_SCORE_ARGS=()
  if [ "\$1" = "baseline" ]; then
    :
  elif [ "\$1" = "configB" ]; then
    TAIL_MAX_ASK="0.30"
    REVERSAL_SCORE_ARGS=(
      --br2-reversal-score-enabled
      --br2-reversal-score-coeffs /opt/pm/configs/reversal_score_coeffs.json
      --br2-reversal-score-cov-min 0.0
      --br2-reversal-score-cov-max 1.0
      --br2-reversal-score-size-floor 0.5
      --br2-reversal-score-size-ceiling 1.0
    )
  else
    echo "unknown arm: \$1" >&2; exit 1
  fi
}

# Build the ordered combo list. The first combo is the trainer: it trains the
# meta-calibrator once and freezes the snapshot. All later combos reuse that
# frozen snapshot so all six share the SAME freshly-trained model.
COMBOS=()
for ARM in "\${ARMS[@]}"; do
  for STARTING_CASH in "\${CASH_LIST[@]}"; do
    COMBOS+=("\${ARM}|\${STARTING_CASH}")
  done
done

SNAPSHOT_PATH="/opt/pm/artifacts/meta-calibrator-snapshot.json"

# run_combo <arm> <starting_cash> <mode:train|reuse>
run_combo() {
  local ARM="\$1"
  local STARTING_CASH="\$2"
  local MODE="\$3"
  local LOCAL_MAX_MARKETS="\${4:-0}"
  local LABEL
  local LOCAL_MARKETS="/opt/pm/markets.jsonl"
  # Eval arms trim the first EVAL_MIN_TRAIN markets (the 062901 training region)
  # so the eval window is the clean out-of-sample FULL history. The snapshot is
  # preloaded and training is forbidden, so no fitting happens on the trimmed
  # region; the trim only excludes it from the evaluated/PnL set.
  local LOCAL_MIN_TRAIN="${EVAL_MIN_TRAIN}"
  if [ "\${MODE}" = "train_only" ]; then
    LABEL="train_only"
    LOCAL_MARKETS="/opt/pm/markets-train.jsonl"
    LOCAL_MIN_TRAIN="${TRAIN_MARKETS}"
  else
    LABEL="\${ARM}_cash\${STARTING_CASH}"
    if [ -n "${LABEL_SUFFIX}" ]; then
      LABEL="\${LABEL}_${LABEL_SUFFIX}"
    fi
  fi
  local OUT_DIR="/opt/pm/results/\${LABEL}"
  mkdir -p "\${OUT_DIR}"
  arm_tail_overrides "\${ARM}"
  echo "[\$(date -u)] running \${LABEL} (arm=\${ARM} cash=\${STARTING_CASH} mode=\${MODE} max_markets=\${LOCAL_MAX_MARKETS} tail_max_ask=\${TAIL_MAX_ASK} target_cov=\${TAIL_TARGET_COV} rev_cov=\${TAIL_REVERSAL_COV} rb_cov=\${TAIL_RB_COV})"

  local EXTRA_MODEL_ARGS=()
  if [ "${DISABLE_META_CALIBRATION}" = "1" ]; then
    EXTRA_MODEL_ARGS=(--disable-meta-calibration)
  elif [ "\${MODE}" = "train_only" ]; then
    # Dedicated fast train step: fit on the first --min-train-markets and write the
    # shared snapshot. --max-markets caps the run so we skip the full ~20k eval.
    EXTRA_MODEL_ARGS=(
      --meta-training-samples-cache /opt/pm/artifacts/meta-training-samples.json
      --meta-calibrator-snapshot-out "\${SNAPSHOT_PATH}"
    )
  elif [ -n "\${SNAPSHOT_IN}" ]; then
    # Externally supplied frozen snapshot: reuse for every arm, never train.
    EXTRA_MODEL_ARGS=(--meta-calibrator-snapshot-in "\${SNAPSHOT_IN}" --forbid-meta-training)
  else
    # Full-eval arm: reuse the freshly-trained frozen snapshot; training forbidden
    # so all six arms share the SAME model and convex knobs cannot perturb it.
    EXTRA_MODEL_ARGS=(--meta-calibrator-snapshot-in "\${SNAPSHOT_PATH}" --forbid-meta-training)
  fi

  (
    while true; do
      sleep 180
      [ -f "\${OUT_DIR}/markets.jsonl" ] && aws s3 cp "\${OUT_DIR}/markets.jsonl" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/markets.jsonl" || true
      [ -f "\${OUT_DIR}/summary.json" ] && aws s3 cp "\${OUT_DIR}/summary.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/summary.json" || true
      [ -f "\${OUT_DIR}/run_manifest.json" ] && aws s3 cp "\${OUT_DIR}/run_manifest.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/run_manifest.json" || true
    done
  ) &
  local UPLOADER_PID="\$!"

  set +e
  PM_TELONEX_REGION="${REGION}" ./target/release/pm-app walk-forward \\
      --markets "\${LOCAL_MARKETS}" \\
      --strategies "${STRATEGIES}" \\
      --starting-cash "\${STARTING_CASH}" \\
      --max-clip-usdc "${MAX_CLIP}" \\
      --max-order-clip-multiplier "${MAX_ORDER_CLIP_MULTIPLIER}" \\
      --max-per-market-exposure-usdc "\${GROSS_CAP}" \\
      "\${PER_MARKET_EXPOSURE_FRAC_ARGS[@]}" \\
      --kelly-fraction "${KELLY}" \\
      --spot-symbol "${SPOT_SYMBOL}" \\
      --use-outcome-label \\
      --portfolio-mode \\
      --clip-fraction-of-equity "\${CLIP_FRAC}" \\
      --clip-drawdown-soft-pct "${CLIP_DRAWDOWN_SOFT_PCT}" \\
      --clip-drawdown-hard-pct "${CLIP_DRAWDOWN_HARD_PCT}" \\
      --clip-drawdown-min-multiplier "${CLIP_DRAWDOWN_MIN_MULTIPLIER}" \\
      "\${BR2_INTERNAL_MODEL_GATE_ARGS[@]}" \\
      --br2-participation-clip-frac "${BR2_PARTICIPATION_CLIP_FRAC}" \\
      --br2-participation-max-pair-cost "${BR2_PARTICIPATION_MAX_PAIR_COST}" \\
      --br2-participation-max-orders-per-leg "${BR2_PARTICIPATION_MAX_ORDERS_PER_LEG}" \\
      --br2-participation-max-inventory-delta-shares "${BR2_PARTICIPATION_MAX_INVENTORY_DELTA_SHARES}" \\
      --br2-participation-repair-inventory-delta-shares "${BR2_PARTICIPATION_REPAIR_INVENTORY_DELTA_SHARES}" \\
      --br2-participation-refresh-secs "${BR2_PARTICIPATION_REFRESH_SECS}" \\
      --br2-participation-stop-secs-before-close "${BR2_PARTICIPATION_STOP_SECS_BEFORE_CLOSE}" \\
      --br2-min-composite-direction "${BR2_MIN_COMPOSITE_DIRECTION}" \\
      --br2-early-clip-frac "${BR2_EARLY_CLIP_FRAC}" \\
      --br2-mid-clip-frac "${BR2_MID_CLIP_FRAC}" \\
      --br2-late-clip-frac "${BR2_LATE_CLIP_FRAC}" \\
      --br2-late-max-fires "${BR2_LATE_MAX_FIRES}" \\
      --br2-late-confirm-min-model-confidence "${BR2_LATE_CONFIRM_MIN_MODEL_CONFIDENCE}" \\
      --br2-late-confirm-max-model-risk "${BR2_LATE_CONFIRM_MAX_MODEL_RISK}" \\
      --br2-late-confirm-min-model-side-p "${BR2_LATE_CONFIRM_MIN_MODEL_SIDE_P}" \\
      --br2-late-confirm-min-model-edge "${BR2_LATE_CONFIRM_MIN_MODEL_EDGE}" \\
      --br2-late-confirm-min-book-skew "${BR2_LATE_CONFIRM_MIN_BOOK_SKEW}" \\
      --br2-late-confirm-max-whipsaw-score "${BR2_LATE_CONFIRM_MAX_WHIPSAW_SCORE}" \\
      --br2-late-confirm-min-realized-vol-180s-bps "${BR2_LATE_CONFIRM_MIN_REALIZED_VOL_180S_BPS}" \\
      --br2-late-confirm-max-observed-range "${BR2_LATE_CONFIRM_MAX_OBSERVED_RANGE}" \\
      "\${BR2_RECENT_REGIME_GATE_ARGS[@]}" \\
      --br2-recent-regime-gate-min-edge "${BR2_RECENT_REGIME_GATE_MIN_EDGE}" \\
      --br2-high-skew-clip-frac "${BR2_HIGH_SKEW_CLIP_FRAC}" \\
      --br2-high-skew-max-clips "${BR2_HIGH_SKEW_MAX_CLIPS}" \\
      --br2-high-skew-max-whipsaw-score "${BR2_HIGH_SKEW_MAX_WHIPSAW_SCORE}" \\
      --br2-high-skew-min-realized-vol-180s-bps "${BR2_HIGH_SKEW_MIN_REALIZED_VOL_180S_BPS}" \\
      --br2-late-favourite-start-secs "${BR2_LATE_FAVOURITE_START_SECS}" \\
      --br2-late-favourite-threshold "${BR2_LATE_FAVOURITE_THRESHOLD}" \\
      --br2-late-favourite-min-ask "${BR2_LATE_FAVOURITE_MIN_ASK}" \\
      --br2-late-favourite-max-ask "${BR2_LATE_FAVOURITE_MAX_ASK}" \\
      --br2-late-favourite-clip-frac "${BR2_LATE_FAVOURITE_CLIP_FRAC}" \\
      --br2-late-favourite-high-cert-clip-frac "${BR2_LATE_FAVOURITE_HIGH_CERT_CLIP_FRAC}" \\
      --br2-late-favourite-high-cert-full-clip-edge "${BR2_LATE_FAVOURITE_HIGH_CERT_FULL_CLIP_EDGE}" \\
      --br2-late-favourite-max-clips "${BR2_LATE_FAVOURITE_MAX_CLIPS}" \\
      --br2-late-favourite-min-sustain-secs "${BR2_LATE_FAVOURITE_MIN_SUSTAIN_SECS}" \\
      --br2-late-favourite-sweep-depth "${BR2_LATE_FAVOURITE_SWEEP_DEPTH}" \\
      --br2-late-favourite-min-model-confidence "${BR2_LATE_FAVOURITE_MIN_MODEL_CONFIDENCE}" \\
      --br2-late-favourite-min-model-direction-abs "${BR2_LATE_FAVOURITE_MIN_MODEL_DIRECTION_ABS}" \\
      --br2-late-favourite-max-model-risk "${BR2_LATE_FAVOURITE_MAX_MODEL_RISK}" \\
      --br2-late-favourite-min-model-side-p "${BR2_LATE_FAVOURITE_MIN_MODEL_SIDE_P}" \\
      --br2-late-favourite-min-model-edge "${BR2_LATE_FAVOURITE_MIN_MODEL_EDGE}" \\
      --br2-late-favourite-high-cert-min-model-edge "${BR2_LATE_FAVOURITE_HIGH_CERT_MIN_MODEL_EDGE}" \\
      "\${BR2_LATE_FAVOURITE_HIGH_CERT_BYPASS_MODEL_EDGE_ARGS[@]}" \\
      --br2-late-favourite-max-whipsaw-score "${BR2_LATE_FAVOURITE_MAX_WHIPSAW_SCORE}" \\
      --br2-late-favourite-max-reversal-pressure "${BR2_LATE_FAVOURITE_MAX_REVERSAL_PRESSURE}" \\
      --br2-late-favourite-min-path-efficiency "${BR2_LATE_FAVOURITE_MIN_PATH_EFFICIENCY}" \\
      --br2-late-favourite-min-realized-vol-180s-bps "${BR2_LATE_FAVOURITE_MIN_REALIZED_VOL_180S_BPS}" \\
      --br2-late-favourite-max-observed-range "${BR2_LATE_FAVOURITE_MAX_OBSERVED_RANGE}" \\
      --br2-late-favourite-range-soft-throttle "${BR2_LATE_FAVOURITE_RANGE_SOFT_THROTTLE}" \\
      --br2-late-favourite-range-hard-throttle "${BR2_LATE_FAVOURITE_RANGE_HARD_THROTTLE}" \\
      --br2-late-favourite-range-extra-edge "${BR2_LATE_FAVOURITE_RANGE_EXTRA_EDGE}" \\
      --br2-late-favourite-range-extra-confidence "${BR2_LATE_FAVOURITE_RANGE_EXTRA_CONFIDENCE}" \\
      --br2-late-favourite-max-adverse-fast-momentum "${BR2_LATE_FAVOURITE_MAX_ADVERSE_FAST_MOMENTUM}" \\
      --br2-late-favourite-max-adverse-broad-momentum "${BR2_LATE_FAVOURITE_MAX_ADVERSE_BROAD_MOMENTUM}" \\
      --br2-late-favourite-max-entry-pullback "${BR2_LATE_FAVOURITE_MAX_ENTRY_PULLBACK}" \\
      --br2-late-favourite-max-avg-entry-drawdown "${BR2_LATE_FAVOURITE_MAX_AVG_ENTRY_DRAWDOWN}" \\
      --br2-tail-clip-frac "${BR2_TAIL_CLIP_FRAC}" \\
      --br2-tail-max-clips "${BR2_TAIL_MAX_CLIPS}" \\
      --br2-tail-sweep-depth "${BR2_TAIL_SWEEP_DEPTH}" \\
      --br2-tail-min-ask "${BR2_TAIL_MIN_ASK}" \\
      --br2-tail-max-ask "\${TAIL_MAX_ASK}" \\
      --br2-tail-min-seconds-to-close "${BR2_TAIL_MIN_SECONDS_TO_CLOSE}" \\
      --br2-tail-min-favourite-unrealized-edge "${BR2_TAIL_MIN_FAVOURITE_UNREALIZED_EDGE}" \\
      --br2-tail-min-observed-range "${BR2_TAIL_MIN_OBSERVED_RANGE}" \\
      --br2-tail-target-favourite-loss-coverage-frac "\${TAIL_TARGET_COV}" \\
      --br2-tail-reversal-coverage-frac "\${TAIL_REVERSAL_COV}" \\
      --br2-tail-reversal-min-seconds-to-close "${BR2_TAIL_REVERSAL_MIN_SECONDS_TO_CLOSE}" \\
      --br2-tail-reversal-max-seconds-to-close "${BR2_TAIL_REVERSAL_MAX_SECONDS_TO_CLOSE}" \\
      --br2-tail-reversal-min-favourite-ask "${BR2_TAIL_REVERSAL_MIN_FAVOURITE_ASK}" \\
      --br2-tail-extreme-threshold "${BR2_TAIL_EXTREME_THRESHOLD}" \\
      --br2-tail-min-skew-step "${BR2_TAIL_MIN_SKEW_STEP}" \\
      --br2-tail-budget-favourite-spend-frac "${BR2_TAIL_BUDGET_FAVOURITE_SPEND_FRAC}" \\
      --br2-tail-budget-favourite-upside-frac "${BR2_TAIL_BUDGET_FAVOURITE_UPSIDE_FRAC}" \\
      --br2-tail-regime-boost-coverage-frac "\${TAIL_RB_COV}" \\
      --br2-tail-regime-boost-budget-spend-frac "\${TAIL_RB_SPEND}" \\
      --br2-tail-regime-boost-budget-upside-frac "\${TAIL_RB_UPSIDE}" \\
      --br2-tail-regime-boost-min-whipsaw-score "\${TAIL_RB_WHIP}" \\
      --br2-tail-regime-boost-min-reversal-pressure "\${TAIL_RB_REVP}" \\
      --br2-tail-regime-boost-min-realized-vol-180s-bps "\${TAIL_RB_VOL}" \\
      --br2-tail-regime-boost-max-path-efficiency "\${TAIL_RB_PATHEFF}" \\
      "\${MODEL_GATE_ARGS[@]}" \\
      --model-gate-min-confidence "${MODEL_GATE_MIN_CONFIDENCE}" \\
      --model-gate-max-risk "${MODEL_GATE_MAX_RISK}" \\
      --model-gate-min-edge "${MODEL_GATE_MIN_EDGE}" \\
      --min-train-markets "\${LOCAL_MIN_TRAIN}" \\
      --max-markets "\${LOCAL_MAX_MARKETS}" \\
      --meta-epochs "${META_EPOCHS}" \\
      --meta-learning-rate "${META_LEARNING_RATE}" \\
      --meta-l2 "${META_L2}" \\
      --meta-weight-clip "${META_WEIGHT_CLIP}" \\
      --meta-max-fit-samples "${META_MAX_FIT_SAMPLES}" \\
      --meta-max-validation-samples "${META_MAX_VALIDATION_SAMPLES}" \\
      --meta-max-samples-per-market "${META_MAX_SAMPLES_PER_MARKET}" \\
      --meta-max-oos-evaluation-samples "${META_MAX_OOS_EVALUATION_SAMPLES}" \\
      --meta-train-min-base-p "${META_TRAIN_MIN_BASE_P}" \\
      --meta-train-max-early-penalty "${META_TRAIN_MAX_EARLY_PENALTY}" \\
      --meta-train-min-mid-distance "${META_TRAIN_MIN_MID_DISTANCE}" \\
      --max-concurrent-fetches "${MAX_CONCURRENT_FETCHES}" \\
      --replay-sample-ms "${REPLAY_SAMPLE_MS}" \\
      --taker-latency-ms "${TAKER_LATENCY_MS}" \\
      --portfolio-checkpoint-every-markets "${PORTFOLIO_CHECKPOINT_EVERY_MARKETS}" \\
      "\${LOCAL_CACHE_ARGS[@]}" \\
      "\${MARKET_CONTEXT_FEATURE_ARGS[@]}" \\
      "\${REVERSAL_SCORE_ARGS[@]}" \\
      "\${EXTRA_MODEL_ARGS[@]}" \\
      --out-markets "\${OUT_DIR}/markets.jsonl" \\
    --out-summary "\${OUT_DIR}/summary.json" \\
    2>&1 | tee "\${OUT_DIR}/run.log"
  local RUN_STATUS="\${PIPESTATUS[0]}"
  set -e
  kill "\${UPLOADER_PID}" || true
  wait "\${UPLOADER_PID}" || true

  # Explicit completion marker for monitoring. The prior harness fired spurious
  # "done" notifications while pm-app was still running, so monitoring must key
  # off WALKFORWARD_EXIT=<status> (and a market-count guard) rather than instance
  # state. Written for both success and failure so a stuck run is distinguishable.
  printf 'WALKFORWARD_EXIT=%s\n' "\${RUN_STATUS}" > "\${OUT_DIR}/STATUS"
  aws s3 cp "\${OUT_DIR}/run.log" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/run.log" || true
  aws s3 cp "\${OUT_DIR}/STATUS" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/STATUS" || true
  if [ "\${RUN_STATUS}" -ne 0 ]; then
    echo "[\$(date -u)] combo \${LABEL} FAILED status=\${RUN_STATUS}"
    echo "\${LABEL}:\${RUN_STATUS}" >> /opt/pm/results/.failures
    return "\${RUN_STATUS}"
  fi

  aws s3 cp "\${OUT_DIR}/markets.jsonl" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/markets.jsonl"
  aws s3 cp "\${OUT_DIR}/summary.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/summary.json"
  aws s3 cp "\${OUT_DIR}/run_manifest.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/run_manifest.json" || true
  echo "[\$(date -u)] combo \${LABEL} done"
}

: > /opt/pm/results/.failures

# Step 1: DEDICATED FAST TRAIN-ONLY step. Fits the meta-calibrator on the first
# --min-train-markets of the TRAIN markets list (full favourite, early data) and
# writes the shared snapshot. --max-markets caps the run so it skips the full eval.
# Skipped entirely when an external snapshot is supplied or meta is disabled.
if [ -z "\${SNAPSHOT_IN}" ] && [ "${DISABLE_META_CALIBRATION}" != "1" ]; then
  run_combo "baseline" "1000" "train_only" "${TRAIN_ONLY_MAX_MARKETS}" || true
  if [ ! -s "\${SNAPSHOT_PATH}" ]; then
    echo "[\$(date -u)] train-only step did not produce a frozen snapshot; aborting"
    aws s3 cp /var/log/pm-bootstrap.log "s3://${RESULTS_BUCKET}/results/${RUN_ID}/bootstrap.log" || true
    shutdown -h now
  fi
  aws s3 cp "\${SNAPSHOT_PATH}" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/artifacts/meta-calibrator-snapshot.json" || true
fi

# Step 2: run ALL SIX arms' full eval IN PARALLEL, capped at MAX_PARALLEL_COMBOS.
# Each evaluates the RECENT markets window with --forbid-meta-training reusing the
# shared frozen snapshot, so all six share the SAME model. No arm evaluates before
# the others start.
for ((i=0; i<\${#COMBOS[@]}; i++)); do
  COMBO="\${COMBOS[\$i]}"
  CP="\${COMBO%%|*}"
  CC="\${COMBO##*|}"
  run_combo "\${CP}" "\${CC}" "reuse" &
  # Throttle: block until fewer than MAX_PARALLEL_COMBOS background jobs remain.
  while [ "\$(jobs -rp | wc -l)" -ge "${MAX_PARALLEL_COMBOS}" ]; do
    sleep 5
  done
done
wait

if [ "${DISABLE_META_CALIBRATION}" != "1" ]; then
  aws s3 cp /opt/pm/artifacts/meta-training-samples.json "s3://${RESULTS_BUCKET}/results/${RUN_ID}/artifacts/meta-training-samples.json" || true
  aws s3 cp "\${SNAPSHOT_PATH}" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/artifacts/meta-calibrator-snapshot.json" || true
fi

if [ -s /opt/pm/results/.failures ]; then
  echo "[\$(date -u)] convex validation completed WITH FAILURES:"
  cat /opt/pm/results/.failures
  aws s3 cp /opt/pm/results/.failures "s3://${RESULTS_BUCKET}/results/${RUN_ID}/FAILURES.txt" || true
fi
aws s3 cp /var/log/pm-bootstrap.log "s3://${RESULTS_BUCKET}/results/${RUN_ID}/bootstrap.log"

echo "[\$(date -u)] convex validation complete; shutting down"
shutdown -h now
EOF
)

# EC2 user-data is capped at 16 KB. This run's bootstrap (full 062901 flag set,
# six arms) exceeds that, so stage the real script in S3 and boot a tiny stub that
# downloads and execs it.
USERDATA_S3_KEY="results/${RUN_ID}/bootstrap-userdata.sh"
USERDATA_TMP="$(mktemp)"
printf '%s\n' "$USER_DATA" > "$USERDATA_TMP"
aws s3 cp "$USERDATA_TMP" "s3://${RESULTS_BUCKET}/${USERDATA_S3_KEY}" --quiet
rm -f "$USERDATA_TMP"
echo "Staged user-data: s3://${RESULTS_BUCKET}/${USERDATA_S3_KEY} ($(printf '%s' "$USER_DATA" | wc -c) bytes)"

USER_DATA_STUB=$(cat <<STUB
#!/bin/bash
set -euo pipefail
exec > >(tee -a /var/log/pm-userdata-stub.log) 2>&1
echo "[\$(date -u)] fetching staged bootstrap for run_id=${RUN_ID}"
for i in 1 2 3 4 5; do
  aws s3 cp "s3://${RESULTS_BUCKET}/${USERDATA_S3_KEY}" /opt/pm-bootstrap.sh && break || sleep 10
done
chmod +x /opt/pm-bootstrap.sh
exec /opt/pm-bootstrap.sh
STUB
)

INSTANCE_ID=$(aws ec2 run-instances \
    --region "$REGION" \
    --image-id "$AMI" \
    --instance-type "$INSTANCE_TYPE" \
    --key-name "$KEY_NAME" \
    --subnet-id "$SUBNET_ID" \
    --security-group-ids "$SECURITY_GROUP_ID" \
    --iam-instance-profile "Name=$INSTANCE_PROFILE" \
    --instance-initiated-shutdown-behavior terminate \
    --block-device-mappings "DeviceName=/dev/xvda,Ebs={VolumeSize=${ROOT_VOLUME_GB},VolumeType=gp3,DeleteOnTermination=true}" \
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=pm-backtest-${RUN_ID}},{Key=run_id,Value=${RUN_ID}}]" \
    --user-data "$USER_DATA_STUB" \
    --query 'Instances[0].InstanceId' --output text)

echo "Launched: $INSTANCE_ID"
echo "Results: s3://${RESULTS_BUCKET}/results/${RUN_ID}/"
echo
echo "Watch:"
echo "  aws s3 ls s3://${RESULTS_BUCKET}/results/${RUN_ID}/ --recursive"
echo "  aws ec2 describe-instance-status --instance-ids ${INSTANCE_ID} --region ${REGION}"
