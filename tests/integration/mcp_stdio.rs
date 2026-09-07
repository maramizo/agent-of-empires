use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use serial_test::parallel;

#[test]
#[parallel]
fn mcp_stdio_keeps_stdout_and_local_state_clean() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aoe2"))
        .args(["mcp", "serve", "--url", "http://127.0.0.1:1"])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("AOE_LOG_LEVEL", "trace")
        .env_remove("AOE_DAEMON_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for request in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
    ] {
        writeln!(input, "{request}").unwrap();
    }
    writeln!(input, "invalid-json").unwrap();
    drop(input);
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("MCP server did not exit after stdin closed");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("stdout contains only JSON-RPC"))
        .collect();
    assert_eq!(replies.len(), 4, "notifications must not receive responses");
    assert_eq!(
        replies[0]["result"]["serverInfo"]["name"],
        "aoe-orchestrator"
    );
    let names: Vec<_> = replies[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"create_agent"));
    assert!(names.contains(&"list_external_conversations"));
    assert!(names.contains(&"onboard_conversation"));
    assert!(names.contains(&"create_project"));
    assert!(names.contains(&"assign_agent_project"));
    assert!(names.contains(&"add_agent_worktree"));
    assert!(names.contains(&"queue_message"));
    assert!(names.contains(&"read_agent_output"));
    assert_eq!(replies[2]["result"], json!({}));
    assert_eq!(replies[3]["error"]["code"], -32700);
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}
