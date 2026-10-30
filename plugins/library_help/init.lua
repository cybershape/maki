local ToolView = require("maki.tool_view")

local PY_TYPES = {
  string = "str",
  integer = "int",
  number = "float",
  boolean = "bool",
  object = "dict",
  array = "list",
}

local function py_type(prop)
  if not prop or type(prop) ~= "table" then
    return "any"
  end
  local t = prop.type
  if t == "array" then
    if prop.items and type(prop.items) == "table" and prop.items.type then
      local inner = PY_TYPES[prop.items.type] or "any"
      return "list[" .. inner .. "]"
    end
    return "list"
  end
  return PY_TYPES[t] or (t or "any")
end

local function format_python_docstring(name, desc, schema)
  schema = schema or {}
  local props = schema.properties or {}
  local req_set = {}
  if schema.required and type(schema.required) == "table" then
    for _, k in ipairs(schema.required) do
      req_set[k] = true
    end
  end

  local req_args = {}
  local opt_args = {}
  for k, v in pairs(props) do
    if req_set[k] then
      table.insert(req_args, { name = k, prop = v })
    else
      table.insert(opt_args, { name = k, prop = v })
    end
  end

  table.sort(req_args, function(a, b) return a.name < b.name end)
  table.sort(opt_args, function(a, b) return a.name < b.name end)

  local sig_lines = {}
  for _, a in ipairs(req_args) do
    table.insert(sig_lines, string.format("    %s: %s,", a.name, py_type(a.prop)))
  end
  for _, a in ipairs(opt_args) do
    local def_val = "None"
    if a.prop and a.prop.default ~= nil then
      if type(a.prop.default) == "string" then
        def_val = string.format("%q", a.prop.default)
      elseif type(a.prop.default) == "boolean" then
        def_val = a.prop.default and "True" or "False"
      else
        def_val = tostring(a.prop.default)
      end
    end
    table.insert(sig_lines, string.format("    %s: %s = %s,", a.name, py_type(a.prop), def_val))
  end

  local sig_args_str = ""
  if #sig_lines > 0 then
    sig_args_str = "\n" .. table.concat(sig_lines, "\n") .. "\n"
  end

  local doc_lines = {}
  table.insert(doc_lines, string.format("async def %s(%s) -> str:", name, sig_args_str))
  table.insert(doc_lines, '    """')
  if desc and #desc > 0 then
    table.insert(doc_lines, "    " .. desc:gsub("\n", "\n    "))
    table.insert(doc_lines, "")
  end

  if #req_args > 0 or #opt_args > 0 then
    table.insert(doc_lines, "    Parameters:")
    for _, a in ipairs(req_args) do
      local pdesc = (a.prop and a.prop.description) or ""
      if #pdesc > 0 then
        table.insert(doc_lines, string.format("    - %s (%s, required): %s", a.name, py_type(a.prop), pdesc))
      else
        table.insert(doc_lines, string.format("    - %s (%s, required)", a.name, py_type(a.prop)))
      end
    end
    for _, a in ipairs(opt_args) do
      local pdesc = (a.prop and a.prop.description) or ""
      if #pdesc > 0 then
        table.insert(doc_lines, string.format("    - %s (%s, optional): %s", a.name, py_type(a.prop), pdesc))
      else
        table.insert(doc_lines, string.format("    - %s (%s, optional)", a.name, py_type(a.prop)))
      end
    end
  end

  table.insert(doc_lines, '    """')
  return table.concat(doc_lines, "\n")
end

maki.api.register_tool({
  name = "library_help",
  kind = "inspect",
  description = "Retrieve the Python function signature, parameter types, and docstring documentation for an external library function before calling it in code_execution.",

  schema = {
    type = "object",
    properties = {
      library_name = {
        type = "string",
        description = "The external library name (e.g. \"github\").",
        required = true,
      },
      function_name = {
        type = "string",
        description = "The function name to inspect (e.g. \"create_issue\").",
        required = true,
      },
    },
  },

  header = function(input)
    local buf = maki.ui.buf()
    local lib = input.library_name or ""
    local fn = input.function_name or ""
    local spans = {
      { "help: ", "dim" },
      { lib .. "." .. fn, "tool" },
    }
    buf:line(spans)
    return buf
  end,

  restore = function(_input, output, _is_error, _ctx)
    return ToolView.restore(output, { max_lines = 30 })
  end,

  handler = function(input, ctx)
    local library_name = input.library_name
    local function_name = input.function_name

    if not library_name or #library_name == 0 or not function_name or #function_name == 0 then
      return {
        llm_output = "error: both 'library_name' and 'function_name' are required parameters.",
        is_error = true,
      }
    end

    local libs, err = maki.agent.mcp_libraries(ctx)
    if err then
      return { llm_output = "error fetching external libraries: " .. err, is_error = true }
    end

    libs = libs or {}
    local matched_lib = nil
    local available_libs = {}

    for _, lib in ipairs(libs) do
      table.insert(available_libs, lib.name)
      if lib.name == library_name then
        matched_lib = lib
        break
      end
    end

    if not matched_lib then
      if #available_libs == 0 then
        return {
          llm_output = string.format("Library '%s' not found. No external libraries are currently available.", library_name),
          is_error = true,
        }
      end
      table.sort(available_libs)
      return {
        llm_output = string.format(
          "Library '%s' not found.\n\nAvailable libraries:\n- %s",
          library_name,
          table.concat(available_libs, "\n- ")
        ),
        is_error = true,
      }
    end

    local matched_fn = nil
    local available_funcs = {}
    for _, fn in ipairs(matched_lib.functions or {}) do
      table.insert(available_funcs, fn.name)
      if fn.name == function_name then
        matched_fn = fn
        break
      end
    end

    if not matched_fn then
      table.sort(available_funcs)
      local funcs_msg = #available_funcs > 0 and table.concat(available_funcs, "\n- ") or "(none)"
      return {
        llm_output = string.format(
          "Function '%s' not found in library '%s'.\n\nAvailable functions in '%s':\n- %s",
          function_name,
          library_name,
          library_name,
          funcs_msg
        ),
        is_error = true,
      }
    end

    local doc = format_python_docstring(matched_fn.name, matched_fn.description, matched_fn.schema)
    return { llm_output = doc }
  end,
})
