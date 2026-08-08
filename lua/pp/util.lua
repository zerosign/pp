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

return M
