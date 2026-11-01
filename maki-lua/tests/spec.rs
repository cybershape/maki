use std::sync::Arc;

use maki_agent::tools::ToolRegistry;
use maki_config::CompatibilityConfig;
use maki_lua::PluginHost;
use test_case::test_case;

#[test_case(true, true, true ; "all_sources")]
#[test_case(true, false, true ; "claude_disabled")]
#[test_case(true, true, false ; "opencode_disabled")]
#[test_case(false, true, true ; "all_disabled_overrides_individuals")]
fn skill_discovery_respects_compatibility(all: bool, claude: bool, opencode: bool) {
    let host = PluginHost::new(Arc::new(ToolRegistry::new())).unwrap();
    host.set_compatibility(CompatibilityConfig {
        all: Some(all),
        claude: Some(claude),
        opencode: Some(opencode),
        ..Default::default()
    })
    .unwrap();
    let source = include_str!("../../plugins/skill/init.lua");
    let harness = format!(
        r#"
local registered
local scanned = {{}}
local reads = {{}}
local original_require = require
local helpers = original_require("skill_helpers")
local roots = {{
  "/config/skills", "/home/.claude/skills", "/home/.config/opencode/skills", "/home/.agents/skills",
  "/repo/child/.maki/skills", "/repo/child/.claude/skills", "/repo/child/.opencode/skills", "/repo/child/.agents/skills",
  "/repo/.maki/skills", "/repo/.claude/skills", "/repo/.opencode/skills", "/repo/.agents/skills",
}}
local function skill_name(path)
  return (path:gsub("/", "_"))
end
local function require(name)
  if name == "maki.shorten_path" then return function(path) return path end end
  if name == "maki.tool_view" then return {{}} end
  if name == "skill_helpers" then return helpers end
  if name == "plugin_dev" then return {{}} end
  if name == "plugin_dev_reference" then return {{}} end
  error(name)
end
local api = maki.api
local maki = {{
  api = {{
    agent_enabled = api.agent_enabled,
    register_options = function() return {{ plugin_dev = false }} end,
    register_tool = function(tool) registered = tool end,
  }},
  env = {{ config_dir = function() return "/config" end }},
  uv = {{ cwd = function() return "/repo/child" end, os_homedir = function() return "/home" end }},
  fs = {{
    joinpath = function(...) return table.concat({{...}}, "/") end,
    metadata = function(path) return path == "/repo/.git" end,
    parents = function() return {{ "/repo", "/" }} end,
    dir = function(path)
      scanned[path] = (scanned[path] or 0) + 1
      return {{ {{ skill_name(path), "directory" }} }}
    end,
    read = function(path)
      reads[path] = true
      return "body"
    end,
  }},
}}
local function load_plugin()
{source}
end
load_plugin()
assert(registered and registered.name == "skill")
local missing = registered.handler({{ name = "missing" }}, {{}})
assert(missing.is_error)
for _, root in ipairs(roots) do
  local expected = true
  if root:find(".claude/", 1, true) then expected = {claude_enabled} end
  if root:find("opencode/", 1, true) then expected = {opencode_enabled} end
  assert((scanned[root] ~= nil) == expected, root)
  assert((reads[root .. "/" .. skill_name(root) .. "/SKILL.md"] ~= nil) == expected, root)
  assert((registered.description:find(skill_name(root), 1, true) ~= nil) == expected, root)
  assert((missing.llm_output:find(skill_name(root), 1, true) ~= nil) == expected, root)
  if expected then assert(scanned[root] == 2, root) end
end
"#,
        claude_enabled = all && claude,
        opencode_enabled = all && opencode
    );
    host.load_source("skill_spec", &harness).unwrap();
}

#[test_case(true ; "enabled_host")]
#[test_case(false ; "disabled_host")]
fn compatibility_policy_is_local_to_host(enabled: bool) {
    let disabled = PluginHost::new(Arc::new(ToolRegistry::new())).unwrap();
    disabled
        .set_compatibility(CompatibilityConfig {
            all: Some(false),
            ..Default::default()
        })
        .unwrap();
    let host = if enabled {
        PluginHost::new(Arc::new(ToolRegistry::new())).unwrap()
    } else {
        disabled
    };
    host.load_source(
        "policy_spec",
        &format!("assert(maki.api.agent_enabled('claude') == {enabled})"),
    )
    .unwrap();
    assert!(
        host.load_source("invalid_policy_spec", "maki.api.agent_enabled('unknown')")
            .is_err()
    );
}

#[test_case("completion", include_str!("../../plugins/completion/tests/spec.lua") ; "completion_plugin_spec")]
#[test_case("edit", include_str!("../../plugins/edit/tests/spec.lua") ; "edit_plugin_spec")]
#[test_case("index", include_str!("../../plugins/index/tests/spec.lua") ; "index_plugin_spec")]
#[test_case("lib", include_str!("../../plugins/lib/tests/spec.lua") ; "lib_spec")]
#[test_case("list_dir", include_str!("../../plugins/list_dir/tests/spec.lua") ; "list_dir_plugin_spec")]
#[test_case("memory", include_str!("../../plugins/memory/tests/spec.lua") ; "memory_plugin_spec")]
#[test_case("question", include_str!("../../plugins/question/tests/spec.lua") ; "question_plugin_spec")]
#[test_case("skill", include_str!("../../plugins/skill/tests/spec.lua") ; "skill_plugin_spec")]
#[test_case("task", include_str!("../../plugins/task/tests/spec.lua") ; "task_plugin_spec")]
#[test_case("thinking", include_str!("../../plugins/thinking/tests/spec.lua") ; "thinking_plugin_spec")]
#[test_case("view_image", include_str!("../../plugins/view_image/tests/spec.lua") ; "view_image_plugin_spec")]
#[test_case("webfetch", include_str!("../../plugins/webfetch/tests/spec.lua") ; "webfetch_plugin_spec")]
#[test_case("websearch", include_str!("../../plugins/websearch/tests/spec.lua") ; "websearch_plugin_spec")]
#[test_case("write", include_str!("../../plugins/write/tests/spec.lua") ; "write_plugin_spec")]
fn plugin_spec(name: &str, spec: &str) {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source(&format!("{name}_spec"), spec)
        .unwrap_or_else(|e| panic!("{name} spec failed:\n{e}"));
}
