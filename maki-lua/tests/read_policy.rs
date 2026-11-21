use std::fs;
use std::sync::Arc;

use maki_agent::tools::test_support::stub_ctx;
use maki_agent::tools::{ToolContext, ToolOutputMode, ToolRegistry};
use maki_agent::{AgentMode, ToolOutput};
use maki_lua::PluginHost;
use maki_lua::test_support::runtime;
use serde_json::json;
use tempfile::tempdir;

const READ_PLUGIN_SRC: &str = include_str!("../../plugins/read/init.lua");

fn setup(index_handler: &str) -> (Arc<ToolRegistry>, PluginHost) {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source("read", READ_PLUGIN_SRC)
        .expect("read plugin should load");
    if !index_handler.is_empty() {
        host.load_source(
            "index_fixture",
            &format!(
                r#"
maki.api.register_tool({{
    name = "index",
    description = "index fixture",
    schema = {{ type = "object", properties = {{ path = {{ type = "string" }} }} }},
    handler = {index_handler},
}})
"#
            ),
        )
        .expect("index fixture should load");
    }
    (reg, host)
}

fn exec_read(
    reg: &Arc<ToolRegistry>,
    path: &std::path::Path,
    offset: i64,
    limit: i64,
    ctx_modifier: impl FnOnce(&mut ToolContext),
) -> Result<String, String> {
    let mut ctx = stub_ctx(&AgentMode::Build);
    ctx.registry = Arc::clone(reg);
    ctx_modifier(&mut ctx);
    let entry = reg.get("read").expect("read tool registered");
    let input = json!({
        "path": path.to_str().unwrap(),
        "offset": offset,
        "limit": limit,
    });
    let inv = entry.tool.parse(&input).expect("parse failed");
    runtime()
        .block_on(inv.execute(&ctx))
        .output
        .map(|out| match out {
            ToolOutput::Plain(s) => s.text,
            other => panic!("unexpected output: {other:?}"),
        })
}

fn create_lines_file(path: &std::path::Path, count: usize) {
    let mut content = String::new();
    for i in 1..=count {
        content.push_str(&format!("line {i}\n"));
    }
    fs::write(path, content).unwrap();
}

#[test]
fn small_file_reads_completely_without_elision() {
    let (reg, _host) = setup("");
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("small.rs");
    create_lines_file(&file_path, 50);

    let out = exec_read(&reg, &file_path, 1, 0, |_| {}).unwrap();
    assert!(!out.contains("Note:"));
    assert!(out.contains("1: line 1"));
    assert!(out.contains("50: line 50"));
    assert!(!out.contains("Truncated lines:"));
}

#[test]
fn large_file_with_targeted_slice_reads_without_elision() {
    let (reg, _host) = setup("");
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("large.rs");
    create_lines_file(&file_path, 500);

    let out = exec_read(&reg, &file_path, 1, 50, |_| {}).unwrap();
    assert!(!out.contains("Note:"));
    assert!(out.contains("1: line 1"));
    assert!(out.contains("50: line 50"));
    assert!(!out.contains("51: line 51"));
    assert!(out.contains("Truncated lines: 51-500. Use offset=51 to read further."));
}

#[test]
fn large_file_unbounded_with_indexable_type_returns_index_skeleton_directly() {
    let index_handler = r#"function(input)
        local path = input.path or ""
        if path:match("%.rs$") then
            return "fn main() [1-10]\nfn helper() [12-50]"
        end
        return { llm_output = "Unsupported file type", is_error = true }
    end"#;
    let (reg, _host) = setup(index_handler);
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("large.rs");
    create_lines_file(&file_path, 500);

    let out = exec_read(&reg, &file_path, 1, 0, |_| {}).unwrap();
    assert!(out.contains("Note:"));
    assert!(out.contains("500 lines"));
    assert!(out.contains("no slice range was specified"));
    assert!(out.contains("fn main() [1-10]"));
    assert!(out.contains("fn helper() [12-50]"));
    assert!(!out.contains("500: line 500"));
}

#[test]
fn large_file_unbounded_with_unsupported_index_type_returns_head_elision() {
    let index_handler = r#"function(input)
        local path = input.path or ""
        if path:match("%.rs$") then
            return "fn main() [1-10]"
        end
        return { llm_output = "Unsupported file type: .txt. Use the read tool instead.", is_error = true }
    end"#;
    let (reg, _host) = setup(index_handler);
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("large.txt");
    create_lines_file(&file_path, 500);

    let out = exec_read(&reg, &file_path, 1, 0, |_| {}).unwrap();
    assert!(out.contains("Note:"));
    assert!(out.contains("500 lines"));
    assert!(out.contains("Showing only lines 1-200 to prevent context bloat"));
    assert!(out.contains("1: line 1"));
    assert!(out.contains("200: line 200"));
    assert!(!out.contains("201: line 201"));
    assert!(out.contains("Truncated lines: 201-500. Use offset=201 to read further."));
}

#[test]
fn large_file_targeted_non_initial_offset_returns_slice_not_index() {
    let index_handler = r#"function(input)
        return "fn main() [1-10]"
    end"#;
    let (reg, _host) = setup(index_handler);
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("large.rs");
    create_lines_file(&file_path, 500);

    let out = exec_read(&reg, &file_path, 100, 0, |_| {}).unwrap();
    assert!(out.contains("Note:"));
    assert!(out.contains("Showing only lines 100-299"));
    assert!(out.contains("100: line 100"));
    assert!(out.contains("299: line 299"));
    assert!(!out.contains("300: line 300"));
    assert!(!out.contains("fn main() [1-10]"));
    assert!(out.contains("Truncated lines: 300-500. Use offset=300 to read further."));
}

#[test]
fn full_output_mode_bypasses_elision() {
    let (reg, _host) = setup("");
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("large.rs");
    create_lines_file(&file_path, 500);

    let out = exec_read(&reg, &file_path, 1, 0, |ctx| {
        ctx.output_mode = ToolOutputMode::Full;
    })
    .unwrap();
    assert!(!out.contains("Note:"));
    assert!(out.contains("1: line 1"));
    assert!(out.contains("500: line 500"));
}
