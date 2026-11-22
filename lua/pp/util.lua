-- pp.nvim — shared helpers.

local M = {}

--- `vim.notify` that is safe in fast-event contexts.
--
-- `vim.system` callbacks can fire while nvim is blocked (e.g. inside
-- `vim.wait`, which the data layer relies on), and `nvim_echo` is forbidden
-- there. Deferring through `vim.schedule` sidesteps that.
function M.notify(msg, level)
  if vim.in_fast_event() then
    vim.schedule(function()
      vim.notify(msg, level or vim.log.levels.INFO)
    end)
  else
    vim.notify(msg, level or vim.log.levels.INFO)
  end
end

--- Resolve the normalized project root for `buf` (default 0):
--- 1. Active LSP client root_dir on `buf`
--- 2. Nearest ancestor containing any `config.options.project_markers`
---    (`.git`, `go.mod`, `Cargo.toml`, `package.json` — matching Rust core)
--- 3. Buffer directory -> current working directory.
function M.project_root(buf)
  buf = buf or 0
  for _, client in ipairs(vim.lsp.get_clients({ bufnr = buf })) do
    if client.config and client.config.root_dir and client.config.root_dir ~= '' then
      return vim.fs.normalize(client.config.root_dir)
    end
  end
  local config = require('pp.config')
  local markers = (config.options and config.options.project_markers)
    or { '.git', 'go.mod', 'Cargo.toml', 'package.json' }
  local marker_root = vim.fs.root(buf, markers)
  if marker_root then
    return vim.fs.normalize(marker_root)
  end
  local buf_name = vim.api.nvim_buf_get_name(buf)
  if buf_name ~= '' then
    return vim.fs.normalize(vim.fs.dirname(buf_name))
  end
  return vim.fs.normalize(vim.fn.getcwd())
end

--- Picker-agnostic selection helper: defers to `vim.ui.select` (just like
--- `picker.lua`), which the user's UI provider (snacks, fzf-lua, telescope, ...)
--- or Neovim's built-in handles without hardcoding any third-party plugin.
function M.select(items, opts, on_choice)
  vim.ui.select(items, opts, on_choice)
end

return M
