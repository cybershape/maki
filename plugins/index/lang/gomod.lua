return function(U)
  local get_text = U.get_text
  local compact_ws = U.compact_ws
  local format_range = U.format_range
  local find_child = U.find_child

  local function node_range(node, source)
    local sr = node:start() + 1
    local text = get_text(node, source):gsub("%s+$", "")
    local _, nls = text:gsub("\n", "")
    return format_range(sr, sr + nls)
  end

  local function clean_text(node, source)
    local raw = get_text(node, source)
    return compact_ws(raw):match("^%s*(.-)%s*$")
  end

  local SPEC_KINDS = {
    require_directive_single = { "require_spec", "requires" },
    require_directive_multi = { "require_spec", "requires" },
    replace_directive_single = { "replace_spec", "replaces" },
    replace_directive_multi = { "replace_spec", "replaces" },
    exclude_directive_single = { "exclude_spec", "excludes" },
    exclude_directive_multi = { "exclude_spec", "excludes" },
    retract_directive_single = { "retract_spec", "retracts" },
    retract_directive_multi = { "retract_spec", "retracts" },
    tool_directive_single = { "tool", "tools" },
    tool_directive_multi = { "tool", "tools" },
    godebug_directive_single = { "godebug_spec", "godebugs" },
    godebug_directive_multi = { "godebug_spec", "godebugs" },
    ignore_directive_single = { "ignore_spec", "ignores" },
    ignore_directive_multi = { "ignore_spec", "ignores" },
  }

  local SECTIONS = {
    { "module", "modules" },
    { "go", "gos" },
    { "toolchain", "toolchains" },
    { "require", "requires" },
    { "replace", "replaces" },
    { "exclude", "excludes" },
    { "retract", "retracts" },
    { "tool", "tools" },
    { "godebug", "godebugs" },
    { "ignore", "ignores" },
  }

  local function render_section(header, items, lines, meta)
    if #items == 0 then
      return
    end
    if #lines > 0 then
      lines[#lines + 1] = ""
    end
    lines[#lines + 1] = header .. ":"
    meta[#lines] = { tag = "section" }
    for _, item in ipairs(items) do
      local body = "  " .. item.text
      lines[#lines + 1] = body .. " " .. item.range
      meta[#lines] = { body = body, range = item.range }
    end
  end

  return {
    extract = function(source, root)
      local state = {
        modules = {},
        gos = {},
        toolchains = {},
        requires = {},
        replaces = {},
        excludes = {},
        retracts = {},
        tools = {},
        godebugs = {},
        ignores = {},
      }

      for _, child in ipairs(root:children()) do
        local kind = child:type()
        if kind == "module_directive" then
          local path_node = find_child(child, "module_path")
          if path_node then
            local text = clean_text(path_node, source):gsub('^"(.-)"$', "%1"):gsub("^'(.-)'$", "%1")
            state.modules[#state.modules + 1] = {
              text = text,
              range = node_range(child, source),
            }
          end
        elseif kind == "go_directive" then
          local ver_node = find_child(child, "go_version")
          if ver_node then
            state.gos[#state.gos + 1] = {
              text = clean_text(ver_node, source),
              range = node_range(child, source),
            }
          end
        elseif kind == "toolchain_directive" then
          local name_node = find_child(child, "toolchain_name")
          if name_node then
            state.toolchains[#state.toolchains + 1] = {
              text = clean_text(name_node, source),
              range = node_range(child, source),
            }
          end
        else
          local spec_def = SPEC_KINDS[kind]
          if spec_def then
            local item_kind, key = spec_def[1], spec_def[2]
            for _, subchild in ipairs(child:children()) do
              if subchild:type() == item_kind then
                state[key][#state[key] + 1] = {
                  text = clean_text(subchild, source),
                  range = node_range(subchild, source),
                }
              end
            end
          end
        end
      end

      local lines = {}
      local meta = {}
      for _, sec in ipairs(SECTIONS) do
        render_section(sec[1], state[sec[2]], lines, meta)
      end

      if #lines == 0 then
        return "", {}
      end
      return table.concat(lines, "\n") .. "\n", meta
    end,
  }
end
