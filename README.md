# pp

A fast, lightweight repository locator and project switcher with a Rust core and a Neovim floating picker.

## Architecture

- **pp CLI**: Scans configured roots for git/project repositories, maintains a memory-mapped `rkyv` cache, and exposes fast `index`, `list`, `search`, and `clear` commands.
- **pp.nvim**: Neovim plugin providing `:PpProject`, `:PpSwitch`, and `:PpSearch`. Runs per-keystroke fuzzy matching directly in Rust via a LuaJIT FFI cdylib (`libpp_nvim.so`) with automatic `vim.ui.select` fallback.

```
lua/pp/picker.lua ── ffi.load ──▶ build/libpp_nvim.so ──▶ crates/pp-nvim ──▶ crates/pp
      (UI only)                  (cdylib FFI bridge)      (pure-Rust API)     (core: scan/cache/search)
lua/pp/projects.lua ── vim.system ──▶ pp CLI binary (data layer)
```

---

## CLI Usage

```sh
pp index         # Rebuild the repository index
pp list          # Print indexed repositories (--no-cache for a live scan)
pp search <q>    # Fast CLI search (--mode substring|prefix|fuzzy|subseq, --limit N)
pp clear         # Clear the index cache
pp generate <sh> # Generate shell completion scripts (fish, bash, zsh)
pp               # Interactive picker (fuzzy-matches any path component; "/" for a whole-path query)
```

---

## Neovim Plugin

### Commands

| Command | Description |
| :--- | :--- |
| `:PpProject` | Pick a project, open a new tab, `tcd` into it, and open its files picker |
| `:PpSwitch` | Pick a project and switch the current window's `cwd` |
| `:PpSearch [mode]` | Search projects using `substring`, `fuzzy`, `prefix`, or `subseq` mode |
| `:PpIndex` | Rebuild the repository index asynchronously |
| `:PpClear` | Clear the index cache |
| `:PpBuild` | Rebuild the native FFI cdylib with `-C target-cpu=native` |

### Configuration

Configure via `require('pp').setup(opts)`:

```lua
require('pp').setup({
  binary = 'pp',                  -- Path or binary name for pp CLI on $PATH
  prompt = 'Workspace Project > ', -- Floating picker prompt
  files_prompt = '%s Files> ',     -- Post-selection files picker prompt (%s = project name)
  open_files = true,              -- Whether to open a files picker after selecting a project
  files_picker = 'dir',           -- 'dir' (default built-in) | 'fzf-lua' | 'snacks' | 'telescope' | 'mini.pick' | 'auto' | 'none'
  picker = nil,                   -- Custom project picker: fn(list, opts) or table { pick = fn, files = fn }
  default_mode = 'substring',     -- Default match mode: substring | fuzzy | prefix | subseq
  fuzzy_distance = 2,             -- Max Levenshtein edit distance for fuzzy mode
  max_results = 200,              -- Max search results rendered per keystroke
  debounce_ms = 25,               -- Keystroke debounce delay (ms)
  lib_path = nil,                 -- Custom path to libpp_nvim.so (nil auto-detects)
})
```

---

## Installation

### 1. CLI Installation

Requires Rust (`cargo`) and [`just`](https://github.com/casey/just).

```sh
just install         # Installs release binary to ~/.local/bin/pp + fish wrapper + completions
just install-static  # Installs static musl binary to ~/.local/bin/pp
pp index             # Build repository index once
```

To remove: `just uninstall`.

### 2. Neovim Plugin Installation (`lazy.nvim`)

```lua
-- ~/.config/nvim/lua/plugins/pp.lua
return {
  'zerosign/pp',
  build = function() require('pp').build_native() end,
  opts = {},
  keys = {
    { '<leader>fp', '<cmd>PpProject<CR>', desc = 'Find project (pp)' },
    { '<leader>fs', '<cmd>PpSearch fuzzy<CR>', desc = 'Search projects (pp)' },
  },
}
```

On installation or update, `lazy.nvim` executes `build_native()` to compile `libpp_nvim.so` tuned specifically for your CPU (`-C target-cpu=native`). If `cargo` is unavailable or the library is missing, `pp.nvim` automatically falls back to `vim.ui.select` — so it uses whatever picker you have configured (e.g. snacks).

---

## Building & Testing

```sh
just build-nvim-native    # Build FFI cdylib (native) -> build/libpp_nvim.so
just nvim-portable        # Build FFI cdylib (portable x86-64)
just clippy               # Run pedantic clippy lints
cargo test --workspace    # Run all Rust unit and integration tests (22 tests)
just test-lua             # Run sandboxed Lua tests and LuaJIT trace audits
```

---

## How It Works

- **Caching**: `pp` maintains an immutable, memory-mapped `rkyv` archive of absolute paths under configured root directories (configurable in `~/.config/pp/config.toml`). The archive is read zero-copy, so the picker borrows paths straight out of the mapping. A detected repository is a leaf by default; subtrees listed in `nested_projects` are descended past instead, so nested repositories inside them are indexed too.
- **Search & Ranking**: Search (`pp search` / `:PpSearch`) ranks per mode — `fuzzy` by edit distance; otherwise an exact basename match, then a basename prefix, then the rest. The interactive `pp` picker scores every path component, so any directory in the path can match (a query containing `/` matches the whole path).
- **FFI Performance**: The Neovim floating UI passes queries to `libpp_nvim.so` via LuaJIT FFI. Matching runs entirely in compiled C/Rust code; Lua only renders the resulting lines to the buffer.
- **Automatic Cache Invalidation**: The FFI layer monitors `pp.cache` modification timestamps on disk and reloads instantly when reindexed by another process.
