-- pp.nvim — per-project region bookmarks.
--
-- Storage: `<project-root>/.nvim/collection.json`
-- Project root is always resolved dynamically from the active buffer/tab via
-- `pp.util.project_root(0)` (never cached in a stale module global). File paths
-- inside `collection.json` are stored relative to `<project-root>` so bookmarks
-- remain portable if the repository is moved or renamed.

local util = require('pp.util')

local M = {}

local function collection_file(root)
  return vim.fs.joinpath(root, '.nvim', 'collection.json')
end

local function resolve_entry_path(root, entry_path)
  if not entry_path or entry_path == '' then
    return ''
  end
  if entry_path:sub(1, 1) == '/' then
    return vim.fs.normalize(entry_path)
  end
  return vim.fs.normalize(vim.fs.joinpath(root, entry_path))
end

local function load_entries(root)
  local file = io.open(collection_file(root), 'r')
  if not file then
    return {}
  end
  local content = file:read('*all')
  file:close()
  local ok, data = pcall(vim.json.decode, content)
  if not ok or type(data) ~= 'table' then
    return {}
  end
  return data
end

local function save_entries(root, entries)
  local file = collection_file(root)
  local dir = vim.fs.dirname(file)
  if vim.fn.isdirectory(dir) == 0 then
    vim.fn.mkdir(dir, 'p')
  end
  local f = io.open(file, 'w')
  if f then
    f:write(vim.json.encode(entries))
    f:close()
  end
end

local function show_region(entry)
  local last_line = vim.api.nvim_buf_line_count(0)
  local start_line = math.min(entry.start or 1, last_line)
  vim.api.nvim_win_set_cursor(0, { start_line, 0 })

  local end_line = entry['end']
  if end_line then
    end_line = math.min(end_line, last_line)
    if end_line > start_line then
      vim.cmd('normal! V')
      vim.api.nvim_win_set_cursor(0, { end_line, 0 })
    end
  end
end

--- Jump to a bookmark within the current tabpage (never yanking focus across
--- unrelated workspace tabs).
local function goto_entry(root, entry, opener)
  local abs_path = resolve_entry_path(root, entry.path)
  if vim.fn.filereadable(abs_path) == 0 then
    util.notify('Bookmark file not found: ' .. abs_path, vim.log.levels.ERROR)
    return
  end

  local bufnr = vim.fn.bufnr(abs_path)
  local target_win = nil
  if bufnr > 0 then
    for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
      if vim.api.nvim_win_get_buf(win) == bufnr then
        target_win = win
        break
      end
    end
  end

  if target_win then
    vim.api.nvim_set_current_win(target_win)
  else
    opener(abs_path)
  end

  show_region(entry)
end

--- Add the current visual selection to the current project's collection.
function M.add()
  local mode = vim.fn.mode()
  if mode ~= 'v' and mode ~= 'V' and mode ~= '\22' then
    util.notify('Select a region first (visual mode), then add to collection', vim.log.levels.INFO)
    return
  end

  local buf_path = vim.api.nvim_buf_get_name(0)
  if buf_path == '' then
    util.notify('Buffer has no file path', vim.log.levels.WARN)
    return
  end
  buf_path = vim.fs.normalize(buf_path)

  local start_line = math.min(vim.fn.line('v'), vim.fn.line('.'))
  local end_line = math.max(vim.fn.line('v'), vim.fn.line('.'))
  local text = vim.api.nvim_buf_get_lines(0, start_line - 1, end_line, false)

  vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Esc>', true, false, true), 'x', false)

  local root = util.project_root(0)
  local rel_path = vim.fs.relpath(root, buf_path) or buf_path
  local entries = load_entries(root)
  table.insert(entries, {
    path = rel_path,
    start = start_line,
    ['end'] = end_line,
    text = text,
    ts = os.time(),
  })
  save_entries(root, entries)

  util.notify(
    string.format(
      'Bookmarked %s:%d-%d (%d total)',
      vim.fs.basename(buf_path),
      start_line,
      end_line,
      #entries
    ),
    vim.log.levels.INFO
  )
end

--- Open the picker over the current project's bookmarks.
function M.pick()
  local root = util.project_root(0)
  local entries = load_entries(root)
  if #entries == 0 then
    util.notify('No bookmarks for this project yet.', vim.log.levels.INFO)
    return
  end

  local items = {}
  for idx, entry in ipairs(entries) do
    local abs_path = resolve_entry_path(root, entry.path)
    local rel = vim.fs.relpath(root, abs_path) or entry.path
    local range = string.format('%s:%d-%d', rel, entry.start or 1, entry['end'] or entry.start or 1)
    local snippet = (type(entry.text) == 'table' and entry.text[1]) and (' │ ' .. entry.text[1])
      or ''
    table.insert(items, {
      text = string.format('%3d │ %s%s', idx, range, snippet),
      file = abs_path,
      pos = { entry.start or 1, 0 },
      entry = entry,
    })
  end

  local ok, snacks = pcall(require, 'snacks')
  if ok and snacks.picker then
    snacks.picker({
      title = 'Collection (' .. vim.fs.basename(root) .. ')',
      items = items,
      format = function(item)
        return { { item.text } }
      end,
      confirm = function(picker, item)
        picker:close()
        if item and item.entry then
          goto_entry(root, item.entry, function(p)
            vim.cmd.edit(vim.fn.fnameescape(p))
          end)
        end
      end,
    })
  else
    util.select(items, {
      prompt = 'Collection (' .. vim.fs.basename(root) .. ')> ',
      format_item = function(item)
        return item.text
      end,
    }, function(item)
      if item and item.entry then
        goto_entry(root, item.entry, function(p)
          vim.cmd.edit(vim.fn.fnameescape(p))
        end)
      end
    end)
  end
end

--- Delete a bookmark entry from the current project's collection.
function M.delete_entry(entry, root)
  root = root or util.project_root(0)
  local target_abs = resolve_entry_path(root, entry.path)
  local entries = load_entries(root)
  for i, e in ipairs(entries) do
    if
      resolve_entry_path(root, e.path) == target_abs
      and (e.start or 0) == (entry.start or 0)
      and (e['end'] or 0) == (entry['end'] or 0)
    then
      table.remove(entries, i)
      save_entries(root, entries)
      util.notify(string.format('Bookmark deleted (%d remaining)', #entries), vim.log.levels.INFO)
      return
    end
  end
  util.notify('Bookmark not found', vim.log.levels.WARN)
end

M.toggle = M.pick

return M
