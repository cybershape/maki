use std::sync::Arc;

use maki_agent::tools::test_support::stub_ctx;
use maki_agent::tools::{ToolContext, ToolRegistry};
use maki_agent::{AgentMode, ToolOutput};
use maki_lua::PluginHost;
use maki_lua::test_support::runtime;
use serde_json::json;

const POWERSHELL_PLUGIN_SRC: &str = include_str!("../../plugins/powershell/init.lua");

fn setup() -> (Arc<ToolRegistry>, PluginHost) {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source("powershell", POWERSHELL_PLUGIN_SRC)
        .expect("powershell plugin should load");
    (reg, host)
}

fn exec_powershell(
    reg: &Arc<ToolRegistry>,
    input: serde_json::Value,
    ctx_modifier: impl FnOnce(&mut ToolContext),
) -> Result<String, String> {
    let mut ctx = stub_ctx(&AgentMode::Build);
    ctx.registry = Arc::clone(reg);
    ctx_modifier(&mut ctx);
    let entry = reg.get("powershell").expect("powershell tool registered");
    let inv = entry.tool.parse(&input).expect("parse failed");
    runtime()
        .block_on(inv.execute(&ctx))
        .output
        .map(|out| match out {
            ToolOutput::Plain(s) => s.text,
            other => panic!("unexpected output: {other:?}"),
        })
}

#[test]
fn powershell_tool_registered_with_head_param() {
    let (reg, _host) = setup();
    let entry = reg.get("powershell").expect("powershell tool registered");
    assert_eq!(entry.tool.name(), "powershell");

    let schema = entry.tool.schema();
    let props = schema.get("properties").expect("properties map");
    assert!(
        props.get("head").is_some(),
        "head parameter must be present in schema"
    );
    assert!(
        props.get("tail").is_some(),
        "tail parameter must be present in schema"
    );
    assert!(
        props.get("command").is_some(),
        "command parameter must be present in schema"
    );
}

#[test]
fn powershell_validates_required_command() {
    let (reg, _host) = setup();
    let err = exec_powershell(&reg, json!({"command": "   "}), |_| {})
        .expect_err("blank command should fail");
    assert_eq!(err, "error: command is required");
}

#[test]
fn powershell_validates_head_and_tail_limits() {
    let (reg, _host) = setup();
    let head_err = exec_powershell(&reg, json!({"command": "Get-Process", "head": 0}), |_| {})
        .expect_err("head=0 should fail");
    assert_eq!(head_err, "error: head must be >= 1");

    let tail_err = exec_powershell(&reg, json!({"command": "Get-Process", "tail": -1}), |_| {})
        .expect_err("tail=-1 should fail");
    assert_eq!(tail_err, "error: tail must be >= 1");
}

#[test]
fn powershell_options_include_output_limits() {
    let (reg, _host) = setup();
    let entry = reg.get("powershell").expect("powershell tool registered");
    let schema = entry.tool.schema();
    assert!(schema["properties"].get("head").is_some());
    assert!(schema["properties"].get("tail").is_some());
}
