# pp — Fast repository locator

A repository switcher and locator with a Rust core and a zero-dependency
Neovim float picker.

Two pieces:

- **`pp` CLI** — scans configured roots for repositories (git markers), keeps
  an `fst`-backed cache, and exposes `list`, `search`, `index`, `clear`.
- **pp.nvim** — `:PpProject` / `:PpSwitch` / `:PpSearch` open a floating picker
  whose per-keystroke search runs **in Rust** through a LuaJIT FFI cdylib
  (`libpp_nvim.so`), falling back to fzf-lua when the library is missing.

```
lua/pp/picker.lua ── ffi.load ──▶ build/libpp_nvim.so ──▶ crates/pp-nvim ──▶ crates/pp
      (UI only)                  (cdylib FFI bridge)      (pure-Rust API)     (core: scan/index/search)
lua/pp/projects.lua ── vim.system ──▶ pp CLI binary (data layer)
```

## CLI

```
pp index        Rebuild the repository index
pp list         Print repositories (--no-cache for a live scan)
pp search <q>   Search (--mode substring|prefix|fuzzy|subseq, --distance, --limit)
pp clear        Clear the index
pp generate <sh> Shell completions (fish, bash, zsh)
pp             Interactive picker (skim) on stderr, selection on stdout
```

## Neovim plugin

```
:PpProject   Pick a project, open it in a new tab, tcd into it, open its files
:PpSwitch    Pick a project and switch the current window's cwd
:PpSearch    Search projects (optional mode: substring|fuzzy|prefix|subseq)
:PpIndex     Rebuild the index
:PpClear     Clear the index
:PpBuild     Build the FFI cdylib with -C target-cpu=native
```

Configuration (`require('pp').setup(opts)`): `binary`, `prompt`, `files_prompt`,
`picker`, `lib_path`, `default_mode`, `fuzzy_distance`, `max_results`,
`debounce_ms`.

## Installation

Requires a Rust toolchain (`cargo`) and [`just`](https://github.com/casey/just).

### CLI

```sh
just install      # binary → ~/.local/bin/pp + fish wrapper + shell completions
pp index          # build the repository index once
```

`just uninstall` removes everything again. Completions cover fish, bash and
zsh; restart your shell (or `source ~/.config/fish/functions/pp.fish`) after
installing.

### Neovim plugin (lazy.nvim / LazyVim)

```lua
-- ~/.config/nvim/lua/plugins/pp.lua
return {
  'zerosign/pp',
  build = function() require('pp').build_native() end, -- compiles the FFI cdylib
  opts = {},
  keys = {
    { '<leader>fp', '<cmd>PpProject<CR>', desc = 'Open project (pp)' },
    { '<leader>fs', '<cmd>PpSearch fuzzy<CR>', desc = 'Search projects (pp)' },
  },
}
```

That's it. On install/update lazy.nvim runs the `build` hook, which compiles
`libpp_nvim.so` for your machine (`-C target-cpu=native`, needs cargo). The
`pp` CLI must be on `$PATH` (step above). If the cdylib is ever missing, the
picker falls back to fzf-lua with a one-time notice; `:PpBuild` rebuilds it
anytime. Pick keys that don't clash with your existing mappings (`<leader>fp`
is Telescope's "find project" in stock LazyVim).

## Building & testing

```sh
just nvim                 # build the FFI cdylib (native: -C target-cpu=native) → build/libpp_nvim.so
just nvim-portable        # portable build (runs on any x86-64 CPU)
just clippy               # pedantic lints (enforced via workspace lints; plain `cargo clippy` is enough)
cargo test --workspace    # 13 tests (12 core + 1 FFI roundtrip)
just test-lua             # sandboxed headless picker + JIT-trace suites (fake $HOME, never touches your config)
cargo +nightly miri test -p pp-nvim   # FFI test under Miri (no UB, no leaks)
```

## Verification status

- `cargo clippy --workspace --all-targets` — clean at clippy::pedantic (workspace lints), exit 0.
- `cargo test --workspace` — 13/13 pass.
- Miri (`-Zmiri-disable-isolation`) — clean on both crates.
- Sandboxed headless picker tests (`just test-lua`, `tests/`) — pass.
- TUI tmux smoke test — picker opens, filtering ranks exact basename first,
  `<C-f>` mode cycle re-renders, `<CR>` accepts and opens the workspace.
- LuaJIT trace audit (`jit.dump 'tb'`) — the per-keystroke path compiles to
  native traces with **zero aborts in pp code**:

  ```
  ---- TRACE 16 start picker.lua:94        ; split_lines byte-scan loop
  ---- TRACE 16 stop -> loop               ; COMPILED
  ---- TRACE 17 start 16/4 picker.lua:96   ; '\n' branch side trace
  ---- TRACE 17 stop -> 16                 ; COMPILED
  ---- TRACE 20..22 start picker.lua:185.. ; nvim_buf_* API calls -> stitch
  ```

  The only aborts (7) are inside nvim's own stdlib (`shared`, `vim/keymap`,
  `vim/_core/editor`) on one-shot setup paths, never in the keystroke path.
- Syscall audit (`strace`) on the query flow: steady state is exactly **one
  `statx` per keystroke** (the index mtime check — zero file reads, zero
  opens); a cold cache does one `openat` + two `read`s + `close` once, then
  the index lives in the Rust process cache.
- Native-build audit: `-C target-cpu=native` release builds are byte-identical
  to the default release build on this machine because the global
  `~/.cargo/config.toml` already tunes `[profile.release]` to
  `-C target-cpu=znver4`. The flag matters on machines with no such config.

## Tradeoffs

- **fst as storage, in-memory matching (decision B) over an embedded DB
  (decision A).** The index is an immutable `fst::Set` of absolute paths and
  search runs over the whole list in memory per keystroke. Simple, fast, and
  dependency-light at the 100s-of-repos scale; it does not scale to huge
  indexes, has no incremental queries, and re-ranks everything on every
  keystroke.
- **Search is in memory, matching is in Rust.** All heavy work (fuzzy
  Levenshtein, subsequence, substring) is done in the cdylib, so Lua only
  renders the returned paths. Cost: the fst index lives outside the Lua GC,
  which sidesteps LuaJIT's ~2 GB managed-heap limit — but the FFI call itself
  is a trace stitch (documented LuaJIT 2.1 behavior, near-zero overhead).
- **Fuzzy mode scores the basename only** (edit distance), not the full path.
  Cheap and predictable; a full-path fuzzy match would need much larger
  distance budgets and produce noisy results.
- **Zero-dependency float picker over fzf-lua by default.** No dependency and
  per-keystroke Rust results, but no preview pane, actions, or formatting.
  The fzf-lua fallback uses fzf's own matcher, so its results can differ from
  the FFI path (Lua never re-filters FFI output by design).
- **Debounce (25 ms) + `max_results` cap (200).** Keeps rendering cheap and
  traces compiling; the long tail of a broad query is intentionally not shown.
- **Lazily refreshed process-global cache.** The FFI layer caches the repo
  list and reloads when `index.fst`'s mtime changes, so a reindex from another
  process appears on the next keystroke — there is no eager invalidation.
- **`nvim_buf_set_lines` is trusted not to fire `TextChanged`**, avoiding a
  self-triggering re-search loop (verified on nvim 0.13.0-dev).
- **The cdylib is built per machine with `-C target-cpu=native` by default.**
  The lib is compiled at install time (lazy.nvim `build` hook / `just nvim`),
  so march=native is the right default — but the result is not portable to
  other CPUs, and builds always re-run the project's `RUSTFLAGS` over any
  global cargo tuning.
- **`_debug` seam** in `lua/pp/picker.lua` exists so headless tests can drive
  the picker without a UI. It is documented as not part of the public API.

## Assumptions

1. **Neovim always runs LuaJIT (2.1).** The picker relies on `ffi.load`,
   `table.new`, and LuaJIT byte-scan idioms (`string.byte`/`string.sub`
   compile to native IR; the pattern engine and closures do not). If Neovim
   ever ships PUC Lua or Luau, the FFI picker breaks — the graceful fallback
   to fzf-lua (or a configured custom picker) is the mitigation.
2. **LuaJIT's `ffi` and `table.new` extensions are available** (bundled in
   Neovim; verified: `jit.version` = LuaJIT 2.1).
3. **A host-platform cdylib exists at `<plugin>/build/libpp_nvim.so`** (or
   `.dylib`/`.dll`), built per machine with `just nvim` or the lazy.nvim
   `build` hook (`require('pp').build_native()`), native by default. Missing →
   fzf-lua fallback with a one-time warning.
4. **The Rust `pp` binary is on `$PATH`** (or set via `config.options.binary`);
   the data layer shells out to it for `list`/`index`/`clear`.
5. **A strict FFI ABI contract**: `pp_search` returns a NUL-terminated,
   newline-joined heap string (empty when no matches) or a null pointer on
   error; every non-null result is released via `pp_string_free`; `pp_refresh`
   returns 1/0. The LuaJIT `ffi.cdef` must stay in lockstep with the Rust
   `unsafe extern "C"` signatures.
6. **One nvim process = one FFI cache.** The repo list is process-global and
   refreshed only when the index file's mtime changes.
7. **`nvim_win_set_cursor` clamps columns to the last character** and
   `startinsert!` behaves like `a` (caret lands just past the prompt) —
   verified empirically; relied on by the picker's open sequence. Also:
   headless nvim does not dispatch keymaps or enter insert mode, so tests use
   the `_debug` seam.
8. **`nvim_buf_set_lines` does not fire `TextChanged`** (current nvim builds).
9. **Index semantics**: an `fst::Set` of absolute paths under configured
   roots; ranking assumes POSIX-ish path basenames (`Path::file_name`).
10. **XDG conventions**: cache/config paths come from `$XDG_CACHE_HOME` /
    `$XDG_CONFIG_HOME` and the cache directory must be writable.
11. **Project detection**: a directory containing any configured marker
    (e.g. `.git`) is a repo root; ignored dirs are pruned; `max_depth` caps
    traversal.
12. **A legacy redb `index.db` may linger**; `reindex` removes it on success.
13. **Rust edition 2024**: `#[unsafe(no_mangle)]` and `unsafe extern "C"`
    exports are required; the workspace targets the host platform for the
    cdylib.
14. **Miri (dev-only)**: rayon's thread spawns are not Miri-compatible, so the
    FFI test builds its throwaway index with `fst::SetBuilder` directly and
    Miri runs with `-Zmiri-disable-isolation` (`statx` is otherwise blocked).
15. **fzf-lua is available for the post-selection files picker** unless a
    custom `picker` is configured; if missing, a graceful "fzf-lua is
    required" notice is shown.
16. **Native-per-machine build model**: `build/` and `target/` are gitignored
    (no committed binaries), and the cdylib is compiled at install time with
    `-C target-cpu=native`. The resulting lib only runs on the build machine
    (or newer); users with their own global cargo tuning keep their settings
    (profile flags are appended after the plugin's `RUSTFLAGS`).
