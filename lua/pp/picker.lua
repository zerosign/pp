-- pp.nvim — picker layer.
--
-- Default: a zero-dependency floating window backed by the pp FFI (cdylib).
-- Every keystroke runs the search in Rust (`pp_search`); Lua only renders the
-- returned matches, so no competing matcher re-filters the results. Falls
-- back to fzf-lua (full list + fzf's own matcher) when the shared library is
-- missing, and honours `config.options.picker` for a fully custom picker.
--
-- fzf-lua is still used for the post-selection `files` picker.

local config = require('pp.config')
local projects = require('pp.projects')
local util = require('pp.util')

local M = {}

local ffi, lib
local warned_fallback = false

-- LuaJIT 2.1 extras: table.new pre-allocates, and localized string builtins
-- keep the per-keystroke hot path in compiled traces.
local table_new = require('table.new')
local byte, sub = string.byte, string.sub

-- ---------------------------------------------------------------------------
-- FFI bridge (LuaJIT ffi.load of libpp_nvim.so)
-- ---------------------------------------------------------------------------

local MODES = {
  substring = 0,
  prefix = 1,
  fuzzy = 2,
  subseq = 3,
}

local function resolve_lib_path()
  -- Default: <plugin>/build/libpp_nvim.so — three dirnames up from picker.lua.
  local source = debug.getinfo(1, 'S').source or ''
  local file = source:sub(1, 1) == '@' and source:sub(2) or source
  local base = vim.fs.joinpath(vim.fs.dirname(vim.fs.dirname(vim.fs.dirname(file))), 'build')
  -- First readable candidate wins; a nil configured path is simply skipped.
  -- The dll name must match what cargo emits for the cdylib (pp_nvim.dll,
  -- no `lib` prefix) — the same name build_native() installs.
  local candidates = {
    config.options.lib_path,
    vim.fs.joinpath(base, 'libpp_nvim.so'),
    vim.fs.joinpath(base, 'libpp_nvim.dylib'),
    vim.fs.joinpath(base, 'pp_nvim.dll'),
  }
  for i = 1, #candidates do
    local path = candidates[i]
    if path and vim.fn.filereadable(path) == 1 then
      return path
    end
  end
  return nil
end

local function load_lib()
  if lib then
    return true
  end

  if not ffi then
    local ok, result = pcall(require, 'ffi')
    if not ok then
      return false
    end
    ffi = result
  end

  local path = resolve_lib_path()

  if not path then
    return false
  end

  ffi.cdef([[
    void *pp_search(const char *query, int mode, unsigned int distance, int limit);
    void pp_string_free(void *ptr);
    int pp_refresh(void);
  ]])

  local ok, handle = pcall(ffi.load, path)
  if not ok then
    util.notify(
      'pp.nvim: failed to load ' .. path .. ': ' .. tostring(handle),
      vim.log.levels.ERROR
    )
    return false
  end
  lib = handle
  return true
end

--- Split newline-separated FFI output into a list of lines.
---
--- Byte-scan rather than string.gmatch: the pattern-matching engine is a C
--- call that would stitch (abort) the JIT trace on every keystroke, whereas
--- string.byte / string.sub compile to native IR. `size_hint` pre-allocates
--- the result table so it never grows dynamically.
local function split_lines(text, size_hint)
  local lines = table_new(size_hint, 0)
  local line_start = 1
  for i = 1, #text do
    if byte(text, i) == 10 then -- '\n'
      if i > line_start then
        lines[#lines + 1] = sub(text, line_start, i - 1)
      end
      line_start = i + 1
    end
  end
  if line_start <= #text then
    lines[#lines + 1] = sub(text, line_start)
  end
  return lines
end

--- Search through the shared library. Returns a list of paths, or nil when
--- the library failed (callers fall back or show an error).
local function search(query, mode, limit)
  if not load_lib() then
    return nil
  end
  local ptr = lib.pp_search(query, MODES[mode] or 0, config.options.fuzzy_distance, limit or -1)
  -- A NULL pointer return arrives as Lua nil (library error), so one guard
  -- covers both cases; ffi.string and the free cannot throw on a valid ptr.
  if ptr == nil then
    return nil
  end
  local text = ffi.string(ptr)
  lib.pp_string_free(ptr)
  return split_lines(text, config.options.max_results)
end

-- ---------------------------------------------------------------------------
-- Floating picker (FFI-backed)
-- ---------------------------------------------------------------------------

local MODE_ORDER = { 'substring', 'fuzzy', 'prefix', 'subseq' }
-- Human-readable mode names for the footer — no raw `[substring]` jargon.
local MODE_LABELS = {
  substring = 'contains',
  prefix = 'starts with',
  fuzzy = 'fuzzy',
  subseq = 'in order',
}
-- Footer hints (rendered on the float border, outside the editable buffer).
local HINTS = '↑↓/jk select · Enter open · Esc close · C-f mode'

local NS = vim.api.nvim_create_namespace('pp-selection')
local NS_PROMPT = vim.api.nvim_create_namespace('pp-prompt')

-- Selection highlight. Linked to Visual so it follows the active colorscheme;
-- a colorscheme that defines PpSelection itself takes precedence.
if vim.tbl_isempty(vim.api.nvim_get_hl(0, { name = 'PpSelection' })) then
  vim.api.nvim_set_hl(0, 'PpSelection', { link = 'Visual' })
end

local state = {
  buf = nil,
  win = nil,
  prompt = nil,
  results = {},
  cursor = 1,
  offset = 1, -- index of the first result row currently rendered in the window
  mode = 'substring',
  timer = nil,
  on_select = nil,
}

local function next_mode(current)
  for i, m in ipairs(MODE_ORDER) do
    if m == current then
      return MODE_ORDER[i % #MODE_ORDER + 1]
    end
  end
  return MODE_ORDER[1]
end

local function close_float()
  if state.timer then
    state.timer:stop()
    state.timer:close()
    state.timer = nil
  end
  if state.win and vim.api.nvim_win_is_valid(state.win) then
    vim.api.nvim_win_close(state.win, true)
  end
  if state.buf and vim.api.nvim_buf_is_valid(state.buf) then
    vim.api.nvim_buf_delete(state.buf, { force = true })
  end
  state.win, state.buf = nil, nil
  state.results, state.cursor, state.offset = {}, 1, 1
end

local function current_query()
  -- The prompt is inline virtual text, not part of the buffer, so the query
  -- is simply the whole first line and can never contain the prompt.
  local line = vim.api.nvim_buf_get_lines(state.buf, 0, 1, false)[1] or ''
  return line
end

--- Number of result rows that fit in the float window. The first buffer row
--- is the query line; the mode hints live in the float footer (window border),
--- so they don't consume a row.
local function visible_count()
  if not state.win or not vim.api.nvim_win_is_valid(state.win) then
    return 12
  end
  return math.max(1, vim.api.nvim_win_get_height(state.win) - 2)
end

--- Keep cursor and offset inside the current result list: clamp the cursor to
--- the list length and pull the viewport back when the list shrinks.
local function clamp_view()
  local n = #state.results
  if n == 0 then
    state.cursor, state.offset = 1, 1
    return
  end
  state.cursor = math.max(1, math.min(state.cursor, n))
  local vis = visible_count()
  state.offset = math.max(1, math.min(state.offset, n - vis + 1))
end

--- Render only the visible slice of results (a viewport), each prefixed with a
--- selection marker (`>` for the selected row). The selection row is
--- highlighted and, because the viewport is exactly window-sized, the highlight
--- can never land off-screen.
local function render()
  local results = state.results
  local n = #results
  local vis = visible_count()
  -- Pre-allocated so rendering never grows the table dynamically.
  local view = table_new(vis, 0)
  if n == 0 then
    view[1] = '  (no matches)'
  else
    local cursor, offset = state.cursor, state.offset
    local last = math.min(offset + vis - 1, n)
    for i = offset, last do
      view[i - offset + 1] = ((i == cursor) and '> ' or '  ') .. results[i]
    end
  end
  -- Pad with empty rows so the window bottom edge stays stable.
  for i = #view + 1, vis do
    view[i] = ''
  end
  vim.api.nvim_buf_set_lines(state.buf, 1, -1, false, view)
  vim.api.nvim_buf_clear_namespace(state.buf, NS, 1, -1)
  if n > 0 then
    local row = 1 + (state.cursor - state.offset)
    vim.api.nvim_buf_set_extmark(state.buf, NS, row, 0, {
      end_row = row,
      end_col = #results[state.cursor] + 2,
      hl_group = 'PpSelection',
    })
  end
end

local function do_search()
  if not state.win or not vim.api.nvim_win_is_valid(state.win) then
    return
  end
  local query = current_query()
  local mode = state.mode
  if query == '' then
    mode = 'substring' -- empty query lists everything
  end
  local results = search(query, mode, config.options.max_results)
  if results == nil then
    state.results = {}
    vim.api.nvim_buf_set_lines(state.buf, 1, -1, false, { '  (search error)' })
    vim.api.nvim_buf_clear_namespace(state.buf, NS, 1, -1)
    return
  end
  state.results = results
  clamp_view()
  render()
end

-- Hoisted so the per-keystroke timer start reuses one closure instead of
-- allocating a fresh one each time (FNEW would abort the JIT trace).
local debounced_search = vim.schedule_wrap(do_search)

local function schedule_search()
  if not state.timer then
    state.timer = vim.uv.new_timer()
  end
  state.timer:stop()
  state.timer:start(config.options.debounce_ms, 0, debounced_search)
end

local function move_selection(delta)
  local n = #state.results
  if n == 0 then
    return
  end
  state.cursor = ((state.cursor - 1 + delta + n) % n) + 1
  local vis = visible_count()
  if state.cursor < state.offset then
    state.offset = state.cursor
  elseif state.cursor > state.offset + vis - 1 then
    state.offset = state.cursor - vis + 1
  end
  render()
end

--- Footer text: key hints, plus the current match mode (in plain words) when
--- it is not the configured default — so the default UI stays clean.
local function footer_text()
  local mode = state.mode
  if mode == config.options.default_mode then
    return HINTS
  end
  return 'match: ' .. (MODE_LABELS[mode] or mode) .. ' · ' .. HINTS
end

local function cycle_mode()
  state.mode = next_mode(state.mode)
  vim.api.nvim_win_set_config(state.win, { footer = footer_text() })
  schedule_search()
end

local function accept()
  local path = state.results[state.cursor]
  local on_select = state.on_select
  close_float()

  if path then
    on_select(path)
  end
end

local function cancel()
  close_float()
end

local function open_float(opts)
  local width = math.min(math.floor(vim.o.columns * 0.6), 100)
  local height = 14
  local row = math.max(1, math.floor((vim.o.lines - height) / 2))
  local col = math.floor((vim.o.columns - width) / 2)

  local mode = opts.mode or config.options.default_mode
  state.mode = MODES[mode] and mode or 'substring'
  state.on_select = opts.on_select
  state.prompt = opts.prompt or config.options.prompt

  local buf = vim.api.nvim_create_buf(false, true)

  local win = vim.api.nvim_open_win(buf, true, {
    relative = 'editor',
    width = width,
    height = height,
    row = row,
    col = col,
    style = 'minimal',
    border = 'rounded',
    footer = footer_text(),
    footer_pos = 'left',
  })

  state.buf, state.win = buf, win

  vim.api.nvim_buf_set_lines(buf, 0, -1, false, { '' })

  -- Completion plugins (blink.cmp, nvim-cmp) would auto-open their popup over
  -- the results while typing. Both expose per-buffer kill switches; the float
  -- is current after nvim_open_win(enter=true), so cmp.setup.buffer applies.
  vim.b[buf].completion = false -- blink.cmp
  local ok_cmp, cmp = pcall(require, 'cmp')
  if ok_cmp then
    cmp.setup.buffer({ enabled = false }) -- nvim-cmp
  end

  -- The prompt is inline virtual text: it renders ahead of the query but is
  -- not part of the buffer, so it can never be deleted or edited.
  -- right_gravity = false keeps it anchored at col 0: with the default (true)
  -- every keystroke pushes the extmark right and the prompt trails the query.
  vim.api.nvim_buf_set_extmark(buf, NS_PROMPT, 0, 0, {
    virt_text = { { state.prompt, 'Comment' } },
    virt_text_pos = 'inline',
    right_gravity = false,
  })

  vim.api.nvim_create_autocmd({ 'TextChangedI', 'TextChanged' }, {
    buffer = buf,
    callback = schedule_search,
  })

  local function imap(lhs, fn)
    vim.keymap.set('i', lhs, fn, { buffer = buf, nowait = true })
  end
  local function nmap(lhs, fn)
    vim.keymap.set('n', lhs, fn, { buffer = buf, nowait = true })
  end
  local actions = {
    ['<CR>'] = accept,
    ['<Esc>'] = cancel,
    ['<C-c>'] = cancel,
    ['<C-f>'] = cycle_mode,
    ['<C-n>'] = function()
      move_selection(1)
    end,
    ['<C-p>'] = function()
      move_selection(-1)
    end,
    ['<Down>'] = function()
      move_selection(1)
    end,
    ['<Up>'] = function()
      move_selection(-1)
    end,
  }
  for lhs, fn in pairs(actions) do
    imap(lhs, fn)
    nmap(lhs, fn)
  end
  -- The float is insert-only. Escaping to normal mode (<C-o>, <C-\><C-n>)
  -- would allow line ops (dd, x, p...) to delete the query row, which pulls
  -- result text up into the editable query line and corrupts the picker.
  -- Swallow the escape hatches; the nmaps above stay as a safety net.
  imap('<C-o>', function() end)
  imap('<C-\\><C-n>', function() end)

  nmap('j', function()
    move_selection(1)
  end)

  nmap('k', function()
    move_selection(-1)
  end)

  nmap('q', cancel)

  -- `nvim_win_set_cursor` clamps to the last character; `startinsert!`
  -- behaves like `a` (append after the cursor). The cursor sits at the start
  -- of the query line (column 0) which renders just past the virtual prompt.
  vim.api.nvim_win_set_cursor(win, { 1, 0 })
  vim.cmd('startinsert!')

  vim.schedule(do_search)
end

-- ---------------------------------------------------------------------------
-- fzf-lua / custom picker (fallback path)
-- ---------------------------------------------------------------------------

local function fzf_lua_picker()
  local ok, fzf = pcall(require, 'fzf-lua')
  if not ok then
    util.notify('pp.nvim: fzf-lua is required (or provide `opts.picker`)', vim.log.levels.ERROR)
    return nil
  end
  return {
    pick = function(items, opts)
      fzf.fzf_exec(items, opts)
    end,
    files = function(opts)
      fzf.files(opts)
    end,
  }
end

local function pick_custom(opts)
  projects.list(function(list)
    if #list == 0 then
      util.notify('No projects found. Run `:PpIndex` to build the index.', vim.log.levels.WARN)
      return
    end
    local fzf = fzf_lua_picker()
    if not fzf then
      return
    end
    fzf.pick(list, {
      prompt = opts.prompt,
      winopts = { preview = { hidden = 'hidden' } },
      actions = {
        ['default'] = function(selected)
          if not selected or selected[1] == '' then
            return
          end
          opts.on_select(selected[1])
        end,
      },
    })
  end)
end

-- ---------------------------------------------------------------------------
-- Public API
-- ---------------------------------------------------------------------------

--- The picker used for the post-selection `files` step (fzf-lua, or the
--- configured custom picker). Returns nil if none is available.
function M.get_files()
  if config.options.picker then
    return config.options.picker
  end
  return fzf_lua_picker()
end

--- Open the project picker. `opts`: `prompt`, `mode`, `on_select(path)`.
function M.pick(opts)
  if config.options.picker then
    pick_custom(opts)
    return
  end
  if load_lib() then
    open_float(opts)
    return
  end
  if not warned_fallback then
    warned_fallback = true
    util.notify(
      'pp.nvim: FFI library not found; falling back to fzf-lua. '
        .. 'Run `just nvim` in the pp repo to build it.',
      vim.log.levels.WARN
    )
  end
  pick_custom(opts)
end

-- Testing seam for headless tests (nvim without a UI does not dispatch
-- keymaps, so the float's actions are exercised directly here). Not part of
-- the public API.
M._debug = {
  state = state,
  cycle_mode = cycle_mode,
  move_selection = move_selection,
  accept = accept,
  cancel = cancel,
  do_search = do_search,
  render = render,
  clamp_view = clamp_view,
}

return M
