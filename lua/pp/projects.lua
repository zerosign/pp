-- pp.nvim — data layer: talks to the `pp` CLI (cached fst index).
--
-- Mirrors the Rust `pp` lib surface (get_repos / scan_repos / reindex / clear),
-- so this module is the Lua counterpart of the core crate. No UI here.

local config = require('pp.config')
local util = require('pp.util')

local M = {}

local function notify(msg, level)
  util.notify(msg, level)
end

--- Run `pp <args>`. `on_success(stdout)` is called on exit 0; failures notify.
local function run(args, on_success)
  local argv = { config.options.binary }
  for _, a in ipairs(args) do
    argv[#argv + 1] = a
  end
  vim.system(argv, { text = true }, function(obj)
    if obj.code ~= 0 then
      local err = (obj.stderr or ''):gsub('%s+$', '')
      if err == '' then
        err = 'is `pp` on your PATH? Try `just install` in ~/Repositories/projects/pp.'
      end
      notify(
        string.format('pp %s failed (%d): %s', table.concat(args, ' '), obj.code, err),
        vim.log.levels.ERROR
      )
      return
    end
    if on_success then
      on_success(obj.stdout or '')
    end
  end)
end

local function lines(stdout)
  return vim.tbl_filter(function(s)
    return s ~= ''
  end, vim.split(stdout or '', '\n'))
end

--- Cached project list; calls `cb(list)`.
function M.list(cb)
  run({ 'list' }, function(stdout)
    cb(lines(stdout))
  end)
end

--- Live project list, bypassing the cache; calls `cb(list)`.
function M.fresh(cb)
  run({ 'list', '--no-cache' }, function(stdout)
    cb(lines(stdout))
  end)
end

--- Rebuild the index (progress is printed by pp itself to stderr).
function M.reindex()
  notify('Rebuilding pp index...')
  run({ 'index' }, function()
    notify('pp index rebuilt')
  end)
end

--- Clear the index; the next lookup rebuilds automatically.
function M.clear()
  run({ 'clear' }, function()
    notify('pp index cleared')
  end)
end

return M
