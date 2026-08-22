-- pp.nvim — public API.
--
--   require('pp').setup(opts)                       configure (picker, prompts)
--   require('pp').switch_project_in_new_tab()       (:PpProject)
--   require('pp').switch_project()                  (:PpSwitch)
--   require('pp').search(mode)                      (:PpSearch [mode])
--   require('pp').reindex()                         (:PpIndex)
--   require('pp').clear()                           (:PpClear)
--
-- Data/UI split: `projects.lua` is the data layer (CLI), `picker.lua` is the
-- UI layer (FFI float picker by default, fzf-lua fallback). This module wires
-- them together.

local config = require('pp.config')
local projects = require('pp.projects')
local picker = require('pp.picker')
local util = require('pp.util')

local M = {}

--- Configure pp.nvim. `opts`: binary, prompt, picker, lib_path, default_mode,
--- fuzzy_distance, max_results, debounce_ms.
function M.setup(opts)
  config.setup(opts)
end

--- Data layer re-exports (for advanced users / scripting).
M.projects = projects.list
M.fresh_projects = projects.fresh
M.reindex = projects.reindex
M.clear = projects.clear

-- ---------------------------------------------------------------------------
-- Orchestration
-- ---------------------------------------------------------------------------

local function open_files(path)
  local name = vim.fs.basename(path)
  local fzf = picker.get_files()
  if not fzf then
    return
  end
  fzf.files({
    cwd = path,
    prompt = string.format(config.options.files_prompt, name),
  })
end

-- ---------------------------------------------------------------------------
-- Native cdylib build (lazy.nvim `build` hook, `:PpBuild`)
-- ---------------------------------------------------------------------------

local function plugin_root()
  -- lua/pp/init.lua -> three dirnames up.
  local source = debug.getinfo(1, 'S').source or ''
  local file = source:sub(1, 1) == '@' and source:sub(2) or source
  return vim.fs.dirname(vim.fs.dirname(vim.fs.dirname(file)))
end

--- The cdylib name cargo produces for the host platform.
local function cdylib_name()
  if jit.os == 'OSX' then
    return 'libpp_nvim.dylib'
  end
  if jit.os == 'Windows' then
    return 'pp_nvim.dll'
  end
  return 'libpp_nvim.so'
end

--- Build the FFI cdylib with `-C target-cpu=native` and install it into
--- `<plugin>/build/`. Blocks until done so lazy.nvim's build hook can rely on
--- it:
---
---   { 'zerosign/pp', build = function() require('pp').build_native() end }
---
--- The result is tuned for the build machine and is not portable to other
--- CPUs — the right tradeoff for a per-machine plugin install. Requires a
--- Rust toolchain (`cargo`) on PATH.
function M.build_native()
  local root = plugin_root()
  local name = cdylib_name()
  local stderr = {}
  local job = vim.fn.jobstart({ 'cargo', 'build', '-p', 'pp-nvim', '--release' }, {
    cwd = root,
    env = { RUSTFLAGS = '-C target-cpu=native' },
    stderr_buffered = true,
    on_stderr = function(_, data)
      for _, line in ipairs(data or {}) do
        stderr[#stderr + 1] = line
      end
    end,
  })
  if job <= 0 then
    util.notify('pp.nvim: could not start cargo (is Rust on your PATH?)', vim.log.levels.ERROR)
    return
  end
  if vim.fn.jobwait({ job })[1] ~= 0 then
    util.notify('pp.nvim build failed: ' .. table.concat(stderr, '\n'), vim.log.levels.ERROR)
    return
  end
  local build_dir = vim.fs.joinpath(root, 'build')
  vim.fn.mkdir(build_dir, 'p')
  local ok_copy, copied = pcall(
    vim.uv.fs_copyfile,
    vim.fs.joinpath(root, 'target', 'release', name),
    vim.fs.joinpath(build_dir, name)
  )
  if not ok_copy or not copied then
    util.notify(
      'pp.nvim: build OK but could not copy ' .. name .. ' into build/',
      vim.log.levels.ERROR
    )
    return
  end
  util.notify('pp.nvim: native lib installed (build/' .. name .. ')')
end

local function open_in_tab(path)
  if not path or path == '' then
    return
  end
  -- Schedule so any picker window closes cleanly first.
  vim.schedule(function()
    vim.cmd('tabnew')
    vim.cmd('tcd ' .. vim.fn.fnameescape(path))
    open_files(path)
    util.notify('Opened workspace: ' .. vim.fs.basename(path))
  end)
end

--- Pick a project, open it in a new tab, `tcd` into it, then open its files.
function M.switch_project_in_new_tab()
  picker.pick({
    prompt = config.options.prompt,
    on_select = open_in_tab,
  })
end

--- Pick a project in `mode` (substring|fuzzy|prefix|subseq), then open it in a
--- new tab. The default mode applies when `mode` is nil.
function M.search(mode)
  picker.pick({
    prompt = config.options.prompt,
    mode = mode,
    on_select = open_in_tab,
  })
end

--- Pick a project and switch the current window's cwd to it.
function M.switch_project()
  picker.pick({
    prompt = 'Switch Project> ',
    on_select = function(path)
      if not path or path == '' then
        return
      end
      vim.api.nvim_set_current_dir(path)
      open_files(path)
    end,
  })
end

return M
