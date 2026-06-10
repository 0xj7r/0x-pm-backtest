#!/usr/bin/env bash
# Runs ON the deployment box: ensure rust, kick off the pm-app release build
# in the background (tail ~/pm-backtest-build.log; BUILD_OK marks success).
set -e
if ! [ -x "$HOME/.cargo/bin/cargo" ]; then
  curl --proto "=https" --tlsv1.2 -sSf -o /tmp/rustup-init.sh https://sh.rustup.rs
  bash /tmp/rustup-init.sh -y --default-toolchain 1.95 >/dev/null 2>&1
fi
cd ~/pm-backtest
nohup bash -c 'source ~/.cargo/env && cargo build --release -p pm-app > ~/pm-backtest-build.log 2>&1 && echo BUILD_OK >> ~/pm-backtest-build.log' >/dev/null 2>&1 &
echo BUILD_STARTED
