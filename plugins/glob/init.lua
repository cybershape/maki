local truncate = require("maki.truncate")
local ToolView = require("maki.tool_view")
local shorten_path = require("maki.shorten_path")
local output_limits = require("maki.output_limits")

local NO_FILES_FOUND = "No files found"

local opts = maki.api.register_options(output_limits.extend({
  search_result_limit = { default = 100, min = 10, desc = "Max files the model sees per search." },
}))

local function glob_view_opts(ctx)
  local tol = ctx:tool_output_lines()
  return { max_lines = (tol and tol.grep) or 3, keep = "head" }
end

local function build_glob_view(lines, ctx)
  local shortened = {}
  for i, line in ipairs(lines) do
    shortened[i] = shorten_path(line)
  end
  return ToolView.restore_lines(shortened, glob_view_opts(ctx))
end

maki.api.register_tool({
  name = "glob",
  permission = "fs_read",
  kind = "search",
  description = [[Find files by glob pattern.

- Respects .gitignore.
- Returns absolute paths sorted by modification time (newest first).
- Prefer speculative parallel searches over sequential rounds of glob+grep.]],

  schema = {
    type = "object",
    properties = {
      pattern = { type = "string", description = "Glob pattern (e.g. **/*.rs, src/**/*.ts)", required = true },
      path = { type = "string", description = "Directory to search in (default: cwd)" },
    },
  },

  header = function(input)
    local buf = maki.ui.buf()
    local spans = { { shorten_path(input.pattern or ""), "tool" } }
    if input.path then
      spans[#spans + 1] = { " in ", "dim" }
      spans[#spans + 1] = { shorten_path(input.path), "path" }
    end
    buf:line(spans)
    return buf
  end,

  restore = function(_input, output, is_error, ctx)
    if is_error or output == NO_FILES_FOUND then
      return ToolView.restore(output, glob_view_opts(ctx))
    end
    return build_glob_view(maki.split(output, "\n"), ctx)
  end,

  handler = function(input, ctx)
    local pattern = input.pattern
    if not pattern then
      return { llm_output = "error: pattern is required", is_error = true }
    end

    local max_lines, max_bytes = output_limits.resolve(opts, ctx)

    local files, err = maki.fs.glob(pattern, {
      path = input.path,
      gitignore = true,
      sort = "mtime",
      limit = ctx:output_mode() == "compact" and opts.search_result_limit or nil,
    })

    if not files then
      return { llm_output = "error: " .. err, is_error = true }
    end

    if #files == 0 then
      return { llm_output = NO_FILES_FOUND }
    end

    local text = table.concat(files, "\n")
    local llm_output = truncate(text, max_lines, max_bytes)

    return {
      llm_output = llm_output,
      body = build_glob_view(files, ctx),
    }
  end,
})
