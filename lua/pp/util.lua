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

--- Resolve the project root for `buf` (default 0):
--- LSP root -> `.git` root -> buffer directory -> current working directory.
function M.project_root(buf)
  buf = buf or 0
  for _, client in ipairs(vim.lsp.get_clients({ bufnr = buf })) do
    if client.config and client.config.root_dir and client.config.root_dir ~= '' then
      return client.config.root_dir
    end
  end
  local git_root = vim.fs.root(buf, '.git')
  if git_root then
    return git_root
  end
  local buf_name = vim.api.nvim_buf_get_name(buf)
  if buf_name ~= '' then
    return vim.fs.dirname(buf_name)
  end
  return vim.fn.getcwd()
end

--- Select from a list via Snacks.picker.select when available, else vim.ui.select.
function M.select(items, opts, on_choice)
  local ok, snacks = pcall(require, 'snacks')
  if ok and snacks.picker and snacks.picker.select then
    snacks.picker.select(items, opts, on_choice)
  else
    vim.ui.select(items, opts, on_choice)
  end
end

return M
