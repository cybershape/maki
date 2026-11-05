return function(U)
  local get_text = U.get_text
  local line_start = U.line_start
  local line_end = U.line_end
  local find_child = U.find_child
  local compact_ws = U.compact_ws
  local new_entry = U.new_entry
  local simple_import = U.simple_import
  local format_range = U.format_range
  local ranged = U.ranged
  local extract_fields_truncated = U.extract_fields_truncated
  local format_skeleton = U.format_skeleton
  local SECTION = U.SECTION
  local CHILD_BRIEF = U.CHILD_BRIEF
  local CHILD_DETAILED = U.CHILD_DETAILED

  local function is_doc_comment(node, source)
    local k = node:type()
    if k == "xml_doc" then
      return true
    end
    if k == "line_comment" then
      return get_text(node, source):sub(1, 3) == "///"
    end
    if k == "block_comment" then
      return get_text(node, source):sub(1, 3) == "(**"
    end
    return false
  end

  local function find_preceding_doc_start(node, source)
    local earliest = nil
    local prev = node:prev_sibling()
    while prev do
      if is_doc_comment(prev, source) then
        earliest = line_start(prev)
        prev = prev:prev_sibling()
      else
        break
      end
    end
    return earliest
  end

  local function is_test_node(node, source)
    local attrs_node = find_child(node, "attributes")
    if attrs_node then
      local t = get_text(attrs_node, source)
      if t:find("Test") or t:find("Fact") or t:find("Theory") or t:find("TestCase") then
        return true
      end
    end
    local prev = node:prev_sibling()
    while prev and prev:type() == "attributes" do
      local t = get_text(prev, source)
      if t:find("Test") or t:find("Fact") or t:find("Theory") or t:find("TestCase") then
        return true
      end
      prev = prev:prev_sibling()
    end
    return false
  end

  local function extract_member_signature(node, source)
    local parts = {}
    for _, child in ipairs(node:children()) do
      local k = child:type()
      if
        k == "static"
        or k == "member"
        or k == "abstract"
        or k == "override"
        or k == "default"
        or k == "inline"
        or k == "val"
      then
        parts[#parts + 1] = get_text(child, source)
      elseif k == "access_modifier" then
        parts[#parts + 1] = get_text(child, source)
      elseif k == "member_signature" then
        parts[#parts + 1] = get_text(child, source)
      elseif k == "method_or_prop_defn" then
        local t = get_text(child, source)
        local before = t:match("^(.-)%s*=") or t:match("^(.-)%s+with%s") or t
        parts[#parts + 1] = before
      elseif k == "property_or_ident" then
        parts[#parts + 1] = get_text(child, source)
      elseif k == "simple_type" or k == "postfix_type" or k == "function_type" then
        parts[#parts + 1] = ": " .. get_text(child, source)
      elseif k == "value_declaration" then
        local t = get_text(child, source)
        local before = t:match("^(.-)%s*=") or t
        parts[#parts + 1] = before
      end
    end
    if #parts == 0 then
      local t = get_text(node, source)
      local before = t:match("^(.-)%s*=") or t
      return compact_ws(before)
    end
    return compact_ws(table.concat(parts, " "))
  end

  local function collect_type_members(type_node, source)
    local members = {}
    local function scan(container)
      for _, child in ipairs(container:children()) do
        local ck = child:type()
        if ck == "member_defn" then
          local sig = extract_member_signature(child, source)
          if sig and sig ~= "" then
            local lr = format_range(line_start(child), line_end(child))
            members[#members + 1] = ranged(sig, lr)
          end
        elseif ck == "type_extension_elements" then
          scan(child)
        elseif ck == "interface_implementation" then
          for _, ic in ipairs(child:children()) do
            if ic:type() == "member_defn" then
              local sig = extract_member_signature(ic, source)
              if sig and sig ~= "" then
                local lr = format_range(line_start(ic), line_end(ic))
                members[#members + 1] = ranged(sig, lr)
              end
            end
          end
        end
      end
    end
    scan(type_node)
    return members
  end

  local function extract_enum(node, source, parent_node)
    local name_node = find_child(node, "type_name")
    local name = name_node and get_text(name_node, source) or "_"
    local entry = new_entry(SECTION.Type, parent_node or node, "type " .. name)
    local cases = {}
    local cases_node = find_child(node, "enum_type_cases")
    if cases_node then
      for _, child in ipairs(cases_node:children()) do
        if child:type() == "enum_type_case" then
          local id = find_child(child, "identifier")
          if id then
            cases[#cases + 1] = get_text(id, source)
          end
        end
      end
    end
    entry.children = cases
    entry.child_kind = CHILD_BRIEF
    return entry
  end

  local function extract_union(node, source, parent_node)
    local name_node = find_child(node, "type_name")
    local name = name_node and get_text(name_node, source) or "_"
    local entry = new_entry(SECTION.Type, parent_node or node, "type " .. name)
    local cases = {}
    local cases_node = find_child(node, "union_type_cases")
    if cases_node then
      for _, child in ipairs(cases_node:children()) do
        if child:type() == "union_type_case" then
          local id = find_child(child, "identifier")
          if id then
            cases[#cases + 1] = get_text(id, source)
          end
        end
      end
    end
    entry.children = cases
    entry.child_kind = CHILD_BRIEF
    return entry
  end

  local function extract_record(node, source, parent_node)
    local name_node = find_child(node, "type_name")
    local name = name_node and get_text(name_node, source) or "_"
    local entry = new_entry(SECTION.Type, parent_node or node, "type " .. name)
    local fields = {}
    local rfields = find_child(node, "record_fields")
    if rfields then
      fields = extract_fields_truncated(rfields, source, "record_field", function(f, src)
        return compact_ws(get_text(f, src))
      end)
    end
    local members = collect_type_members(node, source)
    if #members > 0 then
      local all = {}
      for _, f in ipairs(fields) do
        all[#all + 1] = f
      end
      for _, m in ipairs(members) do
        all[#all + 1] = m
      end
      entry.children = all
      entry.child_kind = CHILD_DETAILED
    else
      entry.children = fields
      entry.child_kind = CHILD_BRIEF
    end
    return entry
  end

  local function extract_anon_type(node, source, parent_node)
    local name_node = find_child(node, "type_name")
    local name = name_node and get_text(name_node, source) or "_"
    local ctor_node = find_child(node, "primary_constr_args")
    local ctor = ""
    if ctor_node then
      local ct = compact_ws(get_text(ctor_node, source))
      if ct:sub(1, 1) == "(" then
        ctor = ct
      else
        ctor = " " .. ct
      end
    end
    local inherit_node = find_child(node, "class_inherits_decl")
    local inherit = ""
    if inherit_node then
      local base = find_child(inherit_node, "simple_type")
      if base then
        inherit = " : " .. get_text(base, source)
      end
    end
    local members = collect_type_members(node, source)

    local is_interface = (ctor == "") and (#members > 0)
    if is_interface then
      for _, m in ipairs(members) do
        if not m.body:find("^abstract") then
          is_interface = false
          break
        end
      end
    end

    local section = is_interface and SECTION.Trait or SECTION.Class
    local entry = new_entry(section, parent_node or node, compact_ws("type " .. name .. ctor .. inherit))
    entry.children = members
    entry.child_kind = CHILD_DETAILED
    return entry
  end

  local function extract_type_abbrev(node, source, parent_node)
    local name_node = find_child(node, "type_name")
    local name = name_node and get_text(name_node, source) or "_"
    return new_entry(SECTION.Type, parent_node or node, "type " .. name)
  end

  local function extract_delegate(node, source, parent_node)
    local name_node = find_child(node, "type_name")
    local name = name_node and get_text(name_node, source) or "_"
    return new_entry(SECTION.Type, parent_node or node, "type " .. name)
  end

  local function extract_let(node, source)
    local defn = find_child(node, "function_or_value_defn")
    if not defn then
      return nil
    end

    local fn_left = find_child(defn, "function_declaration_left")
    local val_left = find_child(defn, "value_declaration_left")

    local text = get_text(defn, source)
    local sig = text:match("^(.-)%s*=") or text
    sig = compact_ws(sig)

    local is_fn = fn_left ~= nil
    if not is_fn and val_left then
      local val_text = get_text(val_left, source)
      if val_text:find("%(") or val_text:find("%s") then
        is_fn = true
      end
    end

    local section = is_fn and SECTION.Function or SECTION.Constant
    return new_entry(section, node, sig)
  end

  local function extract_val_defn(node, source)
    local text = compact_ws(get_text(node, source))
    local is_fn = text:find("%->") ~= nil
    local section = is_fn and SECTION.Function or SECTION.Constant
    return new_entry(section, node, text)
  end

  local function adjust_doc_start(entry, node, source)
    local doc_start = find_preceding_doc_start(node, source)
    if doc_start and doc_start < entry.line_start then
      entry.line_start = doc_start
    end
  end

  local function extract(source, root)
    local entries = {}
    local test_lines = {}

    local function process_node(node)
      local kind = node:type()
      if kind == "namespace" then
        local id_node = find_child(node, "long_identifier")
        local name = id_node and get_text(id_node, source) or "_"
        local e = new_entry(SECTION.Module, node, name)
        adjust_doc_start(e, node, source)
        entries[#entries + 1] = e
        for _, child in ipairs(node:children()) do
          local ck = child:type()
          if ck ~= "namespace" and ck ~= "long_identifier" then
            process_node(child)
          end
        end
      elseif kind == "named_module" then
        local id_node = find_child(node, "long_identifier")
        local name = id_node and get_text(id_node, source) or "_"
        local e = new_entry(SECTION.Module, node, name)
        adjust_doc_start(e, node, source)
        entries[#entries + 1] = e
        for _, child in ipairs(node:children()) do
          local ck = child:type()
          if ck ~= "module" and ck ~= "long_identifier" then
            process_node(child)
          end
        end
      elseif kind == "module_defn" then
        local id_node = find_child(node, "identifier")
        local access_node = find_child(node, "access_modifier")
        local name = id_node and get_text(id_node, source) or "_"
        if access_node then
          name = get_text(access_node, source) .. " " .. name
        end
        local e = new_entry(SECTION.Module, node, name)
        adjust_doc_start(e, node, source)
        entries[#entries + 1] = e
        for _, child in ipairs(node:children()) do
          local ck = child:type()
          if ck ~= "module" and ck ~= "identifier" and ck ~= "access_modifier" and ck ~= "=" then
            process_node(child)
          end
        end
      elseif kind == "import_decl" then
        entries[#entries + 1] = simple_import(node, source, { "open%s+type%s+", "open%s+global%s+", "open%s+" }, ".")
      elseif kind == "type_definition" then
        for _, child in ipairs(node:children()) do
          local ck = child:type()
          local e = nil
          if ck == "enum_type_defn" then
            e = extract_enum(child, source, node)
          elseif ck == "union_type_defn" then
            e = extract_union(child, source, node)
          elseif ck == "record_type_defn" then
            e = extract_record(child, source, node)
          elseif ck == "anon_type_defn" then
            e = extract_anon_type(child, source, node)
          elseif ck == "type_abbrev_defn" then
            e = extract_type_abbrev(child, source, node)
          elseif ck == "delegate_type_defn" then
            e = extract_delegate(child, source, node)
          end
          if e then
            adjust_doc_start(e, node, source)
            entries[#entries + 1] = e
          end
        end
      elseif kind == "declaration_expression" or kind == "function_or_value_defn" then
        if is_test_node(node, source) then
          test_lines[#test_lines + 1] = line_start(node)
        else
          local e = extract_let(node, source)
          if e then
            adjust_doc_start(e, node, source)
            entries[#entries + 1] = e
          end
        end
      elseif kind == "value_definition" then
        local e = extract_val_defn(node, source)
        if e then
          adjust_doc_start(e, node, source)
          entries[#entries + 1] = e
        end
      end
    end

    for _, child in ipairs(root:children()) do
      process_node(child)
    end

    return format_skeleton(entries, test_lines, nil, ".")
  end

  return {
    extract = extract,
  }
end
