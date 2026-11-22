-- pp.nvim — plugin entry point: user commands and default keymaps.
-- The logic lives in lua/pp/ (config, projects, picker, init).

local pp = require('pp')

local function cmd(name, fn, opts)
  vim.api.nvim_create_user_command(
    name,
    fn,
    vim.tbl_extend('force', { force = true }, opts or {})
  )
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

-- Workspace profiles (curated project groups)
cmd('PpWorkspaceMark', function(a)
  pp.workspace.mark(a.args ~= '' and a.args or nil)
end, { desc = 'Mark (add) project to a workspace profile', nargs = '?' })
cmd('PpWorkspaceUnmark', function(a)
  pp.workspace.unmark(a.args ~= '' and a.args or nil)
end, { desc = 'Unmark (remove) project from a workspace profile', nargs = '?' })
cmd('PpWorkspaceCreate', function(a)
  pp.workspace.create(a.args ~= '' and a.args or nil)
end, { desc = 'Create a new workspace profile', nargs = '?' })
cmd('PpWorkspaceRemove', function(a)
  pp.workspace.remove(a.args ~= '' and a.args or nil)
end, { desc = 'Remove a workspace profile', nargs = '?' })
cmd('PpWorkspaceList', function()
  pp.workspace.list()
end, { desc = 'List / switch workspace profile' })
cmd('PpWorkspaceProjects', function()
  pp.workspace.projects()
end, { desc = 'List & open projects in the active workspace profile' })

-- Per-project region bookmarks (collection)
cmd('PpCollectionAdd', function()
  pp.collection.add()
end, { desc = 'Add visual selection to project collection', range = true })
cmd('PpCollectionPick', function()
  pp.collection.pick()
end, { desc = 'Pick project bookmarks from collection' })
