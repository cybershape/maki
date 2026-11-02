return function(U)
  local get_text = U.get_text
  local line_start = U.line_start
  local line_end = U.line_end
  local format_range = U.format_range
  local find_child = U.find_child
  local compact_ws = U.compact_ws
  local truncate = U.truncate
  local new_entry = U.new_entry
  local SECTION = U.SECTION
  local format_skeleton = U.format_skeleton

  local STRUCTURAL_TAGS = {
    div = true,
    form = true,
    section = true,
    article = true,
    aside = true,
    header = true,
    footer = true,
    nav = true,
    main = true,
    table = true,
    thead = true,
    tbody = true,
    tfoot = true,
    tr = true,
    ul = true,
    ol = true,
    dl = true,
    details = true,
    dialog = true,
    fieldset = true,
    button = true,
    input = true,
    select = true,
    textarea = true,
  }

  local VUE_BUILTIN_COMPONENTS = {
    slot = true,
    component = true,
    transition = true,
    ["transition-group"] = true,
    ["keep-alive"] = true,
    teleport = true,
    suspense = true,
    ["router-view"] = true,
    ["router-link"] = true,
    ["RouterView"] = true,
    ["RouterLink"] = true,
  }

  local VUE_MACROS = {
    defineProps = true,
    defineEmits = true,
    defineModel = true,
    defineOptions = true,
    defineSlots = true,
    defineExpose = true,
    withDefaults = true,
  }

  local MAX_TEMPLATE_DEPTH = 5

  local function attr_value(attr_node)
    local quoted = find_child(attr_node, "quoted_attribute_value")
    if quoted then
      return find_child(quoted, "attribute_value")
    end
    return find_child(attr_node, "attribute_value")
  end

  local function is_component_tag(tag_name)
    if VUE_BUILTIN_COMPONENTS[tag_name] then
      return true
    end
    -- PascalCase custom component: e.g. <UserAvatar>, <MyButton>
    if tag_name:match("^[A-Z]") then
      return true
    end
    -- kebab-case custom component: e.g. <user-avatar>, <my-button>
    if tag_name:find("-", 1, true) then
      return true
    end
    return false
  end

  local function extract_tag_info(tag_node, source)
    local name_node = find_child(tag_node, "tag_name")
    if not name_node then
      return nil
    end
    local tag_name = get_text(name_node, source)
    local id, classes = nil, {}
    local has_ref = false

    for _, child in ipairs(tag_node:children()) do
      local ctype = child:type()
      if ctype == "attribute" then
        local aname_node = find_child(child, "attribute_name")
        if aname_node then
          local aname = get_text(aname_node, source)
          if aname == "id" then
            local val_node = attr_value(child)
            if val_node then
              id = get_text(val_node, source)
            end
          elseif aname == "class" then
            local val_node = attr_value(child)
            if val_node then
              for cls in get_text(val_node, source):gmatch("%S+") do
                classes[#classes + 1] = cls
              end
            end
          elseif aname == "ref" then
            has_ref = true
          end
        end
      elseif ctype == "directive_attribute" then
        local dname_node = find_child(child, "directive_name")
        if dname_node then
          local dname = get_text(dname_node, source)
          if dname == "ref" or dname == ":ref" then
            has_ref = true
          end
        end
      end
    end

    return tag_name, id, classes, has_ref
  end

  local function format_tag(tag_name, id, classes)
    local s = "<" .. tag_name
    if id then
      s = s .. "#" .. id
    end
    for _, cls in ipairs(classes) do
      s = s .. "." .. cls
    end
    return s .. ">"
  end

  local function walk_template(node, source, depth, out, is_root_child)
    if depth > MAX_TEMPLATE_DEPTH then
      return
    end
    local kind = node:type()
    if kind == "element" then
      local self_closing = find_child(node, "self_closing_tag")
      local tag_node = find_child(node, "start_tag") or self_closing
      if not tag_node then
        return
      end
      local tag_name, id, classes, has_ref = extract_tag_info(tag_node, source)
      if not tag_name then
        return
      end

      local should_include = is_root_child
        or STRUCTURAL_TAGS[tag_name]
        or is_component_tag(tag_name)
        or id ~= nil
        or has_ref

      if should_include then
        local indent = string.rep("  ", depth + 1)
        local lr = format_range(line_start(node), line_end(node))
        local formatted = format_tag(tag_name, id, classes)
        out[#out + 1] = {
          line = indent .. formatted .. " " .. lr,
          body = indent .. formatted,
          range = lr,
        }
        if not self_closing then
          for _, child in ipairs(node:children()) do
            walk_template(child, source, depth + 1, out, false)
          end
        end
      else
        if not self_closing then
          for _, child in ipairs(node:children()) do
            walk_template(child, source, depth, out, false)
          end
        end
      end
    end
  end

  local function mask_outside_ranges(source, ranges)
    local parts = {}
    local last = 1
    for _, r in ipairs(ranges) do
      if r.start > last then
        local outside = source:sub(last, r.start - 1)
        parts[#parts + 1] = outside:gsub("[^\n]", " ")
      end
      if r.stop >= r.start then
        parts[#parts + 1] = source:sub(r.start, r.stop)
      end
      last = r.stop + 1
    end
    if last <= #source then
      local outside = source:sub(last)
      parts[#parts + 1] = outside:gsub("[^\n]", " ")
    end
    return table.concat(parts)
  end

  local function extract_vue_macro(node, source)
    if node:type() ~= "expression_statement" then
      return nil
    end
    local call = find_child(node, "call_expression")
    if not call then
      return nil
    end
    local fn_node = call:field("function")[1]
    if not fn_node then
      return nil
    end
    local fn_name = get_text(fn_node, source)
    if not VUE_MACROS[fn_name] then
      return nil
    end
    local full_text = compact_ws(truncate(get_text(call, source):gsub(";%s*$", ""), 80))
    local sec = (fn_name == "defineProps" or fn_name == "defineEmits" or fn_name == "defineSlots") and SECTION.Type
      or SECTION.Constant
    return new_entry(sec, node, full_text)
  end

  local function extract_options_api(node, source)
    if node:type() ~= "export_statement" then
      return nil
    end
    local obj_node = nil
    for _, child in ipairs(node:children()) do
      if child:type() == "object" then
        obj_node = child
        break
      elseif child:type() == "call_expression" then
        local fn_node = child:field("function")[1]
        if fn_node and get_text(fn_node, source) == "defineComponent" then
          local args = child:field("arguments")[1]
          if args then
            obj_node = find_child(args, "object")
          end
        end
        if obj_node then
          break
        end
      end
    end

    if not obj_node then
      return nil
    end

    local entries = {}
    local component_name = nil
    local methods = {}

    for _, prop in ipairs(obj_node:children()) do
      local ptype = prop:type()
      if ptype == "pair" or ptype == "method_definition" then
        local key_node = prop:field("key")[1] or prop:field("name")[1]
        local key = key_node and get_text(key_node, source) or ""
        if key == "name" then
          local val_node = prop:field("value")[1]
          if val_node then
            component_name = get_text(val_node, source):gsub("^['\"]", ""):gsub("['\"]$", "")
          end
        elseif key == "methods" then
          local val_node = prop:field("value")[1]
          if val_node and val_node:type() == "object" then
            for _, m in ipairs(val_node:children()) do
              if m:type() == "method_definition" or m:type() == "pair" then
                local mname_node = m:field("name")[1] or m:field("key")[1]
                if mname_node then
                  local mname = get_text(mname_node, source)
                  local params_node = m:field("parameters")[1]
                  local params = params_node and get_text(params_node, source) or "()"
                  entries[#entries + 1] = new_entry(SECTION.Function, m, mname .. params)
                end
              end
            end
          end
        elseif key == "computed" then
          local val_node = prop:field("value")[1]
          if val_node and val_node:type() == "object" then
            for _, c in ipairs(val_node:children()) do
              if c:type() == "method_definition" or c:type() == "pair" then
                local cname_node = c:field("name")[1] or c:field("key")[1]
                if cname_node then
                  local cname = get_text(cname_node, source)
                  entries[#entries + 1] = new_entry(SECTION.Function, c, cname .. "() [computed]")
                end
              end
            end
          end
        elseif ptype == "method_definition" then
          local params_node = prop:field("parameters")[1]
          local params = params_node and get_text(params_node, source) or "()"
          entries[#entries + 1] = new_entry(SECTION.Function, prop, key .. params)
        end
      end
    end

    local label = component_name and ("export default defineComponent (" .. component_name .. ")")
      or "export default defineComponent"
    local main_entry = new_entry(SECTION.Class, node, label)
    entries[#entries + 1] = main_entry
    return entries
  end

  local function extract_script_entries(script_nodes, source)
    if #script_nodes == 0 then
      return {}
    end

    local ranges = {}
    for _, sc in ipairs(script_nodes) do
      local start_tag = find_child(sc, "start_tag")
      local end_tag = find_child(sc, "end_tag")
      if start_tag then
        local _, _, sb = start_tag:end_()
        local eb = nil
        if end_tag then
          local _, _, s = end_tag:start()
          eb = s
        else
          local _, _, s = sc:end_()
          eb = s
        end
        if eb and eb > sb then
          ranges[#ranges + 1] = { start = sb + 1, stop = eb }
        end
      end
    end

    if #ranges == 0 then
      return {}
    end

    local masked = mask_outside_ranges(source, ranges)
    local parser = maki.treesitter.get_parser(masked, "tsx")
    if not parser then
      return {}
    end
    local root = parser:parse()[1]:root()
    local ts_extractor = require("lang.typescript")(U)

    local entries = {}
    for _, child in ipairs(root:children()) do
      local macro_entry = extract_vue_macro(child, masked)
      if macro_entry then
        entries[#entries + 1] = macro_entry
      else
        local opts_entries = extract_options_api(child, masked)
        if opts_entries and #opts_entries > 0 then
          for _, e in ipairs(opts_entries) do
            entries[#entries + 1] = e
          end
        else
          local extracted = ts_extractor.extract_nodes(child, masked, {})
          for _, e in ipairs(extracted or {}) do
            entries[#entries + 1] = e
          end
        end
      end
    end

    return entries
  end

  return {
    extract = function(source, root)
      local out = {}
      local meta = {}

      local block_items = {}
      local template_nodes = {}
      local script_nodes = {}

      for _, child in ipairs(root:children()) do
        local kind = child:type()
        if kind == "template_element" then
          template_nodes[#template_nodes + 1] = child
          local start_tag = find_child(child, "start_tag")
          local tag_text = start_tag and compact_ws(get_text(start_tag, source)) or "<template>"
          local lr = format_range(line_start(child), line_end(child))
          block_items[#block_items + 1] = {
            line_start = line_start(child),
            line_end = line_end(child),
            text = tag_text,
            range = lr,
          }
        elseif kind == "script_element" then
          script_nodes[#script_nodes + 1] = child
          local start_tag = find_child(child, "start_tag")
          local tag_text = start_tag and compact_ws(get_text(start_tag, source)) or "<script>"
          local lr = format_range(line_start(child), line_end(child))
          block_items[#block_items + 1] = {
            line_start = line_start(child),
            line_end = line_end(child),
            text = tag_text,
            range = lr,
          }
        elseif kind == "style_element" then
          local start_tag = find_child(child, "start_tag")
          local tag_text = start_tag and compact_ws(get_text(start_tag, source)) or "<style>"
          local lr = format_range(line_start(child), line_end(child))
          block_items[#block_items + 1] = {
            line_start = line_start(child),
            line_end = line_end(child),
            text = tag_text,
            range = lr,
          }
        end
      end

      -- 1. blocks: section
      if #block_items > 0 then
        local lo, hi = block_items[1].line_start, block_items[#block_items].line_end
        local brange = format_range(lo, hi)
        out[#out + 1] = "blocks: " .. brange
        meta[#out] = { tag = "section", body = "blocks: ", range = brange }
        for _, b in ipairs(block_items) do
          local line = "  " .. b.text .. " " .. b.range
          out[#out + 1] = line
          meta[#out] = { body = "  " .. b.text, range = b.range }
        end
      end

      -- 2. template: section
      local template_items = {}
      for _, tnode in ipairs(template_nodes) do
        for _, child in ipairs(tnode:children()) do
          walk_template(child, source, 0, template_items, true)
        end
      end
      if #template_items > 0 then
        if #out > 0 then
          out[#out + 1] = ""
        end
        out[#out + 1] = "template:"
        meta[#out] = { tag = "section" }
        for _, item in ipairs(template_items) do
          out[#out + 1] = item.line
          meta[#out] = { body = item.body, range = item.range }
        end
      end

      -- 3. script extracts (imports, types, consts, fns, classes)
      local script_entries = extract_script_entries(script_nodes, source)
      if #script_entries > 0 then
        local skel, skel_meta = format_skeleton(script_entries, {}, nil, "/")
        if skel and #skel > 0 then
          if #out > 0 then
            out[#out + 1] = ""
          end
          local skel_lines = maki.split(skel:gsub("\n+$", ""), "\n")
          for i, line in ipairs(skel_lines) do
            out[#out + 1] = line
            meta[#out] = skel_meta and skel_meta[i]
          end
        end
      end

      if #out == 0 then
        return "", {}
      end
      return table.concat(out, "\n") .. "\n", meta
    end,
  }
end
