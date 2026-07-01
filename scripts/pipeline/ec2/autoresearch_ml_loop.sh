#!/usr/bin/env bash
# Minimal autoresearch harness for pm-app ML calibration experiments.
# Defaults to dry-run. Use --execute to create detached worktrees and run
# candidate commands.
set -euo pipefail

usage() {
    cat <<'USAGE'
Usage:
  scripts/autoresearch_ml_loop.sh plan
  scripts/autoresearch_ml_loop.sh gate BASELINE_SUMMARY CANDIDATE_SUMMARY [STRATEGY]
  scripts/autoresearch_ml_loop.sh run --budget N --baseline SUMMARY --commands FILE [--strategy NAME] [--execute]

Environment:
  AUTORESEARCH_LOG_LOSS_DELTA   Required log-loss improvement. Default: 0.0
  AUTORESEARCH_BRIER_TOL        Allowed Brier regression. Default: 0.0
  AUTORESEARCH_MIN_FREE_GB      Disk headroom before a run. Default: 25
  AUTORESEARCH_RESULTS_DIR      Run root. Default: /tmp/pm-autoresearch-ml-loop/<timestamp>

Command-file contract for run:
  Each non-empty, non-comment line is executed in an isolated git worktree.
  It must write candidate summary JSON to $AUTORESEARCH_CANDIDATE_SUMMARY.
USAGE
}

die() {
    echo "error: $*" >&2
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

free_gb() {
    local path="$1"
    mkdir -p "$path"
    df -Pk "$path" | awk 'NR == 2 { printf "%.0f", $4 / 1024 / 1024 }'
}

ensure_free_disk() {
    local path="$1"
    local min_gb="${2:-25}"
    local got_gb
    got_gb="$(free_gb "$path")"
    if awk "BEGIN { exit !($got_gb < $min_gb) }"; then
        die "only ${got_gb}GB free at ${path}; need at least ${min_gb}GB"
    fi
}

jq_num() {
    local file="$1"
    local expr="$2"
    local value
    value="$(jq -er "$expr // empty" "$file")" || return 1
    [ -n "$value" ] || return 1
    printf '%s\n' "$value"
}

compare_less_by_delta() {
    local candidate="$1"
    local baseline="$2"
    local delta="$3"
    awk "BEGIN { exit !(($candidate) < (($baseline) - ($delta))) }"
}

compare_not_greater_than_tol() {
    local candidate="$1"
    local baseline="$2"
    local tol="$3"
    awk "BEGIN { exit !(($candidate) <= (($baseline) + ($tol))) }"
}

cmd_plan() {
    cat <<'PLAN'
Autoresearch loop:
  1. Spend a fixed budget of candidate attempts, usually 3 to 6.
  2. Keep market set, strategy set, sizing, fees, and data mode fixed.
  3. Run each candidate in an isolated detached git worktree.
  4. Candidate command writes summary.json to $AUTORESEARCH_CANDIDATE_SUMMARY.
  5. Gate on lower meta_calibration.oos.calibrated_log_loss and non-regressed
     meta_calibration.oos.calibrated_brier.
  6. Use PnL, drawdown, Sharpe, and fill-tag attribution only after ML passes.
  7. PASS leaves the worktree for review; FAIL removes the worktree.

Useful commands:
  scripts/autoresearch_ml_loop.sh gate /tmp/base.json /tmp/candidate.json bonereaper_v2
  scripts/autoresearch_ml_loop.sh run --budget 3 --baseline /tmp/base.json --commands /tmp/candidates.txt --strategy bonereaper_v2
  scripts/autoresearch_ml_loop.sh run --budget 3 --baseline /tmp/base.json --commands /tmp/candidates.txt --strategy bonereaper_v2 --execute
PLAN
}

cmd_gate() {
    need_cmd jq
    local baseline="${1:-}"
    local candidate="${2:-}"
    local strategy="${3:-bonereaper_v2}"
    [ -f "$baseline" ] || die "baseline summary not found: $baseline"
    [ -f "$candidate" ] || die "candidate summary not found: $candidate"

    local delta="${AUTORESEARCH_LOG_LOSS_DELTA:-0.0}"
    local brier_tol="${AUTORESEARCH_BRIER_TOL:-0.0}"

    local base_ll cand_ll base_brier cand_brier
    base_ll="$(jq_num "$baseline" '.meta_calibration.oos.calibrated_log_loss')" \
        || die "baseline missing meta_calibration.oos.calibrated_log_loss"
    cand_ll="$(jq_num "$candidate" '.meta_calibration.oos.calibrated_log_loss')" \
        || die "candidate missing meta_calibration.oos.calibrated_log_loss"
    base_brier="$(jq_num "$baseline" '.meta_calibration.oos.calibrated_brier')" \
        || die "baseline missing meta_calibration.oos.calibrated_brier"
    cand_brier="$(jq_num "$candidate" '.meta_calibration.oos.calibrated_brier')" \
        || die "candidate missing meta_calibration.oos.calibrated_brier"

    local ll_pass=0
    local brier_pass=0
    compare_less_by_delta "$cand_ll" "$base_ll" "$delta" && ll_pass=1
    compare_not_greater_than_tol "$cand_brier" "$base_brier" "$brier_tol" && brier_pass=1

    local base_return cand_return base_dd cand_dd base_sharpe cand_sharpe
    base_return="$(jq -r --arg strategy "$strategy" '.per_strategy[$strategy].compounded_return_pct // "NA"' "$baseline")"
    cand_return="$(jq -r --arg strategy "$strategy" '.per_strategy[$strategy].compounded_return_pct // "NA"' "$candidate")"
    base_dd="$(jq -r --arg strategy "$strategy" '.per_strategy[$strategy].path_max_drawdown_pct // "NA"' "$baseline")"
    cand_dd="$(jq -r --arg strategy "$strategy" '.per_strategy[$strategy].path_max_drawdown_pct // "NA"' "$candidate")"
    base_sharpe="$(jq -r --arg strategy "$strategy" '.per_strategy[$strategy].sharpe_ratio // "NA"' "$baseline")"
    cand_sharpe="$(jq -r --arg strategy "$strategy" '.per_strategy[$strategy].sharpe_ratio // "NA"' "$candidate")"

    echo "strategy: $strategy"
    echo "oos.calibrated_log_loss: baseline=$base_ll candidate=$cand_ll required_delta=$delta"
    echo "oos.calibrated_brier: baseline=$base_brier candidate=$cand_brier tolerance=$brier_tol"
    echo "secondary.compounded_return_pct: baseline=$base_return candidate=$cand_return"
    echo "secondary.path_max_drawdown_pct: baseline=$base_dd candidate=$cand_dd"
    echo "secondary.sharpe_ratio: baseline=$base_sharpe candidate=$cand_sharpe"

    if [ "$ll_pass" -eq 1 ] && [ "$brier_pass" -eq 1 ]; then
        echo "PASS"
        return 0
    fi

    echo "FAIL"
    return 2
}

cmd_run() {
    need_cmd git
    need_cmd jq

    local budget=""
    local baseline=""
    local commands=""
    local strategy="bonereaper_v2"
    local execute="0"
    local keep_fail="0"
    local base_ref="HEAD"
    local min_free_gb="${AUTORESEARCH_MIN_FREE_GB:-25}"
    local results_dir="${AUTORESEARCH_RESULTS_DIR:-/tmp/pm-autoresearch-ml-loop/$(date -u +%Y%m%dT%H%M%SZ)}"

    while [ $# -gt 0 ]; do
        case "$1" in
            --budget) budget="$2"; shift 2 ;;
            --baseline) baseline="$2"; shift 2 ;;
            --commands) commands="$2"; shift 2 ;;
            --strategy) strategy="$2"; shift 2 ;;
            --execute) execute="1"; shift ;;
            --keep-fail) keep_fail="1"; shift ;;
            --base-ref) base_ref="$2"; shift 2 ;;
            --results-dir) results_dir="$2"; shift 2 ;;
            --min-free-gb) min_free_gb="$2"; shift 2 ;;
            *) die "unknown run arg: $1" ;;
        esac
    done

    [ -n "$budget" ] || die "--budget is required"
    [ -f "$baseline" ] || die "--baseline summary not found: $baseline"
    [ -f "$commands" ] || die "--commands file not found: $commands"
    case "$budget" in
        ''|*[!0-9]*) die "--budget must be a positive integer" ;;
    esac
    [ "$budget" -gt 0 ] || die "--budget must be > 0"

    mkdir -p "$results_dir"
    ensure_free_disk "$results_dir" "$min_free_gb"

    local candidates=()
    local line
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line#"${line%%[![:space:]]*}"}"
        line="${line%"${line##*[![:space:]]}"}"
        [ -z "$line" ] && continue
        case "$line" in \#*) continue ;; esac
        candidates+=("$line")
    done < "$commands"

    [ "${#candidates[@]}" -gt 0 ] || die "no candidate commands in $commands"

    local limit="$budget"
    if [ "$limit" -gt "${#candidates[@]}" ]; then
        limit="${#candidates[@]}"
    fi

    echo "results_dir: $results_dir"
    echo "budget: $limit of ${#candidates[@]} candidate commands"
    echo "base_ref: $base_ref"
    echo "mode: $([ "$execute" = "1" ] && echo execute || echo dry-run)"

    local i
    for ((i = 0; i < limit; i++)); do
        local n label out_dir worktree summary status_file cmd
        n="$(printf '%02d' "$((i + 1))")"
        label="autoresearch_${n}"
        out_dir="$results_dir/$label"
        worktree="$results_dir/worktrees/$label"
        summary="$out_dir/summary.json"
        status_file="$out_dir/status.txt"
        cmd="${candidates[$i]}"

        mkdir -p "$out_dir"
        printf '%s\n' "$cmd" > "$out_dir/command.sh"

        echo
        echo "== $label =="
        echo "summary: $summary"
        if [ "$execute" != "1" ]; then
            echo "dry-run command:"
            echo "  $cmd"
            continue
        fi

        ensure_free_disk "$results_dir" "$min_free_gb"
        git worktree add --detach "$worktree" "$base_ref" >/dev/null

        set +e
        (
            cd "$worktree"
            AUTORESEARCH_LABEL="$label" \
            AUTORESEARCH_OUT_DIR="$out_dir" \
            AUTORESEARCH_CANDIDATE_SUMMARY="$summary" \
            bash -lc "$cmd"
        ) 2>&1 | tee "$out_dir/run.log"
        local run_status="${PIPESTATUS[0]}"
        set -e

        if [ "$run_status" -ne 0 ]; then
            echo "ERROR run_status=$run_status" | tee "$status_file"
            if [ "$keep_fail" != "1" ]; then
                git worktree remove --force "$worktree" >/dev/null 2>&1 || true
            fi
            continue
        fi

        if [ ! -f "$summary" ]; then
            echo "ERROR missing candidate summary: $summary" | tee "$status_file"
            if [ "$keep_fail" != "1" ]; then
                git worktree remove --force "$worktree" >/dev/null 2>&1 || true
            fi
            continue
        fi

        set +e
        "$0" gate "$baseline" "$summary" "$strategy" | tee "$out_dir/gate.log"
        local gate_status="${PIPESTATUS[0]}"
        set -e

        if [ "$gate_status" -eq 0 ]; then
            echo "PASS keep worktree=$worktree" | tee "$status_file"
        else
            echo "FAIL rollback by removing worktree=$worktree" | tee "$status_file"
            if [ "$keep_fail" != "1" ]; then
                git worktree remove --force "$worktree" >/dev/null 2>&1 || true
            fi
        fi
    done
}

main() {
    local cmd="${1:-}"
    case "$cmd" in
        plan) shift; cmd_plan "$@" ;;
        gate) shift; cmd_gate "$@" ;;
        run) shift; cmd_run "$@" ;;
        -h|--help|help|'') usage ;;
        *) die "unknown command: $cmd" ;;
    esac
}

main "$@"
