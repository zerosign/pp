-- JIT trace audit for the pp.nvim hot path.
-- Run via: ./tests/run.sh  (this file runs under the SYSTEM luajit binary,
-- not nvim — see run.sh. nvim's bundled VM cannot load any jit.dump driver:
-- it ships none, system copies use newer syntax, and jit.attach is inert.)
--
-- The real picker module is loaded with a thin vim.* stub, then driven hard
-- through do_search/render/move_selection against the real FFI library while
-- luajit -jdump records traces. We assert: zero trace aborts from pp code.
-- (Stitched C-API/FFI boundaries are expected; ABORT lines are not.)

local script_src = debug.getinfo(1, 'S').source or ''
local script_path = script_src:sub(1, 1) == '@' and script_src:sub(2) or script_src
local root_dir = script_path ~= '' and script_path:match('^(.*)/tests/[^/]+$') or '.'
package.path = root_dir .. '/lua/?.lua;' .. package.path

-- ---------------------------------------------------------------------------
-- Minimal vim.* stub: just enough for picker.lua to load and run headless.
-- ---------------------------------------------------------------------------
local NS_ID = 0
local stub_buf = { lines = { '' } } -- buffer line storage (1-based rows)

local api = {}
function api.nvim_create_namespace()
  NS_ID = NS_ID + 1
  return NS_ID
end
function api.nvim_buf_set_lines(_, _, _, _, replacement)
  -- picker only replaces rows after the query line
  for i, l in ipairs(replacement) do
    stub_buf.lines[i + 1] = l
  end
end
function api.nvim_buf_get_lines(_, start, stop)
  local out = {}
  for i = start + 1, math.min(stop == -1 and #stub_buf.lines or stop, #stub_buf.lines) do
    out[#out + 1] = stub_buf.lines[i]
  end
  return out
end
function api.nvim_buf_clear_namespace() end
function api.nvim_buf_set_extmark() end
function api.nvim_win_is_valid()
  return true
end
function api.nvim_win_get_height()
  return 14
end
function api.nvim_open_win()
  return 42
end
function api.nvim_win_set_cursor() end
function api.nvim_win_set_config() end
function api.nvim_create_buf()
  return 7
end
function api.nvim_get_hl()
  return { link = 'Visual' }
end
function api.nvim_create_autocmd() end
function api.nvim_win_close() end
function api.nvim_buf_delete() end

local function noop() end
local function identity(f)
  return f
end
_G.vim = {
  api = api,
  uv = {
    new_timer = function()
      return { start = noop, stop = noop }
    end,
  },
  schedule_wrap = identity,
  schedule = identity,
  cmd = noop,
  -- vim.b[buf] auto-vivifies in real nvim; mirror that for the stub.
  b = setmetatable({}, {
    __index = function()
      return {}
    end,
  }),
  o = { columns = 200, lines = 50 },
  log = { levels = { ERROR = 4, WARN = 3, INFO = 2 } },
  keymap = { set = noop },
  deepcopy = function(t)
    if type(t) ~= 'table' then
      return t
    end
    local r = {}
    for k, v in pairs(t) do
      r[k] = vim.deepcopy(v)
    end
    return r
  end,
  tbl_deep_extend = function(_, ...) -- 'force' merge; later tables win
    local out = {}
    for i = 2, select('#', ...) do
      local src = select(i, ...)
      if type(src) == 'table' then
        for k, v in pairs(src) do
          if type(v) == 'table' and type(out[k]) == 'table' then
            out[k] = vim.tbl_deep_extend('force', out[k], v)
          else
            out[k] = v
          end
        end
      end
    end
    return out
  end,
  tbl_isempty = function(t)
    return next(t) == nil
  end,
  tbl_filter = function(f, t)
    local out = {}
    for _, v in ipairs(t) do
      if f(v) then
        out[#out + 1] = v
      end
    end
    return out
  end,
  split = function(s, sep)
    local out = {}
    for part in (s .. sep):gmatch('(.-)' .. sep) do
      out[#out + 1] = part
    end
    return out
  end,
  system = noop,
  in_fast_event = function()
    return false
  end,
  notify = noop,
  wait = function()
    return true
  end,
  fn = {
    filereadable = function()
      return 1
    end,
  },
  fs = {
    joinpath = function(...)
      local t = {}
      for i = 1, select('#', ...) do
        t[i] = select(i, ...)
      end
      return table.concat(t, '/')
    end,
    dirname = function(p)
      return p:match('^(.*)/[^/]+$') or p
    end,
  },
}
-- config/util/projects only need notify + list; projects.list is unused on
-- the FFI float path but must exist at load time.
package.preload['pp.projects'] = package.preload['pp.projects']
  or function()
    return {
      list = function(cb)
        cb({})
      end,
      run = noop,
      reindex = noop,
      clear = noop,
      fresh = noop,
      lines = function()
        return {}
      end,
      notify = noop,
    }
  end

-- ---------------------------------------------------------------------------
-- Load the REAL picker and drive the hot path under -jdump.
-- ---------------------------------------------------------------------------
local picker = require('pp.picker')
local d = picker._debug

picker.pick({ prompt = 'jit> ', on_select = function() end })
d.do_search()

local queries = { '', 'k', 'k6', 'k6-', 'k6-o', 'zeb', 'zzz-nothing' }
for _ = 1, 8 do
  for _, q in ipairs(queries) do
    stub_buf.lines[1] = q -- current_query reads row 0
    d.do_search()
    d.move_selection(1)
    d.move_selection(-1)
  end
end

print('workload done')
