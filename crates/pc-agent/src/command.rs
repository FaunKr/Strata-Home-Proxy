//! Runs a program, reads its output line by line and can end it with every
//! program it started. Setup, update and benchmark of Strata run through this.

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::{mpsc, oneshot},
};

/// What to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Working directory; the service's own if `None`.
    pub dir: Option<PathBuf>,
}

/// Ends a running command and the programs it started.
pub type Cancel = Arc<dyn Fn() + Send + Sync>;

/// A started command: its output, its end and a way to cancel it.
pub struct Running {
    lines: mpsc::UnboundedReceiver<String>,
    exit: oneshot::Receiver<Option<i32>>,
    cancel: Cancel,
}

impl Running {
    /// For implementations of [`crate::platform::Platform::run`]: `lines`
    /// carries the output (a line rewritten with `\r` ends in `\r`), `exit`
    /// the exit code (`None` without one, e.g. after a cancel), `cancel`
    /// ends the command.
    pub fn new(
        lines: mpsc::UnboundedReceiver<String>,
        exit: oneshot::Receiver<Option<i32>>,
        cancel: Cancel,
    ) -> Self {
        Self {
            lines,
            exit,
            cancel,
        }
    }

    /// The next line of output; `None` once the command closed its output.
    pub async fn next_line(&mut self) -> Option<String> {
        let line = self.next_output().await?;
        Some(line.strip_suffix('\r').map_or(line.clone(), String::from))
    }

    /// The next line as written: a line a progress bar rewrites with `\r`
    /// ends in `\r`, and the next line takes its place.
    pub async fn next_output(&mut self) -> Option<String> {
        self.lines.recv().await
    }

    /// A handle that cancels the command from elsewhere.
    pub fn canceller(&self) -> Cancel {
        Arc::clone(&self.cancel)
    }

    /// Waits for the end; the exit code, or `None` if there was none.
    pub async fn wait(self) -> Option<i32> {
        self.exit.await.ok().flatten()
    }
}

/// Starts `spec` on this machine.
pub fn spawn(spec: &CommandSpec) -> Result<Running> {
    let mut cmd = tokio::process::Command::new(&spec.program);
    cmd.args(&spec.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Meant for this program alone, not for what it starts.
        .env_remove(crate::platform::REPLACES_ENV);
    if let Some(dir) = &spec.dir {
        cmd.current_dir(dir);
    }
    // Python holds its output back when it goes to a pipe; then the lines
    // would come in batches instead of live.
    cmd.env("PYTHONUNBUFFERED", "1");
    #[cfg(windows)]
    cmd.creation_flags(crate::platform::CREATE_NO_WINDOW);
    // Its own process group, so a cancel reaches everything it started.
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("{} cannot be started", spec.program.display()))?;
    let pid = child.id();
    let (tx, lines) = mpsc::unbounded_channel();
    if let Some(out) = child.stdout.take() {
        tokio::spawn(read_lines(out, tx.clone()));
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(read_lines(err, tx));
    }
    let (exit_tx, exit) = oneshot::channel();
    tokio::spawn(async move {
        let code = child.wait().await.ok().and_then(|s| s.code());
        let _ = exit_tx.send(code);
    });
    let cancel: Cancel = Arc::new(move || {
        if let Some(pid) = pid {
            kill_tree(pid);
        }
    });
    Ok(Running::new(lines, exit, cancel))
}

/// Sends every line of `from` to `to`. Progress bars rewrite their line with
/// `\r`, so that ends a line too; such a line keeps its `\r` at the end, so
/// whoever shows the output can let the next one replace it.
/// [`Running::next_line`] drops it again. `\r\n` ends a line like `\n`.
async fn read_lines(mut from: impl AsyncRead + Unpin, to: mpsc::UnboundedSender<String>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    // A `\r` waits for the next byte: before `\n` it is part of a line end.
    let mut carriage = false;
    while let Ok(n) = from.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        for &b in &chunk[..n] {
            if carriage && b != b'\n' {
                send_line(&mut buf, true, &to);
            }
            carriage = b == b'\r';
            match b {
                b'\n' => send_line(&mut buf, false, &to),
                b'\r' => {}
                _ => buf.push(b),
            }
        }
    }
    send_line(&mut buf, carriage, &to);
}

fn send_line(buf: &mut Vec<u8>, rewritten: bool, to: &mpsc::UnboundedSender<String>) {
    let mut line = String::from_utf8_lossy(buf).trim_end().to_string();
    buf.clear();
    if !line.is_empty() {
        if rewritten {
            line.push('\r');
        }
        let _ = to.send(line);
    }
}

#[cfg(windows)]
fn kill_tree(pid: u32) {
    let _ = crate::platform::hidden(std::process::Command::new("taskkill").args([
        "/F",
        "/T",
        "/PID",
        &pid.to_string(),
    ]))
    .status();
}

#[cfg(unix)]
fn kill_tree(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .status();
}
