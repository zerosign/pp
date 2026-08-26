# pp

A fast repository locator with a Rust core and a Neovim float picker.

Two pieces:

- **pp CLI** -- scans configured roots for repositories, keeps an `fst`-backed
  cache, and exposes `list`, `search`, `index`, `clear`.
- **pp.nvim** -- `:PpProject` / `:PpSwitch` / `:PpSearch` open a floating picker
  whose per-keystroke search runs in Rust through a LuaJIT FFI cdylib
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
just install      # binary to ~/.local/bin/pp + fish wrapper + shell completions
pp index          # build the repository index once
```

`just uninstall` removes everything again. Completions cover fish, bash and
zsh; restart your shell after installing.

### Neovim plugin

```lua
-- ~/.config/nvim/lua/plugins/pp.lua
return {
  'zerosign/pp',
  build = function() require('pp').build_native() end,
  opts = {},
  keys = {
    { '<leader>fp', '<cmd>PpProject<CR>', desc = 'Open project (pp)' },
    { '<leader>fs', '<cmd>PpSearch fuzzy<CR>', desc = 'Search projects (pp)' },
  },
}
```

On install/update lazy.nvim runs the `build` hook, which compiles
`libpp_nvim.so` for your machine (`-C target-cpu=native`, needs cargo). The
`pp` CLI must be on `$PATH`. If the cdylib is missing, the picker falls back
to fzf-lua; `:PpBuild` rebuilds it anytime.

## Building & testing

```sh
just build-nvim-native    # FFI cdylib (native) -> build/libpp_nvim.so
just nvim-portable        # FFI cdylib (portable, any x86-64)
just clippy               # pedantic lints (workspace lints; cargo clippy suffices)
cargo test --workspace    # 13 tests (12 core + 1 FFI roundtrip)
just test-lua             # sandboxed headless picker + JIT-trace suites
cargo +nightly miri test -p pp-nvim   # FFI test under Miri
```

## How it works

The index is an immutable `fst::Set` of absolute paths under configured roots.
Search loads the set into memory and ranks results per keystroke: exact basename
matches first, then prefix, substring, and subsequence. All matching (fuzzy edit
distance, subsequence) runs in the Rust cdylib; Lua only renders the returned
paths.

The FFI layer caches the repo list and reloads when `index.fst` changes on
disk, so a reindex from another process shows up on the next keystroke. The
cdylib is built per machine with `-C target-cpu=native` by default -- the lazy
build hook and `just build-nvim-native` both do this automatically.

The native float picker has no dependencies beyond Neovim's LuaJIT runtime. If
the cdylib is missing or fails to load, pp.nvim falls back to fzf-lua for both
project selection and the post-selection files picker.
