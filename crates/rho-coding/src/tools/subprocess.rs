//! Shell subprocess execution with timeout, cancellation, and process-group
//! kill (tau `_communicate_with_cancellation` + `_kill_process_tree`).
//!
//! stdout and stderr are merged at the fd level (a single `os_pipe`, given to
//! the child as both streams) so the combined byte order matches tau's
//! `stderr=STDOUT`. On unix the child is started in its own process group
//! (`process_group(0)`), so a timeout/cancel `killpg` reaps the shell *and* its
//! pipeline/compound children — not just the top-level `sh`.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use rho_agent::provider::CancellationToken;

/// How the communicate loop terminated.
enum Stop {
    /// The process exited on its own; carries its status.
    Exited(Option<std::process::ExitStatus>),
    /// The timeout elapsed first.
    TimedOut,
    /// The cancellation signal fired first.
    Cancelled,
}

/// The captured outcome of a shell run.
pub struct BashExecution {
    /// The merged stdout+stderr bytes.
    pub output_bytes: Vec<u8>,
    /// Whether the run hit its timeout.
    pub timed_out: bool,
    /// Whether the run was cancelled via the signal.
    pub cancelled: bool,
    /// The process exit code (negative `-signal` when killed, tau parity).
    pub exit_code: Option<i64>,
}

/// Run `shell_command` under a shell, returning merged output and status.
pub async fn run_shell(
    shell_command: &str,
    cwd: &Path,
    timeout: Option<f64>,
    signal: Option<&dyn CancellationToken>,
    use_bash_executable: bool,
) -> Result<BashExecution, String> {
    let (mut reader, writer) = os_pipe::pipe().map_err(|e| e.to_string())?;
    let writer_clone = writer.try_clone().map_err(|e| e.to_string())?;

    let program = if use_bash_executable {
        "bash"
    } else {
        "/bin/sh"
    };
    let mut std_cmd = std::process::Command::new(program);
    std_cmd
        .arg("-c")
        .arg(shell_command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(writer))
        .stderr(Stdio::from(writer_clone));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        std_cmd.process_group(0);
    }

    let mut cmd = tokio::process::Command::from(std_cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    // Close the parent's copies of the pipe's write ends (they were moved into
    // `cmd`'s stdio config, and `spawn` only *dup*s them into the child). Until
    // every writer fd is closed the reader never sees EOF — the classic os_pipe
    // deadlock. The child keeps its own inherited copies.
    drop(cmd);
    let pid = child.id();

    // Blocking read of the merged pipe to EOF (EOF = every writer fd closed).
    let read_handle = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        buf
    });

    // The completion signal is process *exit* (`child.wait()`), never pipe EOF:
    // a command can close/redirect both output streams before it exits (e.g.
    // `exec >/dev/null 2>&1; sleep 60`), which reaches EOF early — so racing the
    // pipe read against the timeout would let such a command outlive its
    // deadline. Racing `wait()` keeps the timeout/cancel enforced against the
    // real process lifetime, matching tau's `communicate()` (which awaits exit).
    let stop = tokio::select! {
        status = child.wait() => Stop::Exited(status.ok()),
        () = maybe_timeout(timeout) => Stop::TimedOut,
        () = maybe_cancel(signal) => Stop::Cancelled,
    };

    // On timeout/cancel the `child.wait()` future is dropped (nothing was
    // reaped), so calling `wait()` again after the kill is valid.
    let (timed_out, cancelled, status) = match stop {
        Stop::Exited(status) => (false, false, status),
        Stop::TimedOut => {
            kill_process_group(pid);
            (true, false, child.wait().await.ok())
        }
        Stop::Cancelled => {
            kill_process_group(pid);
            (false, true, child.wait().await.ok())
        }
    };

    // Drain the pipe. The read completes once the exited/killed group closes
    // every writer fd (a no-op wait already reaped the process above).
    let output_bytes = read_handle.await.unwrap_or_default();
    let exit_code = status.and_then(exit_code_of);

    Ok(BashExecution {
        output_bytes,
        timed_out,
        cancelled,
        exit_code,
    })
}

async fn maybe_timeout(timeout: Option<f64>) {
    match timeout {
        Some(secs) if secs > 0.0 => tokio::time::sleep(Duration::from_secs_f64(secs)).await,
        _ => std::future::pending::<()>().await,
    }
}

async fn maybe_cancel(signal: Option<&dyn CancellationToken>) {
    match signal {
        Some(signal) => loop {
            if signal.is_cancelled() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        },
        None => std::future::pending::<()>().await,
    }
}

#[cfg(unix)]
fn kill_process_group(pid: Option<u32>) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    if let Some(pid) = pid {
        if let Ok(pid) = i32::try_from(pid) {
            // Ignore ESRCH (already gone), matching tau's ProcessLookupError guard.
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: Option<u32>) {
    // Non-unix fallback handled by kill_on_drop / direct kill is not modeled in
    // the M4a slice (development targets unix); see dev-notes/phase-4a.md.
}

#[cfg(unix)]
fn exit_code_of(status: std::process::ExitStatus) -> Option<i64> {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        Some(i64::from(code))
    } else {
        status.signal().map(|sig| -i64::from(sig))
    }
}

#[cfg(not(unix))]
fn exit_code_of(status: std::process::ExitStatus) -> Option<i64> {
    status.code().map(i64::from)
}
