-- pp.nvim — workspace profile manager.
--
-- Curated project groups stored in `<stdpath("data")>/workspaces.json`.
-- Lets you group repositories into named profiles (e.g. "default", "work",
-- "oss") and jump to any project in the active profile using pp's tab/files
-- orchestration.

local util = require('pp.util')

local M = {}
local data_file = vim.fn.stdpath('data') .. '/workspaces.json'

-- In-memory state: active workspace profile per Neovim instance
local current_profile = 'default'

local function load_data()
  local file = io.open(data_file, 'r')
  if not file then
    return { default = {} }
  end
  local content = file:read('*all')
  file:close()
  local ok, data = pcall(vim.json.decode, content)
  if not ok or type(data) ~= 'table' then
    return { default = {} }
  end

  if data.profiles then
    return data.profiles
  end

  return data
end

local function save_data(profiles)
  local dir = vim.fs.dirname(data_file)
  if vim.fn.isdirectory(dir) == 0 then
    vim.fn.mkdir(dir, 'p')
  end

  local file = io.open(data_file, 'w')
  if file then
    file:write(vim.json.encode(profiles))
    file:close()
  end
end

local function get_active_profile_projects(profiles)
  if not profiles[current_profile] then
    profiles[current_profile] = {}
  end
  return profiles[current_profile]
end

local function resolve_path(path, callback)
  if path and path ~= '' then
    local expanded = vim.fn.expand(path)
    if vim.fn.isdirectory(expanded) ~= 0 then
      callback(expanded)
    else
      util.notify('Not a valid directory: ' .. expanded, vim.log.levels.ERROR)
    end
    return
  end

  local root = util.project_root(0)
  if root and root ~= '' then
    callback(root)
  else
    vim.ui.input({
      prompt = 'Could not detect root. Enter project path: ',
      default = vim.fn.getcwd(),
      completion = 'file',
    }, function(input_path)
      if not input_path or input_path == '' then
        return
      end
      local expanded = vim.fn.expand(input_path)
      if vim.fn.isdirectory(expanded) == 0 then
        util.notify('Not a valid directory: ' .. expanded, vim.log.levels.ERROR)
        return
      end
      callback(expanded)
    end)
  end
end

local function add_project_to_profile(profile_name, project_path)
  local profiles = load_data()
  if not profiles[profile_name] then
    profiles[profile_name] = {}
  end

  for _, p in ipairs(profiles[profile_name]) do
    if p == project_path then
      util.notify(
        string.format("Project already marked in workspace '%s'", profile_name),
        vim.log.levels.INFO
      )
      return
    end
  end

  table.insert(profiles[profile_name], project_path)
  save_data(profiles)
  util.notify(
    string.format("Project marked in workspace '%s': %s", profile_name, project_path),
    vim.log.levels.INFO
  )
end

--- Focus existing tab page for `target_path` or open a new tab via pp.open_in_tab.
function M.open_workspace(target_path)
  require('pp').open_in_tab(target_path)
end

--- Create a new workspace profile.
function M.create(name)
  local action = function(profile_name)
    if not profile_name or profile_name == '' then
      return
    end
    profile_name = profile_name:gsub('%s+', '-'):lower()

    local profiles = load_data()
    if profiles[profile_name] then
      util.notify(
        string.format("Workspace profile '%s' already exists!", profile_name),
        vim.log.levels.WARN
      )
      return
    end
    profiles[profile_name] = {}
    current_profile = profile_name
    save_data(profiles)
    util.notify(
      string.format('Created and switched to workspace profile: %s', profile_name),
      vim.log.levels.INFO
    )
  end

  if name and name ~= '' then
    action(name)
  else
    vim.ui.input({ prompt = 'Enter new workspace profile name: ' }, action)
  end
end

--- Mark (assign) current project directory to a chosen workspace profile.
function M.mark(path)
  resolve_path(path, function(resolved_path)
    local profiles = load_data()
    local entries = { '[Create New Workspace]' }
    for name in pairs(profiles) do
      table.insert(entries, name)
    end

    util.select(entries, { prompt = 'Assign Project to Workspace> ' }, function(choice)
      if not choice then
        return
      end

      if choice == '[Create New Workspace]' then
        vim.schedule(function()
          vim.ui.input({ prompt = 'Enter new workspace name: ' }, function(new_name)
            if not new_name or new_name == '' then
              return
            end
            new_name = new_name:gsub('%s+', '-'):lower()
            local p = load_data()
            if not p[new_name] then
              p[new_name] = {}
            end
            current_profile = new_name
            save_data(p)
            add_project_to_profile(new_name, resolved_path)
          end)
        end)
      else
        add_project_to_profile(choice, resolved_path)
      end
    end)
  end)
end

--- Unmark project from one of the workspaces it is assigned to.
function M.unmark(path)
  resolve_path(path, function(resolved_path)
    local profiles = load_data()
    local matching = {}

    for name, projs in pairs(profiles) do
      for _, p in ipairs(projs) do
        if p == resolved_path then
          table.insert(matching, name)
          break
        end
      end
    end

    if #matching == 0 then
      util.notify('Project is not registered in any workspace profile.', vim.log.levels.WARN)
      return
    elseif #matching == 1 then
      local target_profile = matching[1]
      local projs = profiles[target_profile]
      for i, p in ipairs(projs) do
        if p == resolved_path then
          table.remove(projs, i)
          break
        end
      end
      save_data(profiles)
      util.notify(
        string.format("Removed project (unmarked) from '%s': %s", target_profile, resolved_path),
        vim.log.levels.INFO
      )
    else
      util.select(matching, { prompt = 'Unmark Project from Workspace> ' }, function(choice)
        if not choice then
          return
        end
        local p = load_data()
        local projs = p[choice] or {}
        for i, pr in ipairs(projs) do
          if pr == resolved_path then
            table.remove(projs, i)
            break
          end
        end
        save_data(p)
        util.notify(
          string.format("Removed project (unmarked) from '%s': %s", choice, resolved_path),
          vim.log.levels.INFO
        )
      end)
    end
  end)
end

--- Remove a workspace profile.
function M.remove(name)
  local action = function(profile_name)
    if not profile_name or profile_name == '' then
      return
    end
    if profile_name == 'default' then
      util.notify('Cannot delete the default workspace profile!', vim.log.levels.WARN)
      return
    end

    local profiles = load_data()
    if not profiles[profile_name] then
      util.notify(
        string.format("Workspace profile '%s' does not exist!", profile_name),
        vim.log.levels.WARN
      )
      return
    end

    profiles[profile_name] = nil
    if current_profile == profile_name then
      current_profile = 'default'
    end
    save_data(profiles)
    util.notify(string.format('Deleted workspace profile: %s', profile_name), vim.log.levels.INFO)
  end

  if name and name ~= '' then
    action(name)
  else
    local profiles = load_data()
    local entries = {}
    for k in pairs(profiles) do
      if k ~= 'default' then
        table.insert(entries, k)
      end
    end

    if #entries == 0 then
      util.notify('No custom workspace profiles to delete.', vim.log.levels.WARN)
      return
    end

    util.select(entries, { prompt = 'Delete Workspace Profile> ' }, function(choice)
      if choice then
        action(choice)
      end
    end)
  end
end

--- List all workspace profiles and switch active profile.
function M.list()
  local profiles = load_data()
  local entries = {}
  for name in pairs(profiles) do
    if name == current_profile then
      table.insert(entries, string.format('%s (active)', name))
    else
      table.insert(entries, name)
    end
  end

  util.select(entries, { prompt = 'Workspaces> ' }, function(choice)
    if not choice then
      return
    end
    local profile_name = choice:gsub(' %(active%)$', '')
    vim.schedule(function()
      current_profile = profile_name
      util.notify(
        string.format('Switched to workspace profile: %s', profile_name),
        vim.log.levels.INFO
      )
    end)
  end)
end

--- List all projects in active workspace profile and open the chosen one.
function M.projects()
  local profiles = load_data()
  local projs = get_active_profile_projects(profiles)

  if #projs == 0 then
    util.notify(
      string.format(
        "No projects registered in workspace profile '%s'. Use :PpWorkspaceMark!",
        current_profile
      ),
      vim.log.levels.WARN
    )
    return
  end

  local entries = {}
  for _, path in ipairs(projs) do
    table.insert(entries, string.format('%s\t(%s)', vim.fs.basename(path), path))
  end

  util.select(
    entries,
    { prompt = string.format('Projects (%s)> ', current_profile) },
    function(choice)
      if not choice then
        return
      end
      local target_path = choice:match('%((.+)%)$')
      if target_path then
        M.open_workspace(target_path)
      end
    end
  )
end

return M
