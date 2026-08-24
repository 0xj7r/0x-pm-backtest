#!/usr/bin/env bash
# Deploy verification (parity F4). Run ON the box (Dublin) as the unit user.
# For each managed unit:
#   a. FLAGS: the running /proc/<MainPID>/cmdline contains every flag token
#      the repo SSOT prescribes (the flags files for the shadow streams;
#      the repo unit file's ExecStart args for everything else)
#   b. ENV: /proc/<MainPID>/environ (PM_*/SHADOW_* vars only) vs the repo
#      unit file's Environment= lines: values that differ AND vars present
#      in the process but undeclared in the repo unit (catches the
#      EnvironmentFile-precedence class and stale deployed units)
# Then:
#   c. GIT: ~/pm-backtest, ~/deploy-main/polymarket-backtest,
#      ~/deploy-main/polymarket-agent: fetch origin, FAIL if HEAD !=
#      origin/main or tracked files are dirty
# One PASS/FAIL line per check; exit nonzero if any FAIL. Values are printed
# for PM_*/SHADOW_* vars only, never for names matching KEY/SECRET/TOKEN/
# PRIVATE/PASS.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UNIT_DIR="$SCRIPT_DIR/systemd"

UNITS=(
  pm-shadow-final
  pm-shadow-final-b
  pm-shadow-15m
  pm-fast-live
  pm-shadow-exec-paper
  pm-shadow-consensus
  pm-live-collector
)

FAILS=0
pass() { echo "PASS $*"; }
fail() { echo "FAIL $*"; FAILS=$((FAILS + 1)); }

value_printable() {
  # Never print values whose var name smells like a credential, even within
  # the PM_*/SHADOW_* allowlist.
  case "$1" in
    *KEY*|*SECRET*|*TOKEN*|*PRIVATE*|*PASS*) return 1 ;;
    *) return 0 ;;
  esac
}

unit_pid() {
  local pid
  pid="$(systemctl --user show -p MainPID --value "$1" 2>/dev/null || true)"
  if [[ -z "$pid" || "$pid" == "0" ]]; then
    pid="$(systemctl show -p MainPID --value "$1" 2>/dev/null || true)"
  fi
  if [[ -n "$pid" && "$pid" != "0" ]]; then
    echo "$pid"
  fi
}

prescribed_tokens() {
  # Flag tokens the repo says the process must be running with, one per line.
  case "$1" in
    pm-shadow-final|pm-shadow-final-b|pm-shadow-15m)
      ( source "$SCRIPT_DIR/shadow_flags.sh" >/dev/null 2>&1
        printf '%s\n' "${SHADOW_FLAGS[@]}" ) ;;
    *)
      grep -m1 '^ExecStart=' "$UNIT_DIR/$1.service" 2>/dev/null \
        | cut -d= -f2- | tr ' ' '\n' | sed '/^$/d' ;;
  esac
}

check_flags() {
  local unit="$1" pid="$2" cmdline tok
  local missing=()
  if ! cmdline="$(tr '\0' '\n' < "/proc/$pid/cmdline" 2>/dev/null)"; then
    fail "$unit flags: cannot read /proc/$pid/cmdline"
    return
  fi
  while IFS= read -r tok; do
    [[ -z "$tok" ]] && continue
    grep -qxF -- "$tok" <<<"$cmdline" || missing+=("$tok")
  done < <(prescribed_tokens "$unit")
  if ((${#missing[@]} > 0)); then
    fail "$unit flags: missing from running cmdline: ${missing[*]}"
  else
    pass "$unit flags: all prescribed tokens present in running cmdline"
  fi
}

# env_lookup <blob of K=V lines> <KEY>: print value, rc 1 if KEY absent.
env_lookup() {
  local line
  while IFS= read -r line; do
    if [[ "${line%%=*}" == "$2" ]]; then
      printf '%s' "${line#*=}"
      return 0
    fi
  done <<<"$1"
  return 1
}

check_env() {
  local unit="$1" pid="$2"
  local unit_file="$UNIT_DIR/$unit.service"
  local line k v rv declared="" running=""

  while IFS= read -r line; do
    line="${line#Environment=}"
    line="${line#\"}"; line="${line%\"}"
    k="${line%%=*}"
    case "$k" in
      PM_*|SHADOW_*) declared+="$line"$'\n' ;;
    esac
  done < <(grep '^Environment=' "$unit_file" 2>/dev/null || true)

  # Vars from EnvironmentFile= paths declared in the repo unit are declared
  # config too (the '-' prefix marks optional files). Read each referenced
  # file on the box so its PM_*/SHADOW_* vars are not flagged as undeclared.
  local envfile
  while IFS= read -r envfile; do
    envfile="${envfile#EnvironmentFile=}"
    envfile="${envfile#-}"
    [[ -r "$envfile" ]] || continue
    while IFS= read -r line; do
      line="${line%%#*}"
      [[ -z "$line" ]] && continue
      line="${line#\"}"; line="${line%\"}"
      k="${line%%=*}"
      case "$k" in
        PM_*|SHADOW_*) declared+="$line"$'\n' ;;
      esac
    done < "$envfile"
  done < <(grep '^EnvironmentFile=' "$unit_file" 2>/dev/null || true)

  if ! [[ -r "/proc/$pid/environ" ]]; then
    fail "$unit env: cannot read /proc/$pid/environ"
    return
  fi
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    k="${line%%=*}"
    case "$k" in
      PM_*|SHADOW_*) running+="$line"$'\n' ;;
    esac
  done < <(tr '\0' '\n' < "/proc/$pid/environ")

  local problems=()
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    k="${line%%=*}"; v="${line#*=}"
    if ! rv="$(env_lookup "$running" "$k")"; then
      problems+=("$k declared in repo unit but missing from process")
    elif [[ "$rv" != "$v" ]]; then
      if value_printable "$k"; then
        problems+=("$k differs: running='$rv' repo='$v'")
      else
        problems+=("$k differs (values redacted)")
      fi
    fi
  done <<<"$declared"
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    k="${line%%=*}"; v="${line#*=}"
    if ! env_lookup "$declared" "$k" >/dev/null; then
      if value_printable "$k"; then
        problems+=("$k='$v' present in process but undeclared in repo unit")
      else
        problems+=("$k present in process but undeclared in repo unit (value redacted)")
      fi
    fi
  done <<<"$running"

  if ((${#problems[@]} > 0)); then
    fail "$unit env: ${#problems[@]} problem(s)"
    printf '     %s\n' "${problems[@]}"
  else
    pass "$unit env: PM_*/SHADOW_* match repo unit"
  fi
}

check_git() {
  local repo="$1" head origin
  if [[ ! -d "$repo/.git" ]]; then
    fail "git $repo: not a git checkout"
    return
  fi
  if ! git -C "$repo" fetch -q origin 2>/dev/null; then
    fail "git $repo: fetch origin failed"
    return
  fi
  head="$(git -C "$repo" rev-parse HEAD 2>/dev/null || true)"
  origin="$(git -C "$repo" rev-parse origin/main 2>/dev/null || true)"
  if [[ -z "$head" || -z "$origin" || "$head" != "$origin" ]]; then
    fail "git $repo: HEAD ${head:0:12} != origin/main ${origin:0:12}"
  elif [[ -n "$(git -C "$repo" status --porcelain --untracked-files=no 2>/dev/null)" ]]; then
    fail "git $repo: dirty tracked files"
  else
    pass "git $repo: HEAD == origin/main (${head:0:12}), clean"
  fi
}

for unit in "${UNITS[@]}"; do
  if [[ ! -f "$UNIT_DIR/$unit.service" ]]; then
    fail "$unit: no repo unit file at $UNIT_DIR/$unit.service"
    continue
  fi
  pid="$(unit_pid "$unit.service")"
  if [[ -z "$pid" ]]; then
    fail "$unit: not running (no MainPID)"
    continue
  fi
  check_flags "$unit" "$pid"
  check_env "$unit" "$pid"
done

for repo in "$HOME/pm-backtest" "$HOME/deploy-main/polymarket-backtest" "$HOME/deploy-main/polymarket-agent"; do
  check_git "$repo"
done

echo "verify_deploy: $FAILS failure(s)"
[[ "$FAILS" -eq 0 ]]
