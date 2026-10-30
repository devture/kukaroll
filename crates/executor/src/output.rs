// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

//! The run's output: everything kukaroll says and every child's stdout/stderr goes to stderr (the journal)
//! and into a bounded tail that Healthchecks receives on failure.

use std::io::{PipeReader, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, Signal, kill_process_group};

/// Healthchecks accepts up to 100 KB per ping; the end of the output is what explains a failure.
const TAIL_BYTES: usize = 20_000;

/// How long to wait between checks whether a child has exited.
const POLL_INTERVAL: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 200_000_000,
};

pub struct Output {
    tail: Vec<u8>,
}

impl Output {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self { tail: Vec::new() }
    }

    pub fn say(&mut self, message: impl AsRef<str>) {
        self.record(format!("kukaroll: {}\n", message.as_ref()).as_bytes());
    }

    /// The last `TAIL_BYTES` of the output (cut at a line start when possible).
    pub fn tail(&self) -> String {
        let start = self.tail.len().saturating_sub(TAIL_BYTES);
        let tail = &self.tail[start..];
        let tail = match (start > 0, tail.iter().position(|&b| b == b'\n')) {
            (true, Some(newline)) => &tail[newline + 1..],
            _ => tail,
        };
        String::from_utf8_lossy(tail).into_owned()
    }

    /// Runs a command with stdin closed and stdout/stderr captured, until it exits.
    ///
    /// Reading stops once the command has exited and its pending output is drained,
    /// even if a background process it started still holds the pipe open.
    pub fn run(&mut self, command: Command) -> Result<ExitStatus> {
        self.run_until(command, None)
    }

    /// Like `run`, but kills the command (in its own process group, so with whatever it started, e.g. git's ssh)
    /// once `timeout` has passed, and fails then.
    pub fn run_with_timeout(
        &mut self,
        mut command: Command,
        timeout: Duration,
    ) -> Result<ExitStatus> {
        command.process_group(0);
        self.run_until(command, Some(timeout))
    }

    fn run_until(&mut self, mut command: Command, timeout: Option<Duration>) -> Result<ExitStatus> {
        let (reader, writer) = std::io::pipe().context("cannot create a pipe")?;
        command
            .stdin(Stdio::null())
            .stdout(writer.try_clone().context("cannot duplicate a pipe")?)
            .stderr(writer);
        let program = command.get_program().to_string_lossy().into_owned();
        let mut child = command
            .spawn()
            .with_context(|| format!("cannot start {program}"))?;
        // The Command holds the pipe's write end; drop it so the pipe closes when the child exits.
        drop(command);
        let mut reader = reader;
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let mut killed = false;
        loop {
            if !killed && deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                if let Some(pid) = Pid::from_raw(child.id().cast_signed()) {
                    // Already gone if it exited meanwhile: the loop notices that next.
                    let _ = kill_process_group(pid, Signal::KILL);
                }
                killed = true;
            }
            if readable(&reader, &POLL_INTERVAL)? {
                if self.read_chunk(&mut reader)? {
                    break;
                }
            } else if child.try_wait()?.is_some() {
                // Drain what is already buffered, without waiting for stray background writers.
                while readable(&reader, &Timespec::default())? && !self.read_chunk(&mut reader)? {}
                break;
            }
        }
        let status = child.wait()?;
        if let (true, Some(timeout)) = (killed, timeout) {
            bail!("timed out after {}s ({status})", timeout.as_secs_f32());
        }
        Ok(status)
    }

    /// Reads once from a readable pipe.
    /// Returns true at end of file.
    fn read_chunk(&mut self, reader: &mut PipeReader) -> Result<bool> {
        let mut buffer = [0u8; 8192];
        let read = reader.read(&mut buffer).context("cannot read output")?;
        self.record(&buffer[..read]);
        Ok(read == 0)
    }

    fn record(&mut self, bytes: &[u8]) {
        // Failing to write to the journal must not fail the deploy.
        let _ = std::io::stderr().write_all(bytes);
        self.tail.extend_from_slice(bytes);
        if self.tail.len() > 2 * TAIL_BYTES {
            self.tail.drain(..self.tail.len() - TAIL_BYTES);
        }
    }
}

fn readable(reader: &PipeReader, timeout: &Timespec) -> Result<bool> {
    let fd = reader.as_fd();
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    loop {
        match poll(&mut fds, Some(timeout)) {
            Ok(ready) => return Ok(ready > 0),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error).context("cannot poll output"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn captures_stdout_and_stderr_and_the_exit_status() {
        let mut output = Output::new();
        output.say("starting");
        let status = output.run(sh("echo out; echo err >&2; exit 3")).unwrap();
        assert_eq!(status.code(), Some(3));
        assert_eq!(output.tail(), "kukaroll: starting\nout\nerr\n");
    }

    #[test]
    fn keeps_only_the_tail() {
        let mut output = Output::new();
        output
            .run(sh(
                "i=0; while [ $i -lt 5000 ]; do echo line-$i; i=$((i+1)); done",
            ))
            .unwrap();
        let tail = output.tail();
        assert!(tail.len() <= TAIL_BYTES);
        assert!(tail.starts_with("line-"), "{}", &tail[..20]);
        assert!(tail.ends_with("line-4999\n"));
    }

    #[test]
    fn does_not_wait_for_background_processes_holding_the_pipe() {
        let mut output = Output::new();
        let started = Instant::now();
        let status = output.run(sh("sleep 30 & echo done")).unwrap();
        assert!(status.success());
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(output.tail(), "done\n");
    }
}
