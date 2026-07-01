#!/usr/bin/env bash
# Launch one EC2 runner for a train-once, portfolio-grid backtest.
#
# The instance:
#   1. syncs this repo from S3,
#   2. builds pm-app,
#   3. extracts/trains the meta-calibrator once on the first N markets,
#   4. reuses the frozen snapshot for every clip/gross sizing config,
#   5. uploads summaries, market rows, logs, and model artifacts to S3,
#   6. terminates itself.
#
# Usage:
#   AWS_PROFILE=visumlabs ./scripts/launch_ec2_portfolio_grid.sh \
#     --start-date 2026-02-12 --end-date 2026-05-20 \
#     --train-markets 20000 \
#     --clip-fractions 0.003,0.005,0.0075,0.01 \
#     --gross-caps 500,750,1000
#
# Add --forbid-meta-training when launching sizing/execution sweeps that must
# reuse a frozen meta-calibrator snapshot.
set -euo pipefail

REGION="${AWS_REGION:-us-east-1}"
INSTANCE_TYPE="${INSTANCE_TYPE:-c7i.4xlarge}"
ARCH="${ARCH:-x86_64}"
USE_SPOT="${USE_SPOT:-0}"
SPOT_MAX_PRICE="${SPOT_MAX_PRICE:-}"
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
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-portfolio-grid-$$"

MARKETS_KEY=""
START_DATE=""
END_DATE=""
SLUG_PREFIXES="btc-updown-5m-"
STRATEGIES="bonereaper_v2"
PROFILE_PATH=""
STARTING_CASH="1000"
MAX_CLIP="100"
MAX_ORDER_CLIP_MULTIPLIER="6.0"
MAX_PER_MARKET_EXPOSURE_FRAC=""
KELLY="0.25"
CLIP_DRAWDOWN_SOFT_PCT="1.0"
CLIP_DRAWDOWN_HARD_PCT="1.0"
CLIP_DRAWDOWN_MIN_MULTIPLIER="0.0"
CLIP_SESSION_DRAWDOWN_SOFT_PCT="1.0"
CLIP_SESSION_DRAWDOWN_HARD_PCT="1.0"
CLIP_SESSION_DRAWDOWN_MIN_MULTIPLIER="0.0"
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
COMPETITOR_RECYCLER_CHILD_CLIP_SHARES=""
COMPETITOR_RECYCLER_MAX_LEG_SHARES=""
COMPETITOR_RECYCLER_MAX_PAIR_COST="0.970"
COMPETITOR_RECYCLER_REPAIR_DELTA_SHARES=""
COMPETITOR_RECYCLER_LEAN_DELTA_SHARES=""
COMPETITOR_RECYCLER_STOP_SECS_BEFORE_CLOSE="30.0"
COMPETITOR_RECYCLER_MIN_REGIME_REALIZED_VOL_180S_BPS="0.0"
COMPETITOR_RECYCLER_MIN_REGIME_SIGN_FLIP_RATE="0.0"
COMPETITOR_RECYCLER_MAX_REGIME_PATH_EFFICIENCY="1.0"
COMPETITOR_RECYCLER_MAX_ABS_SPOT_FLOW_30S="inf"
COMPETITOR_RECYCLER_STRESS_WARMUP_EVENTS="20"
COMPETITOR_RECYCLER_MAX_ATTRACTIVE_PAIR_FRAC_SO_FAR="inf"
COMPETITOR_RECYCLER_MIN_TOP_BID_ASK_SIZE_RATIO="0.0"
COMPETITOR_RECYCLER_STRESS_CLIP_MULTIPLIER="0.0"
COMPETITOR_RECYCLER_QUOTE_MODE="passive_bid"
ENFORCE_MODEL_GATE="true"
MODEL_GATE_MIN_CONFIDENCE="0.68"
MODEL_GATE_MAX_RISK="0.72"
MODEL_GATE_MIN_EDGE="0.00"
ENABLE_MARKET_CONTEXT_FEATURES="0"
TRAIN_MARKETS="2880"
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
SPOT_SYMBOL="BTCUSDT"
MAX_CONCURRENT_FETCHES="32"
REPLAY_SAMPLE_MS="0"
TAKER_LATENCY_MS="0"
USE_LOCAL_CACHE="0"
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
        --clip-session-drawdown-soft-pct) CLIP_SESSION_DRAWDOWN_SOFT_PCT="$2"; shift 2 ;;
        --clip-session-drawdown-hard-pct) CLIP_SESSION_DRAWDOWN_HARD_PCT="$2"; shift 2 ;;
        --clip-session-drawdown-min-multiplier) CLIP_SESSION_DRAWDOWN_MIN_MULTIPLIER="$2"; shift 2 ;;
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
        --competitor-recycler-child-clip-shares) COMPETITOR_RECYCLER_CHILD_CLIP_SHARES="$2"; shift 2 ;;
        --competitor-recycler-max-leg-shares) COMPETITOR_RECYCLER_MAX_LEG_SHARES="$2"; shift 2 ;;
        --competitor-recycler-max-pair-cost) COMPETITOR_RECYCLER_MAX_PAIR_COST="$2"; shift 2 ;;
        --competitor-recycler-repair-delta-shares) COMPETITOR_RECYCLER_REPAIR_DELTA_SHARES="$2"; shift 2 ;;
        --competitor-recycler-lean-delta-shares) COMPETITOR_RECYCLER_LEAN_DELTA_SHARES="$2"; shift 2 ;;
        --competitor-recycler-stop-secs-before-close) COMPETITOR_RECYCLER_STOP_SECS_BEFORE_CLOSE="$2"; shift 2 ;;
        --competitor-recycler-min-regime-realized-vol-180s-bps) COMPETITOR_RECYCLER_MIN_REGIME_REALIZED_VOL_180S_BPS="$2"; shift 2 ;;
        --competitor-recycler-min-regime-sign-flip-rate) COMPETITOR_RECYCLER_MIN_REGIME_SIGN_FLIP_RATE="$2"; shift 2 ;;
        --competitor-recycler-max-regime-path-efficiency) COMPETITOR_RECYCLER_MAX_REGIME_PATH_EFFICIENCY="$2"; shift 2 ;;
        --competitor-recycler-max-abs-spot-flow-30s) COMPETITOR_RECYCLER_MAX_ABS_SPOT_FLOW_30S="$2"; shift 2 ;;
        --competitor-recycler-stress-warmup-events) COMPETITOR_RECYCLER_STRESS_WARMUP_EVENTS="$2"; shift 2 ;;
        --competitor-recycler-max-attractive-pair-frac-so-far) COMPETITOR_RECYCLER_MAX_ATTRACTIVE_PAIR_FRAC_SO_FAR="$2"; shift 2 ;;
        --competitor-recycler-min-top-bid-ask-size-ratio) COMPETITOR_RECYCLER_MIN_TOP_BID_ASK_SIZE_RATIO="$2"; shift 2 ;;
        --competitor-recycler-stress-clip-multiplier) COMPETITOR_RECYCLER_STRESS_CLIP_MULTIPLIER="$2"; shift 2 ;;
        --competitor-recycler-quote-mode) COMPETITOR_RECYCLER_QUOTE_MODE="$2"; shift 2 ;;
        --enforce-model-gate) ENFORCE_MODEL_GATE="$2"; shift 2 ;;
        --model-gate-min-confidence) MODEL_GATE_MIN_CONFIDENCE="$2"; shift 2 ;;
        --model-gate-max-risk) MODEL_GATE_MAX_RISK="$2"; shift 2 ;;
        --model-gate-min-edge) MODEL_GATE_MIN_EDGE="$2"; shift 2 ;;
        --enable-market-context-features) ENABLE_MARKET_CONTEXT_FEATURES="1"; shift ;;
        --train-markets) TRAIN_MARKETS="$2"; shift 2 ;;
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
        --arch) ARCH="$2"; shift 2 ;;
        --spot) USE_SPOT="1"; shift ;;
        --spot-max-price) SPOT_MAX_PRICE="$2"; shift 2 ;;
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
if [ "$USE_SPOT" != "1" ] && [ "$LARGE_INSTANCE" = "1" ] && [ "$ALLOW_ON_DEMAND_LARGE" != "1" ]; then
    echo "Refusing on-demand $INSTANCE_TYPE launch without --allow-on-demand-large." >&2
    echo "Use --spot for interruptible research, or set ALLOW_ON_DEMAND_LARGE=1 when on-demand is intentional." >&2
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

case "$ARCH" in
    x86_64|arm64) ;;
    *) echo "--arch must be x86_64 or arm64" >&2; exit 1 ;;
esac

AMI_ARCH="$ARCH"
AMI_PARAM="/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-${AMI_ARCH}"

if [ "$ARCH" = "arm64" ] && [[ "$PM_APP_BINARY_S3_URI" == *x86* ]]; then
    echo "arm64 launch cannot use an x86 pm-app binary: $PM_APP_BINARY_S3_URI" >&2
    exit 1
fi
if [ "$ARCH" = "x86_64" ] && [[ "$PM_APP_BINARY_S3_URI" == *arm64* || "$PM_APP_BINARY_S3_URI" == *aarch64* ]]; then
    echo "x86_64 launch cannot use an ARM pm-app binary: $PM_APP_BINARY_S3_URI" >&2
    exit 1
fi

AMI=$(aws ssm get-parameter \
    --region "$REGION" \
    --name "$AMI_PARAM" \
    --query Parameter.Value --output text)

echo "AMI: $AMI"
echo "AMI arch: $ARCH"
echo "Run ID: $RUN_ID"
echo "Source git SHA: $SOURCE_GIT_SHA"
INSTANCE_MARKET_OPTIONS_ARGS=()
if [ "$USE_SPOT" = "1" ]; then
    if [ -n "$SPOT_MAX_PRICE" ]; then
        INSTANCE_MARKET_OPTIONS_ARGS=(
            --instance-market-options
            "MarketType=spot,SpotOptions={MaxPrice=${SPOT_MAX_PRICE},SpotInstanceType=one-time,InstanceInterruptionBehavior=terminate}"
        )
    else
        INSTANCE_MARKET_OPTIONS_ARGS=(
            --instance-market-options
            "MarketType=spot,SpotOptions={SpotInstanceType=one-time,InstanceInterruptionBehavior=terminate}"
        )
    fi
    echo "EC2 market: spot"
    [ -n "$SPOT_MAX_PRICE" ] && echo "Spot max price: $SPOT_MAX_PRICE"
else
    echo "EC2 market: on-demand"
fi
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
COMPETITOR_RECYCLER_ARGS=(
  --competitor-recycler-max-pair-cost "${COMPETITOR_RECYCLER_MAX_PAIR_COST}"
  --competitor-recycler-stop-secs-before-close "${COMPETITOR_RECYCLER_STOP_SECS_BEFORE_CLOSE}"
  --competitor-recycler-min-regime-realized-vol-180s-bps "${COMPETITOR_RECYCLER_MIN_REGIME_REALIZED_VOL_180S_BPS}"
  --competitor-recycler-min-regime-sign-flip-rate "${COMPETITOR_RECYCLER_MIN_REGIME_SIGN_FLIP_RATE}"
  --competitor-recycler-max-regime-path-efficiency "${COMPETITOR_RECYCLER_MAX_REGIME_PATH_EFFICIENCY}"
  --competitor-recycler-max-abs-spot-flow-30s "${COMPETITOR_RECYCLER_MAX_ABS_SPOT_FLOW_30S}"
  --competitor-recycler-stress-warmup-events "${COMPETITOR_RECYCLER_STRESS_WARMUP_EVENTS}"
  --competitor-recycler-max-attractive-pair-frac-so-far "${COMPETITOR_RECYCLER_MAX_ATTRACTIVE_PAIR_FRAC_SO_FAR}"
  --competitor-recycler-min-top-bid-ask-size-ratio "${COMPETITOR_RECYCLER_MIN_TOP_BID_ASK_SIZE_RATIO}"
  --competitor-recycler-stress-clip-multiplier "${COMPETITOR_RECYCLER_STRESS_CLIP_MULTIPLIER}"
  --competitor-recycler-quote-mode "${COMPETITOR_RECYCLER_QUOTE_MODE}"
)
if [ -n "${COMPETITOR_RECYCLER_CHILD_CLIP_SHARES}" ]; then
  COMPETITOR_RECYCLER_ARGS+=(--competitor-recycler-child-clip-shares "${COMPETITOR_RECYCLER_CHILD_CLIP_SHARES}")
fi
if [ -n "${COMPETITOR_RECYCLER_MAX_LEG_SHARES}" ]; then
  COMPETITOR_RECYCLER_ARGS+=(--competitor-recycler-max-leg-shares "${COMPETITOR_RECYCLER_MAX_LEG_SHARES}")
fi
if [ -n "${COMPETITOR_RECYCLER_REPAIR_DELTA_SHARES}" ]; then
  COMPETITOR_RECYCLER_ARGS+=(--competitor-recycler-repair-delta-shares "${COMPETITOR_RECYCLER_REPAIR_DELTA_SHARES}")
fi
if [ -n "${COMPETITOR_RECYCLER_LEAN_DELTA_SHARES}" ]; then
  COMPETITOR_RECYCLER_ARGS+=(--competitor-recycler-lean-delta-shares "${COMPETITOR_RECYCLER_LEAN_DELTA_SHARES}")
fi
FORBID_META_TRAINING_ARGS=()
if [ "${FORBID_META_TRAINING}" = "1" ]; then
  FORBID_META_TRAINING_ARGS=(--forbid-meta-training)
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
if [ -n "${PROFILE_PATH}" ]; then
  echo "[\$(date -u)] ignoring --profile ${PROFILE_PATH}; launcher CLI knobs are authoritative for sweeps"
fi
PER_MARKET_EXPOSURE_FRAC_ARGS=()
if [ -n "${MAX_PER_MARKET_EXPOSURE_FRAC}" ]; then
  PER_MARKET_EXPOSURE_FRAC_ARGS=(--max-per-market-exposure-frac "${MAX_PER_MARKET_EXPOSURE_FRAC}")
fi

IFS=',' read -r -a CLIPS <<< "${CLIP_FRACTIONS}"
IFS=',' read -r -a GROSS_CAPS <<< "${GROSS_CAPS}"
FIRST=1

for CLIP_FRAC in "\${CLIPS[@]}"; do
  for GROSS_CAP in "\${GROSS_CAPS[@]}"; do
    LABEL="clip_\${CLIP_FRAC//./p}_gross_\${GROSS_CAP//./p}"
    if [ -n "${MAX_PER_MARKET_EXPOSURE_FRAC}" ]; then
      LABEL="\${LABEL}_expfrac_${MAX_PER_MARKET_EXPOSURE_FRAC//./p}"
    fi
    if [ "${TAKER_LATENCY_MS}" != "0" ]; then
      LABEL="\${LABEL}_lat${TAKER_LATENCY_MS}ms"
    fi
    if [ -n "${LABEL_SUFFIX}" ]; then
      LABEL="\${LABEL}_${LABEL_SUFFIX}"
    fi
    OUT_DIR="/opt/pm/results/\${LABEL}"
    mkdir -p "\${OUT_DIR}"
    echo "[\$(date -u)] running \${LABEL}"

    EXTRA_MODEL_ARGS=()
    if [ "${DISABLE_META_CALIBRATION}" = "1" ]; then
      EXTRA_MODEL_ARGS=(
        --disable-meta-calibration
      )
    elif [ -n "\${SNAPSHOT_IN}" ]; then
      EXTRA_MODEL_ARGS=(
        --meta-calibrator-snapshot-in "\${SNAPSHOT_IN}"
      )
    elif [ "\${FIRST}" = "1" ]; then
      EXTRA_MODEL_ARGS=(
        --meta-training-samples-cache /opt/pm/artifacts/meta-training-samples.json
        --meta-calibrator-snapshot-out /opt/pm/artifacts/meta-calibrator-snapshot.json
      )
      FIRST=0
    else
      EXTRA_MODEL_ARGS=(
        --meta-calibrator-snapshot-in /opt/pm/artifacts/meta-calibrator-snapshot.json
      )
    fi

    (
      while true; do
        sleep 180
        [ -f "\${OUT_DIR}/markets.jsonl" ] && aws s3 cp "\${OUT_DIR}/markets.jsonl" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/markets.jsonl" || true
        [ -f "\${OUT_DIR}/summary.json" ] && aws s3 cp "\${OUT_DIR}/summary.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/summary.json" || true
        [ -f "\${OUT_DIR}/run_manifest.json" ] && aws s3 cp "\${OUT_DIR}/run_manifest.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/run_manifest.json" || true
      done
    ) &
    UPLOADER_PID="\$!"

    set +e
    PM_TELONEX_REGION="${REGION}" ./target/release/pm-app walk-forward \\
      --markets /opt/pm/markets.jsonl \\
      --strategies "${STRATEGIES}" \\
      --starting-cash "${STARTING_CASH}" \\
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
      --clip-session-drawdown-soft-pct "${CLIP_SESSION_DRAWDOWN_SOFT_PCT}" \\
      --clip-session-drawdown-hard-pct "${CLIP_SESSION_DRAWDOWN_HARD_PCT}" \\
      --clip-session-drawdown-min-multiplier "${CLIP_SESSION_DRAWDOWN_MIN_MULTIPLIER}" \\
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
      --br2-tail-max-ask "${BR2_TAIL_MAX_ASK}" \\
      --br2-tail-min-seconds-to-close "${BR2_TAIL_MIN_SECONDS_TO_CLOSE}" \\
      --br2-tail-min-favourite-unrealized-edge "${BR2_TAIL_MIN_FAVOURITE_UNREALIZED_EDGE}" \\
      --br2-tail-min-observed-range "${BR2_TAIL_MIN_OBSERVED_RANGE}" \\
      --br2-tail-target-favourite-loss-coverage-frac "${BR2_TAIL_TARGET_FAVOURITE_LOSS_COVERAGE_FRAC}" \\
      --br2-tail-reversal-coverage-frac "${BR2_TAIL_REVERSAL_COVERAGE_FRAC}" \\
      --br2-tail-reversal-min-seconds-to-close "${BR2_TAIL_REVERSAL_MIN_SECONDS_TO_CLOSE}" \\
      --br2-tail-reversal-max-seconds-to-close "${BR2_TAIL_REVERSAL_MAX_SECONDS_TO_CLOSE}" \\
      --br2-tail-reversal-min-favourite-ask "${BR2_TAIL_REVERSAL_MIN_FAVOURITE_ASK}" \\
      --br2-tail-extreme-threshold "${BR2_TAIL_EXTREME_THRESHOLD}" \\
      --br2-tail-min-skew-step "${BR2_TAIL_MIN_SKEW_STEP}" \\
      --br2-tail-budget-favourite-spend-frac "${BR2_TAIL_BUDGET_FAVOURITE_SPEND_FRAC}" \\
      --br2-tail-budget-favourite-upside-frac "${BR2_TAIL_BUDGET_FAVOURITE_UPSIDE_FRAC}" \\
      --br2-tail-regime-boost-coverage-frac "${BR2_TAIL_REGIME_BOOST_COVERAGE_FRAC}" \\
      --br2-tail-regime-boost-budget-spend-frac "${BR2_TAIL_REGIME_BOOST_BUDGET_SPEND_FRAC}" \\
      --br2-tail-regime-boost-budget-upside-frac "${BR2_TAIL_REGIME_BOOST_BUDGET_UPSIDE_FRAC}" \\
      --br2-tail-regime-boost-min-whipsaw-score "${BR2_TAIL_REGIME_BOOST_MIN_WHIPSAW_SCORE}" \\
      --br2-tail-regime-boost-min-reversal-pressure "${BR2_TAIL_REGIME_BOOST_MIN_REVERSAL_PRESSURE}" \\
      --br2-tail-regime-boost-min-realized-vol-180s-bps "${BR2_TAIL_REGIME_BOOST_MIN_REALIZED_VOL_180S_BPS}" \\
      --br2-tail-regime-boost-max-path-efficiency "${BR2_TAIL_REGIME_BOOST_MAX_PATH_EFFICIENCY}" \\
      "\${COMPETITOR_RECYCLER_ARGS[@]}" \\
      "\${MODEL_GATE_ARGS[@]}" \\
      --model-gate-min-confidence "${MODEL_GATE_MIN_CONFIDENCE}" \\
      --model-gate-max-risk "${MODEL_GATE_MAX_RISK}" \\
      --model-gate-min-edge "${MODEL_GATE_MIN_EDGE}" \\
      --min-train-markets "${TRAIN_MARKETS}" \\
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
      "\${FORBID_META_TRAINING_ARGS[@]}" \\
      "\${MARKET_CONTEXT_FEATURE_ARGS[@]}" \\
      "\${EXTRA_MODEL_ARGS[@]}" \\
      --out-markets "\${OUT_DIR}/markets.jsonl" \\
      --out-summary "\${OUT_DIR}/summary.json" \\
      2>&1 | tee "\${OUT_DIR}/run.log"
    RUN_STATUS="\${PIPESTATUS[0]}"
    set -e
    kill "\${UPLOADER_PID}" || true
    wait "\${UPLOADER_PID}" || true
    if [ "\${RUN_STATUS}" -ne 0 ]; then
      aws s3 cp "\${OUT_DIR}/run.log" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/run.log" || true
      exit "\${RUN_STATUS}"
    fi

    aws s3 cp "\${OUT_DIR}/markets.jsonl" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/markets.jsonl"
    aws s3 cp "\${OUT_DIR}/summary.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/summary.json"
    aws s3 cp "\${OUT_DIR}/run_manifest.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/run_manifest.json" || true
    aws s3 cp "\${OUT_DIR}/run.log" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${LABEL}/run.log"
  done
done

if [ "${DISABLE_META_CALIBRATION}" != "1" ]; then
  aws s3 cp /opt/pm/artifacts/meta-training-samples.json "s3://${RESULTS_BUCKET}/results/${RUN_ID}/artifacts/meta-training-samples.json" || true
  aws s3 cp /opt/pm/artifacts/meta-calibrator-snapshot.json "s3://${RESULTS_BUCKET}/results/${RUN_ID}/artifacts/meta-calibrator-snapshot.json" || true
fi
aws s3 cp /var/log/pm-bootstrap.log "s3://${RESULTS_BUCKET}/results/${RUN_ID}/bootstrap.log"

echo "[\$(date -u)] portfolio grid complete; shutting down"
shutdown -h now
EOF
)

RUN_INSTANCE_ARGS=(
    --region "$REGION"
    --image-id "$AMI"
    --instance-type "$INSTANCE_TYPE"
    --key-name "$KEY_NAME"
    --subnet-id "$SUBNET_ID"
    --security-group-ids "$SECURITY_GROUP_ID"
    --iam-instance-profile "Name=$INSTANCE_PROFILE"
    --instance-initiated-shutdown-behavior terminate
    --block-device-mappings "DeviceName=/dev/xvda,Ebs={VolumeSize=${ROOT_VOLUME_GB},VolumeType=gp3,DeleteOnTermination=true}"
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=pm-backtest-${RUN_ID}},{Key=run_id,Value=${RUN_ID}}]"
    --user-data "$USER_DATA"
    --query 'Instances[0].InstanceId'
    --output text
)
if [ "$USE_SPOT" = "1" ]; then
    RUN_INSTANCE_ARGS+=("${INSTANCE_MARKET_OPTIONS_ARGS[@]}")
fi

INSTANCE_ID=$(aws ec2 run-instances "${RUN_INSTANCE_ARGS[@]}")

echo "Launched: $INSTANCE_ID"
echo "Results: s3://${RESULTS_BUCKET}/results/${RUN_ID}/"
echo
echo "Watch:"
echo "  aws s3 ls s3://${RESULTS_BUCKET}/results/${RUN_ID}/ --recursive"
echo "  aws ec2 describe-instance-status --instance-ids ${INSTANCE_ID} --region ${REGION}"
