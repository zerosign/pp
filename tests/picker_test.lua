-- Headless behavior test for pp.nvim picker (sandboxed).
-- Run via: ./tests/run.sh   (sets HOME/XDG_* into tests/sandbox)
local script_src = debug.getinfo(1, 'S').source or ''
local script_path = script_src:sub(1, 1) == '@' and script_src:sub(2) or script_src
local root_dir = script_path ~= '' and script_path:match('^(.*)/tests/[^/]+$') or '.'
package.path = root_dir .. '/lua/?.lua;' .. package.path
local api = vim.api
local picker = require('pp.picker')

local function assert(cond, msg)
  if not cond then
    error('FAIL: ' .. msg)
  end
end

local function footer_of(win)
  local ok, cfg = pcall(api.nvim_win_get_config, win)
  if not ok or not cfg.footer then
    return nil
  end
  local parts = {}
  for _, chunk in ipairs(cfg.footer) do
    parts[#parts + 1] = tostring(chunk[1] or '')
  end
  return table.concat(parts)
end

local function find_picker()
  for _, w in ipairs(api.nvim_list_wins()) do
    local s = footer_of(w)
    if s and s:find('Enter open', 1, true) then
      return w, s
    end
  end
  return nil, nil
end

local function results()
  local st = picker._debug.state
  local out = {}
  for _, l in ipairs(api.nvim_buf_get_lines(st.buf, 1, -1, false)) do
    if l ~= '' then
      out[#out + 1] = l:sub(3) -- strip '> '/'  ' marker
    end
  end
  return out
end

-- 1. Open + initial UI -------------------------------------------------------
local selected = nil
picker.pick({
  prompt = 'test> ',
  on_select = function(p)
    selected = p
  end,
})

local win, foot = find_picker()
assert(win, 'picker window opened')
assert(foot:find('C-f mode', 1, true), 'footer hints present')
assert(foot:find('match:', 1, true) == nil, 'no mode label in default mode')

local st = picker._debug.state
assert(st.buf and api.nvim_buf_is_valid(st.buf), 'buffer valid')
assert(vim.b[st.buf].completion == false, 'blink.cmp disabled for the picker buffer')
local q = api.nvim_buf_get_lines(st.buf, 0, 1, false)[1]
assert(q == '', 'query line starts empty')
assert(q:find('test>', 1, true) == nil, 'prompt is virtual text, not buffer content')

-- 2. Search ranking: exact basename first ------------------------------------
-- Sandbox layout (see run.sh): k6, k6-operator, notk6, work/k6-thing, zebra.
local ROOT = (os.getenv('HOME') or '') .. '/Repositories'
assert(#ROOT > #'/Repositories', 'HOME set by tests/run.sh')

api.nvim_buf_set_lines(st.buf, 0, 1, false, { 'k6' })
picker._debug.do_search()
local r = results()
assert(#r == 4, 'k6 matches all four k6 repos, got ' .. tostring(#r))
assert(r[1] == ROOT .. '/k6', 'exact basename ranked first, got: ' .. tostring(r[1]))
assert(r[2] == ROOT .. '/k6-operator', 'basename prefix ranked second, got: ' .. tostring(r[2]))

local raw = api.nvim_buf_get_lines(st.buf, 1, 2, false)[1]
assert(raw:sub(1, 2) == '> ', 'selected row carries > marker')

-- 3. Mode cycle -> human label in footer --------------------------------------
picker._debug.cycle_mode()
foot = footer_of(win)
assert(foot and foot:find('fuzzy', 1, true), 'footer shows fuzzy label after cycle')
picker._debug.cycle_mode()
picker._debug.cycle_mode()
picker._debug.cycle_mode()
foot = footer_of(win)
assert(foot:find('match:', 1, true) == nil, 'mode label gone back at default')

-- 4. Move selection on a multi-result list ------------------------------------
api.nvim_buf_set_lines(st.buf, 0, 1, false, { '' })
picker._debug.do_search()
r = results()
assert(#r == 5, 'empty query lists all sandbox repos, got ' .. tostring(#r))
picker._debug.move_selection(1)
assert(st.cursor == 2, 'cursor moved to 2, got ' .. tostring(st.cursor))

-- 5. Accept closes + selects ---------------------------------------------------
picker._debug.accept()
assert(selected == r[2], 'accept passed second result, got ' .. tostring(selected))
assert(find_picker() == nil, 'float closed after accept')

-- 6. Reopen + cancel -----------------------------------------------------------
selected = nil
picker.pick({
  prompt = 'test> ',
  on_select = function(p)
    selected = p
  end,
})
assert(find_picker(), 'picker reopened')
if st.timer then
  st.timer:stop()
end
picker._debug.cancel()
assert(find_picker() == nil, 'float closed after cancel')
assert(selected == nil, 'cancel selects nothing')

print('PICKER TEST PASSED')
