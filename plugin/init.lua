-- pp.nvim — plugin entry point: user commands and default keymaps.
-- The logic lives in lua/pp/ (config, projects, picker, init).

local pp = require('pp')

local function cmd(name, fn, opts)
  vim.api.nvim_create_user_command(name, fn, opts or {})
end

cmd('PpProject', function()
  pp.switch_project_in_new_tab()
end, { desc = 'Open a pp project in a new tab (tcd + files picker)' })
cmd('PpSwitch', function()
  pp.switch_project()
end, { desc = 'Switch pp project: set cwd and open its files' })
cmd('PpSearch', function(a)
  local mode = (a.args or ''):gsub('%s+', '')
  pp.search(mode ~= '' and mode or nil)
end, {
  desc = 'Search pp projects (mode: substring|fuzzy|prefix|subseq)',
  nargs = '?',
})
cmd('PpIndex', function()
  pp.reindex()
end, { desc = 'Rebuild the pp repository index' })
cmd('PpClear', function()
  pp.clear()
end, { desc = 'Clear the pp repository index' })
cmd('PpBuild', function()
  pp.build_native()
end, { desc = 'Build the FFI cdylib with -C target-cpu=native' })
