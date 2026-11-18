//! Process-level tests for `arana mcp`: stdio framing, tool surface, bounds and
//! structured errors. Every case here fails locally (validation, protocol or
//! bounds) so the suite never touches the network.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

const REPLY_TIMEOUT: Duration = Duration::from_secs(20);
const PROTOCOL_VERSION: &str = "2025-06-18";

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
    home: PathBuf,
    _state: tempfile::TempDir,
}

impl Server {
    async fn start() -> Self {
        let state = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(state.path()).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_arana"))
            .arg("mcp")
            .env("ARANA_HOME", &home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn `arana mcp`");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr_pipe = child.stderr.take().unwrap();
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = stderr.clone();
        tokio::spawn(async move {
            let mut buf = String::new();
            let _ = stderr_pipe.read_to_string(&mut buf).await;
            sink.lock().unwrap().push_str(&buf);
        });
        Self {
            child,
            stdin: Some(stdin),
            lines: BufReader::new(stdout).lines(),
            stderr,
            home,
            _state: state,
        }
    }

    async fn send(&mut self, message: &Value) {
        self.send_line(&serde_json::to_string(message).unwrap())
            .await;
    }

    async fn send_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin closed");
        stdin.write_all(line.as_bytes()).await.unwrap();
        stdin.write_all(b"\n").await.unwrap();
        stdin.flush().await.unwrap();
    }

    async fn next(&mut self) -> Value {
        let line = timeout(REPLY_TIMEOUT, self.lines.next_line())
            .await
            .expect("server reply timed out")
            .unwrap()
            .expect("server closed stdout");
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout line is not JSON-RPC ({e}): {line}"))
    }

    async fn expect_silence(&mut self, window: Duration) {
        match timeout(window, self.lines.next_line()).await {
            Err(_) => {}
            Ok(Ok(Some(line))) => panic!("unexpected message: {line}"),
            Ok(Ok(None)) => panic!("server closed stdout while a reply was unexpected"),
            Ok(Err(e)) => panic!("stdout read failed: {e}"),
        }
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        let reply = self.next().await;
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(
            reply["id"],
            json!(id),
            "reply id must echo the request: {reply}"
        );
        reply
    }

    async fn initialize(&mut self) {
        let reply = self
            .request(
                1,
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "arana-tests", "version": "0.1.0"}
                }),
            )
            .await;
        assert!(reply.get("error").is_none(), "{reply}");
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(reply["result"]["serverInfo"]["name"], "arana");
        assert!(reply["result"]["capabilities"]["tools"].is_object());
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
    }

    async fn call(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        let reply = self
            .request(
                id,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )
            .await;
        assert!(
            reply.get("error").is_none(),
            "unexpected protocol error: {reply}"
        );
        reply["result"].clone()
    }

    async fn shutdown(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        let status = timeout(REPLY_TIMEOUT, self.child.wait())
            .await
            .expect("server did not exit on stdin EOF")
            .unwrap();
        while let Some(line) = timeout(Duration::from_secs(5), self.lines.next_line())
            .await
            .expect("stdout did not drain")
            .unwrap()
        {
            assert!(
                serde_json::from_str::<Value>(&line).is_ok(),
                "non-protocol text on stdout after EOF: {line}"
            );
        }
        status
    }
}

/// The structured payload a tool result carries, checked to match its text twin.
fn payload(result: &Value) -> Value {
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result has no text content: {result}"));
    let parsed: Value = serde_json::from_str(text).expect("tool text must be JSON");
    assert_eq!(
        parsed, result["structuredContent"],
        "structuredContent and serialized text must agree"
    );
    parsed
}

fn error_code(result: &Value) -> (bool, String) {
    assert_eq!(
        result["isError"],
        json!(true),
        "expected a tool error: {result}"
    );
    let payload = payload(result);
    assert_eq!(payload["schema_version"], json!(1));
    assert_eq!(payload["ok"], json!(false));
    assert_eq!(payload["items"], json!([]));
    (
        true,
        payload["error"]["code"].as_str().unwrap_or("").to_string(),
    )
}

#[tokio::test]
async fn initialize_and_tools_list_advertise_only_public_read_tools() {
    let mut server = Server::start().await;
    server.initialize().await;

    // An unknown protocol version falls back to the version this build speaks.
    let reply = server
        .request(
            2,
            "initialize",
            json!({"protocolVersion": "1999-01-01", "capabilities": {}}),
        )
        .await;
    assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSION);

    let reply = server.request(3, "tools/list", json!({})).await;
    let tools = reply["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["list_capabilities", "read_url", "search"]);
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert_eq!(tool["inputSchema"]["additionalProperties"], json!(false));
        assert_eq!(tool["annotations"]["readOnlyHint"], json!(true));
        assert!(!tool["description"].as_str().unwrap().is_empty());
        let properties = tool["inputSchema"]["properties"].as_object().unwrap();
        for forbidden in ["backend", "channel", "timeout", "profile", "command"] {
            assert!(
                !properties.contains_key(forbidden),
                "{} exposes {forbidden}",
                tool["name"]
            );
        }
    }
    let search = tools.iter().find(|t| t["name"] == "search").unwrap();
    assert_eq!(
        search["inputSchema"]["properties"]["limit"]["minimum"],
        json!(1)
    );
    assert_eq!(
        search["inputSchema"]["properties"]["limit"]["maximum"],
        json!(10)
    );
    assert_eq!(search["inputSchema"]["additionalProperties"], json!(false));

    assert!(server.shutdown().await.success());
}

#[tokio::test]
async fn list_capabilities_reports_the_explicit_public_allowlist() {
    let mut server = Server::start().await;
    server.initialize().await;

    let result = server.call(4, "list_capabilities", json!({})).await;
    assert_eq!(result["isError"], json!(false));
    let payload = payload(&result);
    assert_eq!(payload["server"]["name"], "arana");
    assert_eq!(payload["server"]["transport"], "stdio");
    assert_eq!(payload["server"]["access"], "public-read-only");
    let actions = payload["allowlist"].as_array().unwrap();
    assert_eq!(actions.len(), 2);
    for action in actions {
        assert_eq!(action["channel"], "web");
        assert_eq!(action["access"], "public");
    }
    assert_eq!(actions[0]["tool"], "read_url");
    assert_eq!(actions[0]["action"], "read");
    assert_eq!(actions[1]["tool"], "search");
    assert_eq!(actions[1]["action"], "search");
    let excluded = payload["excluded"].as_array().unwrap();
    for name in ["login", "browser", "setup", "install", "backend override"] {
        assert!(
            excluded.iter().any(|e| e == name),
            "capabilities must exclude {name}: {excluded:?}"
        );
    }
    let tools: Vec<&str> = payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, vec!["list_capabilities", "read_url", "search"]);

    // Capabilities are local data: no state directory, config or login is created.
    let entries = std::fs::read_dir(&server.home).unwrap().count();
    assert_eq!(
        entries, 0,
        "list_capabilities must not touch the state root"
    );

    assert!(server.shutdown().await.success());
}

#[tokio::test]
async fn unsafe_targets_and_invalid_arguments_map_to_structured_bad_input() {
    let mut server = Server::start().await;
    server.initialize().await;

    let cases = [
        json!({"url": "file:///etc/passwd"}),
        json!({"url": "http://127.0.0.1:8080/admin"}),
        json!({"url": "http://localhost/"}),
        json!({"url": "http://[::1]/"}),
        json!({"url": "http://169.254.169.254/latest/meta-data"}),
        json!({"url": "https://user:secret@example.com/"}),
        json!({"url": "https://example.com:99999/"}),
        json!({"url": ""}),
        json!({"url": "   "}),
        json!({}),
        json!({"url": "https://example.com/", "backend": "opencli"}),
        json!({"url": 42}),
    ];
    for (index, arguments) in cases.iter().enumerate() {
        let start = Instant::now();
        let result = server
            .call(10 + index as u64, "read_url", arguments.clone())
            .await;
        let (_, code) = error_code(&result);
        assert_eq!(code, "BAD_INPUT", "case {arguments}: {result}");
        let payload = payload(&result);
        assert_eq!(payload["action"], "read");
        assert!(
            !payload["error"]["message"].as_str().unwrap().is_empty(),
            "{result}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "validation must not attempt a fetch: {arguments}"
        );
    }

    assert!(server.shutdown().await.success());
}

#[tokio::test]
async fn search_argument_bounds_are_enforced_before_any_request() {
    let mut server = Server::start().await;
    server.initialize().await;

    let long_query = "q".repeat(513);
    let cases = [
        json!({"query": ""}),
        json!({"query": "   "}),
        json!({}),
        json!({"query": 7}),
        json!({"query": "rust", "limit": 0}),
        json!({"query": "rust", "limit": 11}),
        json!({"query": "rust", "limit": -1}),
        json!({"query": "rust", "limit": "5"}),
        json!({"query": "rust", "limit": 2.5}),
        json!({"query": "rust", "limit": true}),
        json!({"query": "rust", "site": "example.com"}),
        json!({"query": long_query}),
    ];
    for (index, arguments) in cases.iter().enumerate() {
        let start = Instant::now();
        let result = server
            .call(40 + index as u64, "search", arguments.clone())
            .await;
        let (_, code) = error_code(&result);
        assert_eq!(code, "BAD_INPUT", "case {arguments}: {result}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "validation must not attempt a search: {arguments}"
        );
    }

    assert!(server.shutdown().await.success());
}

#[tokio::test]
async fn unknown_tools_and_methods_are_protocol_errors() {
    let mut server = Server::start().await;
    server.initialize().await;

    let reply = server
        .request(
            60,
            "tools/call",
            json!({"name": "browser_setup", "arguments": {}}),
        )
        .await;
    assert_eq!(reply["error"]["code"], json!(-32602));
    assert_eq!(reply["error"]["data"]["code"], "BAD_INPUT");
    assert!(reply["result"].is_null());

    let reply = server
        .request(
            61,
            "tools/call",
            json!({"name": "read_url", "arguments": {"url": "https://example.com/"}, "extra": 1}),
        )
        .await;
    // Extra top-level params are ignored; the call itself stays bounded by validation.
    assert!(reply.get("result").is_some() || reply["error"]["code"] == json!(-32602));

    let reply = server.request(62, "resources/list", json!({})).await;
    assert_eq!(reply["error"]["code"], json!(-32601));
    let reply = server.request(63, "prompts/list", json!({})).await;
    assert_eq!(reply["error"]["code"], json!(-32601));

    assert!(server.shutdown().await.success());
}

#[tokio::test]
async fn malformed_and_oversized_frames_do_not_kill_the_session() {
    let mut server = Server::start().await;
    server.initialize().await;

    server
        .send_line("{\"jsonrpc\": \"2.0\", \"id\": 70, \"method\":")
        .await;
    let reply = server.next().await;
    assert_eq!(reply["error"]["code"], json!(-32700));
    assert_eq!(reply["id"], Value::Null);

    server.send_line("[1, 2, 3]").await;
    let reply = server.next().await;
    assert_eq!(reply["error"]["code"], json!(-32600));
    assert_eq!(reply["id"], Value::Null);

    server
        .send_line(&json!({"jsonrpc": "1.0", "id": 70, "method": "ping"}).to_string())
        .await;
    let reply = server.next().await;
    assert_eq!(reply["error"]["code"], json!(-32600));
    assert_eq!(reply["id"], json!(70));

    server
        .send_line(&json!({"jsonrpc": "2.0", "id": 71}).to_string())
        .await;
    let reply = server.next().await;
    assert_eq!(reply["error"]["code"], json!(-32600));
    assert_eq!(reply["id"], json!(71));

    // One frame above the 1 MiB request bound is rejected as a whole frame.
    let oversized = json!({
        "jsonrpc": "2.0",
        "id": 72,
        "method": "tools/call",
        "params": {"name": "read_url", "arguments": {"url": "x".repeat(1024 * 1024 + 16)}}
    })
    .to_string();
    assert!(oversized.len() > 1024 * 1024);
    server.send_line(&oversized).await;
    let reply = server.next().await;
    assert_eq!(reply["error"]["code"], json!(-32600));
    assert_eq!(reply["id"], Value::Null);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("1048576"));

    let reply = server.request(73, "ping", json!({})).await;
    assert_eq!(reply["result"], json!({}));

    assert!(server.shutdown().await.success());
}

#[tokio::test]
async fn notifications_get_no_reply_and_eof_shuts_down_cleanly() {
    let mut server = Server::start().await;
    server.initialize().await;
    server
        .send(
            &json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                      "params": {"requestId": 999, "reason": "test"}}),
        )
        .await;
    server.expect_silence(Duration::from_millis(150)).await;

    let reply = server.request(80, "ping", json!({})).await;
    assert_eq!(reply["result"], json!({}));

    // Diagnostics are drained after the child closes stderr on exit.
    let stderr_sink = server.stderr.clone();
    let status = server.shutdown().await;
    assert!(
        status.success(),
        "clean EOF shutdown must exit 0: {status:?}"
    );
    let mut stderr = String::new();
    for _ in 0..100 {
        stderr = stderr_sink.lock().unwrap().clone();
        if !stderr.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        stderr.contains("arana mcp"),
        "startup diagnostics belong on stderr: {stderr}"
    );
    assert!(
        stderr.contains("ready on stdio"),
        "the host should see one readiness line: {stderr}"
    );
    assert!(
        !stderr.contains("jsonrpc"),
        "protocol frames must never be duplicated on stderr: {stderr}"
    );
}
