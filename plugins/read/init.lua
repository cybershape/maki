local ToolView = require("maki.tool_view")
local shorten_path = require("maki.shorten_path")
local output_limits = require("maki.output_limits")

local HIGHLIGHT_CONTEXT_LINES = 200
local MAX_RAW_BYTES = 3 * 1024 * 1024
local MAX_EDGE = 1568
local MAX_INPUT_BYTES = 50 * 1024 * 1024

local MEDIA_TYPES = {
  png = "image/png",
  jpeg = "image/jpeg",
  gif = "image/gif",
  webp = "image/webp",
}

local IMAGE_EXTS = {
  png = true,
  jpg = true,
  jpeg = true,
  gif = true,
  webp = true,
}

local DESCRIPTION = [[Read a file (text or image).

- Supports absolute, relative, and ~/ paths.
- For text files: returns contents with line numbers (1-indexed).
  - **offset** (default 1) and **limit** (default 0 = read to end, capped at 2000 lines).
  - Use the **index** tool or **grep** tool first to find the offset and limit.
  - Only read the sections you actually need.
  - Use `wc -l` to check total number of lines before reading to decide a reasonable limit.
  - Use truncation hints (e.g. "truncated lines X-Y") to continue with the correct offset.
  - Do not reread the same range (same file and same offset).
  - Prefer grep to locate content instead of scanning full files.
  - Call in parallel when reading multiple files.
  - Avoid tiny repeated slices - read a larger window if you need more context.
- For image files (png, jpeg, gif, webp): returns vision input so you can inspect the image.
  - Oversized images are downscaled automatically (animated gif/webp keep only the first frame).
  - offset and limit are ignored for images.]]

local opts = maki.api.register_options({
  max_line_bytes = { default = 500, min = 80, desc = "Truncate lines the model sees at this many bytes." },
  max_output_lines = output_limits.specs.max_output_lines,
})

local function format_size(bytes)
  if bytes >= 1024 * 1024 then
    return string.format("%.1fMB", bytes / (1024 * 1024))
  end
  return string.format("%dKB", math.ceil(bytes / 1024))
end

local function caption(path, bytes, width, height, note)
  return string.format("[image: %s %s %dx%d%s]", shorten_path(path), format_size(bytes), width, height, note or "")
end

local function fail(msg)
  return { llm_output = msg, is_error = true }
end

local function load_image(path, ctx)
  if ctx and ctx.supports_vision and not ctx:supports_vision() then
    return fail("cannot view " .. path .. ": current model does not support vision")
  end

  local bytes, read_err = maki.fs.read_bytes(path)
  if not bytes then
    return fail("cannot read " .. path .. ": " .. (read_err or "unknown error"))
  end
  local size = buffer.len(bytes)
  if size > MAX_INPUT_BYTES then
    return fail(
      string.format("%s is too large to view (%s; limit %s)", path, format_size(size), format_size(MAX_INPUT_BYTES))
    )
  end

  local info, probe_err = maki.image.probe(bytes)
  if not info then
    return fail(path .. " is not an image (" .. (probe_err or "unrecognized format") .. ")")
  end
  if not MEDIA_TYPES[info.format] then
    return fail("unsupported image format " .. info.format .. ": only png, jpeg, gif, and webp can be viewed")
  end

  local img, decode_err = maki.image.decode(bytes)
  if not img then
    return fail("cannot decode " .. path .. ": " .. (decode_err or "unknown error"))
  end

  if size <= MAX_RAW_BYTES and math.max(info.width, info.height) <= MAX_EDGE then
    return {
      llm_output = caption(path, size, info.width, info.height),
      image = img,
    }
  end

  local resized = math.max(info.width, info.height) > MAX_EDGE
  if resized then
    img = img:resize(MAX_EDGE, MAX_EDGE)
  end

  local out_format = info.format == "jpeg" and "jpeg" or "png"
  local encoded = img:encode(out_format)
  if #encoded > MAX_RAW_BYTES and out_format == "png" then
    out_format = "jpeg"
    encoded = img:encode(out_format)
  end
  if #encoded > MAX_RAW_BYTES then
    return fail(
      string.format(
        "%s is too large to view (%s after downscaling; limit %s)",
        path,
        format_size(#encoded),
        format_size(MAX_RAW_BYTES)
      )
    )
  end

  local note = resized and string.format(", downscaled from %dx%d", info.width, info.height) or ", re-encoded"
  if info.format == "gif" or info.format == "webp" then
    note = note .. ", first frame only"
  end

  return {
    llm_output = caption(path, #encoded, img:width(), img:height(), note),
    image = { media_type = MEDIA_TYPES[out_format], data = maki.base64.encode(encoded) },
  }
end

local function read_view_opts(ctx)
  local tol = ctx:tool_output_lines()
  return { max_lines = (tol and tol.read) or 10, keep = "head" }
end

local function apply_highlights(view, lines, ext, prefix)
  local opts = prefix and { prefix = prefix } or nil
  local highlighted = maki.ui.highlight(table.concat(lines, "\n"), ext, opts)
  if not highlighted then
    return
  end
  for i, hl_spans in ipairs(highlighted) do
    local plain = view.all_lines[i]
    if not plain then
      break
    end
    view:update_line(i, { plain[1], table.unpack(hl_spans) })
  end
  view:flush()
end

local function read_window(path, start, limit, count, ctx)
  local first = math.max(start - HIGHLIGHT_CONTEXT_LINES, 1)
  local context_len = start - first
  local page, err = maki.fs.read_lines(path, {
    offset = first,
    limit = limit and context_len + limit,
    max_line_bytes = output_limits.line_bytes(opts, ctx),
    count = count,
  })
  if not page then
    return nil, err
  end
  local lines = page.lines
  return {
    lines = table.move(lines, context_len + 1, #lines, 1, {}),
    total = page.total,
    context = table.concat(lines, "\n", 1, math.min(context_len, #lines)),
  }
end

local function build_file_view(lines, start_line, total_lines, path, ctx, prefix)
  local buf = maki.ui.buf()
  local view = ToolView.new(buf, read_view_opts(ctx))
  local nr_fmt = ToolView.line_nr_fmt(start_line + #lines - 1) .. " "

  for i, line in ipairs(lines) do
    view:append({ { string.format(nr_fmt, start_line + i - 1), "line_nr" }, { line } })
  end

  local trunc_start = start_line + #lines
  if trunc_start <= total_lines then
    view:append({
      {
        string.format(
          "... Truncated %d lines. Use offset=%d to read further.",
          total_lines - trunc_start + 1,
          trunc_start
        ),
        "dim",
      },
    })
  end

  view:finish()

  local ext = path:match("%.([^%.]+)$") or ""
  maki.async.run(function()
    apply_highlights(view, lines, ext, prefix)
  end)

  buf:on("click", function()
    view:toggle()
  end)
  return buf
end

local function read_file(path, offset, limit, ctx)
  local start = math.max(math.floor(offset), 1)
  local max_output_lines = output_limits.resolve(opts, ctx)
  local max_lines = limit > 0 and limit or nil
  if max_output_lines then
    max_lines = math.min(max_lines or max_output_lines, max_output_lines)
  end

  local window, err = read_window(path, start, max_lines, true, ctx)
  if not window then
    return { llm_output = "read error: " .. tostring(err), is_error = true }
  end
  local lines, total_lines = window.lines, window.total

  ctx:record_read(path)

  local parts = {}
  local nr_fmt = ToolView.line_nr_fmt(start + #lines - 1) .. ": %s"
  for i, line in ipairs(lines) do
    parts[#parts + 1] = string.format(nr_fmt, start + i - 1, line)
  end
  local llm_output = table.concat(parts, "\n")

  local trunc_start = start + #lines
  if trunc_start <= total_lines then
    llm_output = llm_output
      .. string.format(
        "\n\n...\n\nTruncated lines: %d-%d. Use offset=%d to read further.",
        trunc_start,
        total_lines,
        trunc_start
      )
  end

  local shown = #lines
  local annotation = shown < total_lines and string.format("%d of %d lines", shown, total_lines)
    or string.format("%d lines", shown)

  local basename = path:match("([^/]+)$")
  local parent = maki.fs.dirname(path)
  if parent and not ctx:is_instruction_file(basename) then
    ctx:load_instructions(parent)
  end

  return {
    llm_output = llm_output,
    body = build_file_view(lines, start, total_lines, path, ctx, window.context),
    annotation = annotation,
  }
end

maki.api.register_prompt_hint({
  slot = "tool_usage",
  content = [[
- When using the **read** tool on text files, only read the sections you actually need.
- Use `wc -l` to check total number of lines before reading to decide a reasonable **read** tool limit.]],
})

maki.api.register_tool({
  name = "read",
  permission = "fs_read",
  kind = "read",
  description = DESCRIPTION,

  schema = {
    type = "object",
    properties = {
      path = {
        type = "string",
        description = "Path to the file to read (text or image)",
        required = true,
        alias = "file_path",
      },
      offset = {
        type = "integer",
        description = "Line number to start from (1-indexed). Defaults to 1. Ignored for images.",
      },
      limit = {
        type = "integer",
        description = "Max number of lines to read. Use 0 or omit to read until end of file (capped at 2000 lines). Ignored for images.",
      },
    },
  },

  header = function(input)
    local buf = maki.ui.buf()
    local s = shorten_path(input.path or "")
    if input.offset then
      local start = input.offset
      if input.limit and input.limit > 0 then
        s = s .. ":" .. start .. "-" .. (start + input.limit - 1)
      else
        s = s .. ":" .. start
      end
    end
    buf:line({ { s, "path" } })
    return buf
  end,

  restore = function(input, output, _is_error, ctx)
    if output:match("^%[image: ") then
      return {}
    end
    local lines, start_line, total_lines = {}, nil, nil
    for _, raw in ipairs(maki.split(output, "\n")) do
      local nr, text = raw:match("^%s*(%d+): (.*)$")
      if nr then
        start_line = start_line or tonumber(nr)
        lines[#lines + 1] = text
      else
        local trunc_end = raw:match("Truncated lines: %d+%-(%d+)")
        if trunc_end then
          total_lines = tonumber(trunc_end)
        end
      end
    end
    if #lines == 0 then
      return ToolView.restore(output, read_view_opts(ctx))
    end
    start_line = start_line or 1
    total_lines = total_lines or (start_line + #lines - 1)
    local path = input.path or ""
    local above = start_line > 1 and read_window(path, start_line, 0, false, ctx)
    return build_file_view(lines, start_line, total_lines, path, ctx, above and above.context)
  end,

  handler = function(input, ctx)
    local raw = input.path
    if not raw then
      return { llm_output = "error: path is required", is_error = true }
    end
    local path = maki.fs.abspath(raw)
    local meta = maki.fs.metadata(path)
    if not meta then
      return { llm_output = "error: path not found: " .. path, is_error = true }
    end
    if meta.is_dir then
      return { llm_output = "error: path is a directory, use the list tool instead", is_error = true }
    end

    local ext = path:match("%.([^%.]+)$")
    local lower_ext = ext and ext:lower()
    if lower_ext and IMAGE_EXTS[lower_ext] then
      return load_image(path, ctx)
    end

    local offset = input.offset or 1
    local limit = input.limit or 0
    local res = read_file(path, offset, limit, ctx)
    if res.is_error and (res.llm_output:find("non%-utf8") or res.llm_output:find("UTF%-8")) then
      local bytes = maki.fs.read_bytes(path, { len = 1024 })
      if bytes then
        local info = maki.image.probe(bytes)
        if info and MEDIA_TYPES[info.format] then
          return load_image(path, ctx)
        end
      end
      return { llm_output = "read error: " .. path .. " is a binary file and not a supported image format", is_error = true }
    end
    return res
  end,
})
