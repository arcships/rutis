//! bash 工具(minimal mode,设计 §二.1):`bash -c` 新进程执行,
//! 状态不跨调用(workdir 参数而非 `cd`);非零退出以 `[exit code: N]`
//! 标记回喂而非报错;长输出截尾;超时杀进程。
//!
//! 描述抄 dsh `bashDescription`,裁掉 sandbox / background /
//! 环境变量段(minimal mode 明确不做);输出拼装对齐 dsh render:
//! stdout 主体 + `[stderr]` 段 + 行尾标记(exit 标记在最后)。

use std::collections::VecDeque;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;

use super::ToolDef;

/// 单流(stdout / stderr 各自)最大字符数,超出截尾。
const MAX_OUTPUT_CHARS: usize = 10_000;
/// 每条流最多保留 4 倍字符上限的尾部字节，覆盖 UTF-8 最长编码。
const MAX_OUTPUT_BYTES: usize = 4 * MAX_OUTPUT_CHARS;
/// 默认超时;`timeout_ms` 参数可覆盖,封顶 [`MAX_TIMEOUT_MS`]。
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

pub(crate) const BASH_DESCRIPTION: &str =
    "Execute a bash command (`bash -c`) and return its stdout/stderr. \
Each call runs in a fresh shell: no state (cwd, variables, functions) persists between calls — \
pass `workdir` instead of using `cd`. Non-zero exits are reported as `[exit code: N]`. \
Long output is truncated to its tail.";

/// bash 工具:`ToolDef` 数据,装进 `ToolsPlugin`(设计 §三,非独立插件)。
pub fn bash_tool() -> ToolDef {
    ToolDef::new(
        "bash",
        BASH_DESCRIPTION,
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The bash command to execute." },
                "description": {
                    "type": "string",
                    "description": "Clear, concise description of what this command does in active voice, 5-10 words (shown in the UI)."
                },
                "workdir": { "type": "string", "description": "Working directory for this command. Defaults to the process working directory; state does not persist between calls, so pass `workdir` instead of using `cd`." },
                "timeout_ms": { "type": "number", "description": "Timeout in milliseconds. The executor applies its configured default and cap, and kills the command on expiry." }
            },
            "required": ["command", "description"]
        }),
        run_bash,
    )
}

async fn run_bash(args: Value) -> Result<Value, String> {
    let command = args["command"]
        .as_str()
        .ok_or_else(|| "missing required parameter `command`".to_string())?
        .to_string();
    let timeout_ms = args["timeout_ms"]
        .as_u64()
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .min(MAX_TIMEOUT_MS);

    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c")
        .arg(&command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
    if let Some(dir) = args["workdir"].as_str() {
        cmd.current_dir(dir);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn bash: {e}"))?;
    let mut child = ManagedChild::new(child);
    let stdout = child.child.as_mut().unwrap().stdout.take().unwrap();
    let stderr = child.child.as_mut().unwrap().stderr.take().unwrap();
    let execution = async {
        let (stdout, stderr, status) = tokio::join!(
            read_tail(stdout),
            read_tail(stderr),
            child.child.as_mut().unwrap().wait()
        );
        Ok::<_, std::io::Error>((stdout?, stderr?, status?))
    };
    match tokio::time::timeout(Duration::from_millis(timeout_ms), execution).await {
        Err(_) => {
            child.terminate().await;
            Ok(Value::String(render(
                "",
                "",
                &[format!("[timed out after {timeout_ms}ms]")],
            )))
        }
        Ok(Err(e)) => {
            child.terminate().await;
            Err(format!("failed to run command: {e}"))
        }
        Ok(Ok((stdout, stderr, status))) => {
            child.finish();
            Ok(Value::String(render(
                &stdout.text(),
                &stderr.text(),
                &status_markers(&status),
            )))
        }
    }
}

/// Owns the shell until it is reaped. On Unix, the shell starts a fresh
/// process group and cancellation kills ordinary descendants in that group.
/// A process that deliberately leaves the group is outside this guarantee.
/// On non-Unix platforms only the direct child is managed.
struct ManagedChild {
    child: Option<Child>,
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl ManagedChild {
    fn new(child: Child) -> Self {
        Self {
            #[cfg(unix)]
            pgid: child.id().map(|id| id as i32),
            child: Some(child),
        }
    }

    fn kill_group(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            // Negative pid addresses the group created for this shell.
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
    }

    async fn terminate(&mut self) {
        self.kill_group();
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }

    fn finish(&mut self) {
        self.kill_group();
        self.child.take(); // wait() completed before this call.
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.kill_group();
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            // An aborted runner cannot await. Reap the direct shell before
            // its JoinHandle reports cancellation; kill_on_drop is fallback.
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        }
    }
}

#[derive(Default)]
struct TailOutput {
    bytes: VecDeque<u8>,
    total: usize,
}

impl TailOutput {
    fn text(mut self) -> String {
        let truncated = self.total > self.bytes.len();
        while self.bytes.front().is_some_and(|b| b & 0xc0 == 0x80) {
            self.bytes.pop_front();
        }
        let bytes: Vec<_> = self.bytes.into_iter().collect();
        let text = String::from_utf8_lossy(&bytes);
        if truncated {
            format!("[output truncated]\n{text}")
        } else {
            text.into_owned()
        }
    }
}

async fn read_tail(mut reader: impl AsyncRead + Unpin) -> std::io::Result<TailOutput> {
    let mut out = TailOutput::default();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Ok(out);
        }
        out.total = out.total.saturating_add(n);
        out.bytes.extend(&chunk[..n]);
        while out.bytes.len() > MAX_OUTPUT_BYTES {
            out.bytes.pop_front();
        }
    }
}

/// 非零退出 / 信号标记(exit 标记保持最后,对齐 dsh parse 契约)。
fn status_markers(status: &std::process::ExitStatus) -> Vec<String> {
    if let Some(code) = status.code() {
        if code != 0 {
            return vec![format!("[exit code: {code}]")];
        }
        return Vec::new();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let signal = status
            .signal()
            .map_or_else(|| "?".to_string(), |s| s.to_string());
        vec![format!("[killed by signal: {signal}]")]
    }
    #[cfg(not(unix))]
    vec!["[killed]".to_string()]
}

/// dsh render 同构:stdout 主体 + `[stderr]` 段 + 行尾标记;空输出 `(no output)`。
fn render(stdout: &str, stderr: &str, markers: &[String]) -> String {
    let mut body = truncate_tail(stdout);
    let err = truncate_tail(stderr);
    if !err.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(&err);
    }
    if body.is_empty() {
        body = "(no output)".to_string();
    }
    if markers.is_empty() {
        return body;
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&markers.join("\n"));
    body
}

/// 截尾保留末尾 [`MAX_OUTPUT_CHARS`] 个字符(UTF-8 边界安全)。
fn truncate_tail(s: &str) -> String {
    let count = s.chars().count();
    if count <= MAX_OUTPUT_CHARS {
        return s.to_string();
    }
    let tail: String = s.chars().skip(count - MAX_OUTPUT_CHARS).collect();
    format!("[output truncated]\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tail_reader_is_bounded_and_utf8_safe() {
        let (mut writer, reader) = tokio::io::duplex(1024);
        let send = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            writer
                .write_all(&vec![b'x'; MAX_OUTPUT_BYTES * 3])
                .await
                .unwrap();
            writer.write_all("中".as_bytes()).await.unwrap();
        });
        let tail = read_tail(reader).await.unwrap();
        send.await.unwrap();
        assert!(tail.bytes.len() <= MAX_OUTPUT_BYTES);
        assert_eq!(tail.total, MAX_OUTPUT_BYTES * 3 + 3);
        let text = tail.text();
        assert!(text.starts_with("[output truncated]\n"));
        assert!(text.ends_with('中'));
        assert!(!text.contains('�'));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn both_pipes_drain_and_background_holder_times_out() {
        let out = run_bash(json!({
            "command": "head -c 100000 /dev/zero | tr '\\0' o; head -c 100000 /dev/zero | tr '\\0' e >&2",
            "timeout_ms": 3000
        }))
        .await
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
        assert!(out.contains("[output truncated]"));
        assert!(out.contains("[stderr]"));
        assert!(out.contains('o') && out.contains('e'));

        let held = run_bash(json!({
            "command": "sleep 10 & echo done",
            "timeout_ms": 100
        }))
        .await
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
        assert!(held.contains("[timed out after 100ms]"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_stops_background_descendant() {
        let late = std::env::temp_dir().join(format!(
            "rutis-bash-timeout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let command = format!("(sleep 0.3; echo late > '{}') & wait", late.display());
        let out = run_bash(json!({"command": command, "timeout_ms": 100}))
            .await
            .unwrap();
        assert!(out.as_str().unwrap().contains("[timed out after 100ms]"));
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!late.exists(), "background descendant survived timeout");
        let _ = std::fs::remove_file(late);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn normal_exit_stops_background_descendant_with_closed_pipes() {
        let late = std::env::temp_dir().join(format!(
            "rutis-bash-normal-exit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let command = format!(
            "(sleep 0.3; echo late > '{}') >/dev/null 2>&1 & echo done",
            late.display()
        );
        let out = run_bash(json!({"command": command, "timeout_ms": 1000}))
            .await
            .unwrap();
        assert!(out.as_str().unwrap().contains("done"));
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!late.exists(), "background descendant survived shell exit");
        let _ = std::fs::remove_file(late);
    }
}
