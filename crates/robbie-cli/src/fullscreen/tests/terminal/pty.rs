//! Unix process/descriptor ownership for the fullscreen regression scenarios.
use std::{
    fs::File,
    io::{Read, Write},
    os::fd::{AsFd, AsRawFd},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, FdFlag, fcntl},
    poll::{PollFd, PollFlags, poll},
    pty::{Winsize, openpty},
    sys::{
        signal::{Signal, kill},
        stat::Mode,
        termios::{Termios, tcgetattr},
    },
    unistd::{Pid, mkfifo},
};
use tempfile::TempDir;

pub(super) fn contains(output: &[u8], marker: &[u8]) -> bool {
    output.windows(marker.len()).any(|bytes| bytes == marker)
}

pub(super) fn verify_fixture(selector: &str) -> Result<()> {
    let listed = Command::new(std::env::current_exe()?)
        .args(["--exact", selector, "--ignored", "--list"])
        .output()
        .context("could not list the terminal child fixture")?;
    ensure!(
        listed.status.success(),
        "fixture listing failed: {selector}"
    );
    let stdout = String::from_utf8(listed.stdout)?;
    let matches: Vec<_> = stdout
        .lines()
        .filter(|line| line.ends_with(": test"))
        .collect();
    ensure!(
        matches.len() == 1 && matches[0] == format!("{selector}: test"),
        "expected exactly one ignored fixture: {selector}; got {matches:?}"
    );
    Ok(())
}

pub(super) struct TerminalProcess {
    master: File,
    slave: File,
    original: Termios,
    child: Child,
    output: Vec<u8>,
    selector: &'static str,
    // Keep the FIFO inside the tool workspace until the child and PTY are closed.
    _scratch: TempDir,
}

impl TerminalProcess {
    pub(super) fn spawn(selector: &'static str) -> Result<Self> {
        verify_fixture(selector)?;
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        let scratch = tempfile::Builder::new()
            .prefix(".robbie-pty-")
            .tempdir_in(&workspace)?;
        let fifo = scratch.path().join("pipe");
        mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR)?;
        let size = Winsize {
            ws_row: 24,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let opened = openpty(Some(&size), None)?;
        // Keep the master and original slave out of the child; it receives
        // only the slave duplicates assigned to stdin/stdout/stderr.
        fcntl(&opened.master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        fcntl(&opened.slave, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        let master = File::from(opened.master);
        let slave = File::from(opened.slave);
        let original = tcgetattr(&slave)?;
        let child = Command::new(std::env::current_exe()?)
            .args(["--exact", selector, "--ignored", "--nocapture"])
            .current_dir(workspace)
            .env("TERM", "xterm-256color")
            .env("ROBBIE_TEST_FIFO", &fifo)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave.try_clone()?))
            .spawn()
            .context("could not spawn the terminal child fixture")?;
        Ok(Self {
            master,
            slave,
            original,
            child,
            output: Vec::new(),
            selector,
            _scratch: scratch,
        })
    }

    pub(super) fn output(&self) -> &[u8] {
        &self.output
    }

    fn collect_once(&mut self, timeout_ms: u16) -> Result<bool> {
        let ready = {
            let mut descriptors = [PollFd::new(self.master.as_fd(), PollFlags::POLLIN)];
            loop {
                match poll(&mut descriptors, timeout_ms) {
                    Ok(_) => {
                        break descriptors[0]
                            .revents()
                            .unwrap_or_else(PollFlags::empty)
                            .contains(PollFlags::POLLIN);
                    }
                    Err(Errno::EINTR) => continue,
                    Err(error) => return Err(error.into()),
                }
            }
        };
        if !ready {
            return Ok(false);
        }
        let mut bytes = [0; 65_536];
        let count = match self.master.read(&mut bytes) {
            Ok(count) => count,
            Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        self.output.extend_from_slice(&bytes[..count]);
        ensure!(
            self.output.len() <= 16 * 1024 * 1024,
            "PTY output exceeded the fixture limit"
        );
        Ok(count != 0)
    }

    pub(super) fn wait_for(
        &mut self,
        observation: &str,
        timeout: Duration,
        predicate: impl Fn(&[u8]) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if predicate(&self.output) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let tail = &self.output[self.output.len().saturating_sub(2000)..];
                // Debug formatting escapes terminal controls, including OSC 52.
                bail!(
                    "{}: timed out waiting for {observation}; exit={:?}; tail={:?}",
                    self.selector,
                    self.child.try_wait()?,
                    String::from_utf8_lossy(tail)
                );
            }
            self.collect_once(50)?;
        }
    }

    pub(super) fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.master.write_all(bytes)?;
        Ok(())
    }

    pub(super) fn type_and_wait(&mut self, bytes: &[u8], marker: &[u8]) -> Result<()> {
        let mark = self.output.len();
        self.send(bytes)?;
        self.wait_for(
            "typed input acknowledgement",
            Duration::from_secs(3),
            |output| contains(&output[mark..], marker),
        )
    }

    pub(super) fn resize(&self, rows: u16, columns: u16) -> Result<()> {
        let size = Winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: slave is a live PTY descriptor and size points to an initialized
        // winsize for the TIOCSWINSZ request; the kernel reads it synchronously.
        let result = unsafe { libc::ioctl(self.slave.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        ensure!(
            result == 0,
            "could not resize PTY: {}",
            std::io::Error::last_os_error()
        );
        kill(
            Pid::from_raw(i32::try_from(self.child.id())?),
            Signal::SIGWINCH,
        )?;
        Ok(())
    }

    pub(super) fn finish(&mut self, shutdown_mark: usize) -> Result<()> {
        self.wait_for(
            "Ctrl+C alternate-screen restoration",
            Duration::from_secs(3),
            |output| contains(&output[shutdown_mark..], b"\x1b[?1049l"),
        )?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            ensure!(
                Instant::now() < deadline,
                "PTY child did not exit after restoring the screen"
            );
            self.collect_once(50)?;
        };
        while self.collect_once(100)? {}
        ensure!(status.success(), "PTY child fixture failed: {status}");
        ensure!(
            contains(&self.output, b"1 passed"),
            "PTY child selector did not execute a test"
        );
        for mode in [1000, 1002, 1003, 1006, 2004] {
            ensure!(
                contains(
                    &self.output[shutdown_mark..],
                    format!("\x1b[?{mode}l").as_bytes()
                ),
                "terminal mode {mode} was not restored"
            );
        }
        ensure!(
            tcgetattr(&self.slave)? == self.original,
            "terminal attributes were not restored"
        );
        Ok(())
    }
}

impl Drop for TerminalProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
