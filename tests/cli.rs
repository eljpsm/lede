//! End-to-end tests of the binary: exit codes, stdout bytes, and the
//! requests that actually leave for the API. Every run gets a scratch HOME,
//! so the developer's real config is never read and first-run bootstrap is
//! exercised every time, and a mock chat-completions server, so no test
//! needs a network or a key.
//!
//! The unit tests cover the pieces. These cover the claim the pieces exist
//! to make: that inside `git commit -m "$(lede generate)"`, stdout is exactly one
//! well-formed message and nothing else.
//!
//! Shape of a test: build a `TempTree`, stage a change in its repo, start a
//! `mock_server` with canned replies, run the binary, assert on exit code
//! and output. `TempTree` cleans up on drop.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::thread::JoinHandle;

use tempfile::TempDir;

/// A scratch home and repository, removed on drop.
struct TempTree {
    root: TempDir,
}

impl TempTree {
    fn new(name: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("lede-cli-{name}-"))
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        std::fs::create_dir_all(root.path().join("repo")).unwrap();
        let tree = TempTree { root };
        git(&tree, &["init", "-q"]);
        tree
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn repo(&self) -> PathBuf {
        self.root.path().join("repo")
    }

    fn config_dir(&self) -> PathBuf {
        self.home().join(".config/lede")
    }

    fn stage_file(&self, file_name: &str, contents: &str) {
        std::fs::write(self.repo().join(file_name), contents).unwrap();
        git(self, &["add", file_name]);
    }
}

fn git(tree: &TempTree, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(tree.repo())
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// One canned successful chat-completions reply carrying `content`.
fn chat_response(content: &str) -> (u16, String) {
    let body = serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": content},
        }],
    })
    .to_string();
    (200, body)
}

/// A captured request: what actually left for the API.
struct Request {
    headers: String,
    body: String,
}

/// Serve one canned (status, body) per accepted connection, then stop.
/// Returns the port and a handle yielding the requests received, so tests
/// can assert on what actually left for the API.
fn mock_server(responses: Vec<(u16, String)>) -> (u16, JoinHandle<Vec<Request>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, response_body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_request(&mut stream));
            let response = format!(
                "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body,
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    (port, handle)
}

/// Enough HTTP to serve one client: read headers, honor Content-Length,
/// split the two apart.
fn read_request(stream: &mut impl Read) -> Request {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).unwrap();
        if n == 0 {
            return Request {
                headers: String::from_utf8_lossy(&buf).into_owned(),
                body: String::new(),
            };
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(header_end) = find(&buf, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_lowercase();
            let content_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < header_end + 4 + content_length {
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            return Request {
                headers,
                body: String::from_utf8_lossy(&buf[header_end + 4..]).into_owned(),
            };
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Run the binary under test from the tree's repo, sandboxed to its home,
/// pointed at the mock server, with a fake key in the environment.
fn lede(tree: &TempTree, port: u16, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lede"))
        .current_dir(tree.repo())
        .env("HOME", tree.home())
        .env("XDG_CONFIG_HOME", tree.home().join(".config"))
        .env("OPENAI_API_KEY", "test-key")
        .arg("generate")
        .arg("--base-url")
        .arg(format!("http://127.0.0.1:{port}/v1"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_staged_change_yields_one_formatted_message_on_stdout() {
    let tree = TempTree::new("happy-path");
    tree.stage_file("lib.rs", "fn main() {}\n");
    let (port, server) = mock_server(vec![chat_response(
        "Add lib module\n\nStub out the entry point.",
    )]);

    let output = lede(&tree, port, &[]);

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert_eq!(
        stdout_of(&output),
        "Add lib module\n\nStub out the entry point.\n"
    );

    // First run bootstrapped the config, reported on stderr only.
    assert!(stderr_of(&output).contains("wrote"));
    assert!(tree.config_dir().join("config.toml").is_file());
    assert!(tree.config_dir().join("prompt.txt").is_file());

    // The request carried the system prompt, the file list, and the diff.
    let requests = server.join().unwrap();
    assert!(requests[0].body.contains("git commit messages"));
    assert!(requests[0].body.contains("Staged files:"));
    assert!(requests[0].body.contains("A\\tlib.rs"));
    assert!(
        requests[0]
            .headers
            .contains("authorization: bearer test-key")
    );
}

#[test]
fn the_hint_reaches_the_model() {
    let tree = TempTree::new("hint");
    tree.stage_file("lib.rs", "fn main() {}\n");
    let (port, server) = mock_server(vec![chat_response("Add lib module")]);

    let output = lede(&tree, port, &["refactor only"]);

    assert!(output.status.success());
    let requests = server.join().unwrap();
    assert!(
        requests[0]
            .body
            .contains("Additional context from the user: refactor only")
    );
}

#[test]
fn an_overlong_subject_gets_one_retry() {
    let tree = TempTree::new("retry");
    tree.stage_file("lib.rs", "fn main() {}\n");
    let long = "Add a subject line that is way too long to pass the fifty character check";
    let (port, server) = mock_server(vec![chat_response(long), chat_response("Add lib module")]);

    let output = lede(&tree, port, &[]);

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "Add lib module\n");
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].body.contains("limit is 50"));
}

#[test]
fn a_retry_that_stays_long_falls_back_to_truncation() {
    let tree = TempTree::new("truncate");
    tree.stage_file("lib.rs", "fn main() {}\n");
    let long = "Add a subject line that is way too long to pass the fifty character check";
    let (port, _server) = mock_server(vec![chat_response(long), chat_response(long)]);

    let output = lede(&tree, port, &[]);

    assert!(output.status.success());
    let stdout = stdout_of(&output);
    let subject = stdout.lines().next().unwrap();
    assert!(subject.chars().count() <= 50, "subject too long: {subject}");
    assert!(long.starts_with(subject));
}

#[test]
fn the_model_override_reaches_the_request() {
    let tree = TempTree::new("model-override");
    tree.stage_file("lib.rs", "fn main() {}\n");
    let (port, server) = mock_server(vec![chat_response("Add lib module")]);

    let output = lede(&tree, port, &["-m", "qwen2.5-coder"]);

    assert!(output.status.success());
    let requests = server.join().unwrap();
    // Parsed rather than substring-matched: ureq pretty-prints its JSON,
    // and the whitespace is not part of the contract.
    let request: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(request["model"], "qwen2.5-coder");
}

// The retry is best effort: when the second request fails outright, the
// first reply is still salvaged by truncation rather than failing the run.
#[test]
fn a_failed_retry_still_truncates() {
    let tree = TempTree::new("failed-retry");
    tree.stage_file("lib.rs", "fn main() {}\n");
    let long = "Add a subject line that is way too long to pass the fifty character check";
    // One response only: the server is gone by the time the retry connects.
    let (port, _server) = mock_server(vec![chat_response(long)]);

    let output = lede(&tree, port, &[]);

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let stdout = stdout_of(&output);
    let subject = stdout.lines().next().unwrap();
    assert!(subject.chars().count() <= 50, "subject too long: {subject}");
    assert!(long.starts_with(subject));
}

#[test]
fn an_api_error_reports_status_and_message() {
    let tree = TempTree::new("api-error");
    tree.stage_file("lib.rs", "fn main() {}\n");
    let error = r#"{"error": {"message": "Incorrect API key provided", "type": "auth"}}"#;
    let (port, _server) = mock_server(vec![(401, error.to_owned())]);

    let output = lede(&tree, port, &[]);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        stderr_of(&output).contains("API error (HTTP 401): Incorrect API key provided"),
        "stderr: {}",
        stderr_of(&output)
    );
}

#[test]
fn a_non_json_error_body_falls_back_to_its_first_line() {
    let tree = TempTree::new("plain-error");
    tree.stage_file("lib.rs", "fn main() {}\n");
    let (port, _server) = mock_server(vec![(502, "upstream exploded\ndetails\n".to_owned())]);

    let output = lede(&tree, port, &[]);

    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("API error (HTTP 502): upstream exploded"),
        "stderr: {}",
        stderr_of(&output)
    );
}

#[test]
fn an_empty_reply_is_an_error() {
    let tree = TempTree::new("empty-reply");
    tree.stage_file("lib.rs", "fn main() {}\n");
    let (port, _server) = mock_server(vec![chat_response("")]);

    let output = lede(&tree, port, &[]);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        stderr_of(&output).contains("API returned an empty message"),
        "stderr: {}",
        stderr_of(&output)
    );
}

#[test]
fn a_reply_with_no_choices_is_an_error() {
    let tree = TempTree::new("no-choices");
    tree.stage_file("lib.rs", "fn main() {}\n");
    let (port, _server) = mock_server(vec![(200, r#"{"choices": []}"#.to_owned())]);

    let output = lede(&tree, port, &[]);

    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("API returned no choices"),
        "stderr: {}",
        stderr_of(&output)
    );
}

#[test]
fn nothing_staged_fails_before_any_request() {
    let tree = TempTree::new("nothing-staged");

    // Port 1 refuses connections, so reaching the API at all would surface
    // as a connection error instead of the message asserted below.
    let output = lede(&tree, 1, &[]);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        stderr_of(&output).contains("nothing staged"),
        "stderr: {}",
        stderr_of(&output)
    );
    // A doomed run must not bootstrap config files: the staged change is
    // read before config loading, and this pins that order.
    assert!(!tree.config_dir().exists());
}

#[test]
fn a_non_repository_fails_before_config_loading() {
    let tree = TempTree::new("not-repo");
    let dir = tree.root.path().join("not-repo");
    std::fs::create_dir_all(&dir).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_lede"))
        .current_dir(dir)
        .env("HOME", tree.home())
        .env("XDG_CONFIG_HOME", tree.home().join(".config"))
        .env_remove("OPENAI_API_KEY")
        .arg("generate")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    // git's own error is passed through, prefixed with the command that
    // failed; lede adds no repository detection of its own.
    assert!(stderr_of(&output).contains("git diff --cached"));
    assert!(!tree.config_dir().exists());
}

// The whole point of auth.command: no key anywhere in the environment, yet
// the request goes out authorized with what the command printed.
#[test]
fn the_key_command_supplies_the_key() {
    let tree = TempTree::new("key-cmd");
    tree.stage_file("lib.rs", "fn main() {}\n");
    std::fs::create_dir_all(tree.config_dir()).unwrap();
    std::fs::write(
        tree.config_dir().join("config.toml"),
        "[auth]\nmode = \"bearer\"\ncommand = \"echo key-from-command\"\n",
    )
    .unwrap();
    let (port, server) = mock_server(vec![chat_response("Add lib module")]);

    let output = Command::new(env!("CARGO_BIN_EXE_lede"))
        .current_dir(tree.repo())
        .env("HOME", tree.home())
        .env("XDG_CONFIG_HOME", tree.home().join(".config"))
        .env_remove("OPENAI_API_KEY")
        .arg("generate")
        .arg("--base-url")
        .arg(format!("http://127.0.0.1:{port}/v1"))
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert_eq!(stdout_of(&output), "Add lib module\n");
    let requests = server.join().unwrap();
    assert!(
        requests[0]
            .headers
            .contains("authorization: bearer key-from-command"),
        "headers: {}",
        requests[0].headers
    );
}

#[test]
fn no_authentication_omits_the_authorization_header() {
    let tree = TempTree::new("no-auth");
    tree.stage_file("lib.rs", "fn main() {}\n");
    std::fs::create_dir_all(tree.config_dir()).unwrap();
    std::fs::write(
        tree.config_dir().join("config.toml"),
        "[auth]\nmode = \"none\"\n",
    )
    .unwrap();
    let (port, server) = mock_server(vec![chat_response("Add lib module")]);

    let output = Command::new(env!("CARGO_BIN_EXE_lede"))
        .current_dir(tree.repo())
        .env("HOME", tree.home())
        .env("XDG_CONFIG_HOME", tree.home().join(".config"))
        .env_remove("OPENAI_API_KEY")
        .arg("generate")
        .arg("--base-url")
        .arg(format!("http://127.0.0.1:{port}/v1"))
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let requests = server.join().unwrap();
    assert!(!requests[0].headers.contains("authorization:"));
}

#[test]
fn a_missing_api_key_names_the_variable() {
    let tree = TempTree::new("no-key");
    tree.stage_file("lib.rs", "fn main() {}\n");

    let output = Command::new(env!("CARGO_BIN_EXE_lede"))
        .current_dir(tree.repo())
        .env("HOME", tree.home())
        .env("XDG_CONFIG_HOME", tree.home().join(".config"))
        .env_remove("OPENAI_API_KEY")
        .arg("generate")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        stderr_of(&output).contains("OPENAI_API_KEY is not set"),
        "stderr: {}",
        stderr_of(&output)
    );
}

/// Run `lede init` sandboxed to the tree's home. `key` controls whether
/// OPENAI_API_KEY is present, which init's advisory check reports on.
fn lede_init(tree: &TempTree, key: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lede"));
    command
        .current_dir(tree.repo())
        .env("HOME", tree.home())
        .env("XDG_CONFIG_HOME", tree.home().join(".config"))
        .env_remove("OPENAI_API_KEY")
        .arg("init");
    if let Some(key) = key {
        command.env("OPENAI_API_KEY", key);
    }
    command.output().unwrap()
}

#[test]
fn init_writes_the_default_files_and_reports_them() {
    let tree = TempTree::new("init");

    let output = lede_init(&tree, Some("test-key"));

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let stdout = stdout_of(&output);
    assert!(stdout.contains("wrote"), "stdout: {stdout}");
    assert!(stdout.contains("config.toml"), "stdout: {stdout}");
    assert!(stdout.contains("prompt.txt"), "stdout: {stdout}");
    assert!(stdout.contains("the api key resolves"), "stdout: {stdout}");
    assert!(tree.config_dir().join("config.toml").is_file());
    assert!(tree.config_dir().join("prompt.txt").is_file());
}

// Idempotence is the promise that makes "run lede init" safe advice on any
// machine: a second run must keep user edits, not reset them to defaults.
#[test]
fn a_second_init_keeps_edited_files() {
    let tree = TempTree::new("init-again");
    lede_init(&tree, Some("test-key"));
    std::fs::write(
        tree.config_dir().join("config.toml"),
        "model = \"llama3\"\n",
    )
    .unwrap();

    let output = lede_init(&tree, Some("test-key"));

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let stdout = stdout_of(&output);
    assert!(!stdout.contains("wrote"), "stdout: {stdout}");
    assert!(stdout.contains("kept existing"), "stdout: {stdout}");
    let config = std::fs::read_to_string(tree.config_dir().join("config.toml")).unwrap();
    assert_eq!(config, "model = \"llama3\"\n");
}

// The check is advisory: the files were written, so init succeeds and the
// missing key is a warning naming both ways to provide one.
#[test]
fn init_without_a_key_warns_but_succeeds() {
    let tree = TempTree::new("init-no-key");

    let output = lede_init(&tree, None);

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let stdout = stdout_of(&output);
    assert!(stdout.contains("warning:"), "stdout: {stdout}");
    assert!(
        stdout.contains("OPENAI_API_KEY is not set"),
        "stdout: {stdout}"
    );
    assert!(tree.config_dir().join("config.toml").is_file());
}

// A bare `lede` must not default to generating (and spending an API call);
// clap prints usage and exits nonzero.
#[test]
fn a_bare_invocation_fails_with_usage() {
    let tree = TempTree::new("bare");

    let output = Command::new(env!("CARGO_BIN_EXE_lede"))
        .current_dir(tree.repo())
        .env("HOME", tree.home())
        .env("XDG_CONFIG_HOME", tree.home().join(".config"))
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("Usage"),
        "stderr: {}",
        stderr_of(&output)
    );
    assert!(!tree.config_dir().exists());
}
