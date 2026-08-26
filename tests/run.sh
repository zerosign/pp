#!/usr/bin/env bash
# Sandboxed test harness for pp.nvim.
#
# Everything runs inside tests/sandbox: a fake $HOME with fake repositories,
# isolated XDG dirs, and a freshly built pp index. Neither your real nvim
# config/state nor your real repository index is ever touched:
#   - nvim runs with -u NONE -i NONE and HOME/XDG_* pointed into the sandbox
#   - pp resolves config/cache/roots from the same overridden env
#
# Usage: ./tests/run.sh
set -uo pipefail
cd "$(dirname "$0")/.." # repo root

BIN=target/release/pp
if [ ! -x "$BIN" ]; then
  echo "==> building pp binary"
  cargo build --release --bin pp >/dev/null
fi

SB=tests/sandbox
echo "==> building sandbox in $SB"
rm -rf "$SB"
mkdir -p "$SB/home/Repositories/work" \
  "$SB/home/.config/pp" \
  "$SB/home/.local/share" "$SB/home/.local/state" \
  "$SB/cache"

# Fake repositories (marker = .git dir). Query 'k6' exercises all rank tiers:
#   k6            -> exact basename
#   k6-operator   -> basename prefix
#   notk6         -> basename contains (other)
#   work/k6-thing -> path contains (other)
#   zebra         -> unrelated filler
mkrepo() { mkdir -p "$1/.git"; }
mkrepo "$SB/home/Repositories/k6"
mkrepo "$SB/home/Repositories/k6-operator"
mkrepo "$SB/home/Repositories/notk6"
mkrepo "$SB/home/Repositories/work/k6-thing"
mkrepo "$SB/home/Repositories/zebra"

export HOME="$SB/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state"

# Minimal nvim init for tmux/manual testing with this sandbox.
# Headless tests use -u NONE and load scripts directly; this file
# exists so `env HOME=... XDG_*=... nvim -u tests/sandbox/init.lua`
# boots a working pp session against the fake repos.
cat > "$SB/init.lua" <<LUA
vim.opt.runtimepath:prepend('${PWD}')
require('pp').setup({})
LUA

# Repos live at $HOME/Repositories (the default root); no extra env needed —
# the Rust code resolves everything from HOME/XDG_* alone.

echo "==> indexing sandbox repos"
"$BIN" index

fail=0
run_suite() {
  local name="$1" script="tests/$2" out
  echo "==> $name"
  out=$(nvim --headless -u NONE -i NONE -c "luafile $script" -c 'qa!' 2>&1) || true
  [ -n "$out" ] && printf '%s\n' "$out"
  if ! printf '%s\n' "$out" | grep -q 'PASSED'; then
    echo "!! $name FAILED"
    fail=1
  fi
}

run_suite "picker behavior" picker_test.lua
run_suite "selection/scroll/highlight" select_test.lua

# JIT audit runs under the SYSTEM luajit binary: its -jdump driver matches its
# own VM, while nvim cannot load any dump driver (none bundled, system copies
# use newer syntax, jit.attach inert). The audit loads the REAL picker module
# with a thin vim.* stub and drives the genuine FFI+render hot path.
echo "==> JIT audit (system luajit -jdump)"
DUMP="$SB/jit_dump.txt"
JIT_OUT=$(luajit -e '
local jd = require("jit.dump")
jd.start("a,s", "'"$DUMP"'")
local f = assert(loadfile("tests/jit_audit.lua"))
f()
' 2>&1) || true
printf '%s\n' "$JIT_OUT"
if ! printf '%s\n' "$JIT_OUT" | grep -q 'workload done'; then
  echo "!! JIT audit FAILED (workload did not complete)"
  fail=1
elif [ -f "$DUMP" ] && grep 'ABORT' "$DUMP" | grep -Eq 'pp/|picker\.lua'; then
  echo "!! JIT audit FAILED: trace aborts from pp code:"
  grep 'ABORT' "$DUMP" | grep -E 'pp/|picker\.lua'
  fail=1
else
  echo "JIT AUDIT PASSED ($(grep -c 'TRACE' "$DUMP" 2>/dev/null || echo 0) trace lines, no pp aborts)"
fi

if [ "$fail" -eq 0 ]; then
  echo "==> ALL SUITES PASSED"
fi
exit "$fail"
