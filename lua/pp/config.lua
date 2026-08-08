-- pp.nvim — configuration: defaults + user overrides.
--
-- Consumed by every other module via `require('pp.config').options`.
-- Users configure pp.nvim through `require('pp').setup(opts)` (see init.lua).

local M = {}

M.defaults = {
  binary = 'pp',                -- binary name (must be on PATH) or absolute path
  prompt = 'Workspace Project > ',
  files_prompt = '%s Files> ',  -- %s is substituted with the project basename
  picker = nil,                 -- custom picker: { pick = fn(items, opts), files = fn(opts) }
                                -- (nil -> FFI float picker, fzf-lua fallback)

  -- FFI float picker options
  lib_path = nil,               -- nil -> <plugin>/build/libpp_nvim.so
  default_mode = 'substring',   -- substring | fuzzy | prefix | subseq (see `:PpSearch`)
  fuzzy_distance = 2,           -- max edits for fuzzy mode
  max_results = 200,            -- results rendered per keystroke
  debounce_ms = 25,             -- keystroke debounce before searching
}

--- Effective config after setup() has run.
M.options = vim.deepcopy(M.defaults)

--- Merge user opts over the defaults. Safe to call more than once.
function M.setup(opts)
  M.options = vim.tbl_deep_extend('force', vim.deepcopy(M.defaults), opts or {})
end

return M
