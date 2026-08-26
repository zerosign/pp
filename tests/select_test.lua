-- Headless selection/scroll/highlight test for pp.nvim picker (sandboxed).
-- Run via: ./tests/run.sh
local script_src = debug.getinfo(1, 'S').source or ''
local script_path = script_src:sub(1, 1) == '@' and script_src:sub(2) or script_src
local root_dir = script_path ~= '' and script_path:match('^(.*)/tests/[^/]+$') or '.'
package.path = root_dir .. '/lua/?.lua;' .. package.path
local api = vim.api
local picker = require('pp.picker')
local NS = api.nvim_create_namespace('pp-selection')

local function assert(cond, msg)
  if not cond then
    error('FAIL: ' .. msg)
  end
end

-- Highlight must be defined (linked to Visual by the plugin).
local ok, hl = pcall(api.nvim_get_hl, 0, { name = 'PpSelection' })
assert(ok and hl and next(hl) ~= nil, 'PpSelection highlight defined')

picker.pick({ prompt = 'sel> ', on_select = function() end })
local st = picker._debug.state
assert(st.win and api.nvim_win_is_valid(st.win), 'picker open')
if st.timer then
  st.timer:stop()
end

-- Inject 40 fake results and render.
local items = {}
for i = 1, 40 do
  items[i] = string.format('/tmp/fake/repo-%02d', i)
end
st.results = items
st.cursor, st.offset = 1, 1
picker._debug.render()

local vis = api.nvim_win_get_height(st.win) - 2
assert(vis >= 10, 'viewport size sane: ' .. tostring(vis))

local function mark_row()
  local marks = api.nvim_buf_get_extmarks(st.buf, NS, 0, -1, {})
  assert(#marks == 1, 'exactly one extmark, got ' .. #marks)
  return marks[1][2] -- 0-based buffer row
end

-- Walk down through all 40 items (wraps back to 1); the highlight must stay
-- inside the viewport at every step.
for step = 1, 40 do
  picker._debug.move_selection(1)
  local row = mark_row()
  assert(
    row >= 1 and row <= vis,
    string.format('step %d: extmark row %d outside viewport 1..%d', step, row, vis)
  )
  local line = api.nvim_buf_get_lines(st.buf, row, row + 1, false)[1] or ''
  assert(line:sub(1, 2) == '> ', 'step ' .. step .. ': marker missing at row ' .. row)
  if step == 39 then
    -- cursor=40 (last item): viewport pinned to the bottom edge.
    assert(
      st.offset == 40 - vis + 1,
      'bottom-edge offset ' .. tostring(st.offset) .. ' ~= ' .. tostring(40 - vis + 1)
    )
  end
end
assert(st.cursor == 1, 'wrapped to cursor 1, got ' .. tostring(st.cursor))
assert(st.offset == 1, 'offset reset after wrap, got ' .. tostring(st.offset))

-- Wrap upward from the top jumps to the end with viewport following.
st.cursor, st.offset = 1, 1
picker._debug.render()
picker._debug.move_selection(-1)
assert(st.cursor == 40, 'wrap-up cursor 40, got ' .. tostring(st.cursor))
assert(st.offset == 40 - vis + 1, 'wrap-up offset follows cursor')

-- Moving above the viewport top pulls the offset along.
st.cursor, st.offset = 5, 5
picker._debug.move_selection(-1)
assert(st.cursor == 4 and st.offset == 4, 'offset follows cursor up')

-- Shrinking the list clamps cursor and offset.
st.results = { unpack(items, 1, 10) }
picker._debug.clamp_view()
assert(st.cursor <= 10, 'cursor clamped to shrunk list')
assert(st.offset == 1, 'offset clamped to 1 after shrink')

picker._debug.cancel()

print('SELECT TEST PASSED')
