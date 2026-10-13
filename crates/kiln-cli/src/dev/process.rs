//! Starting processes, each in its own process group, and signalling those groups.
//!
//! Every process `kiln dev` starts leads a new process group (a new session, on a PTY),
//! so a signal to the group reaches whatever the command started too: `sh -c` and its
//! children, `cargo run` and the app, `npm` and its watcher. Groups are only ever
//! signalled by the ids recorded here when they were started; nothing is found by name.

use std::{
    collections::BTreeSet,
    ffi::OsString,
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Mutex, mpsc::Sender},
    thread,
    time::Duration,
};

use portable_pty::{CommandBuilder, MasterPty, PtySize};

use super::supervisor::{Event, Stream};

/// Process groups started and not yet known to be gone, for cleanup on a panic.
static GROUPS: Mutex<BTreeSet<i32>> = Mutex::new(BTreeSet::new());

/// Sends `signal` to every process in group `pgid`. Whether the group still exists.
pub fn signal_group(pgid: i32, signal: i32) -> bool {
    // kill(0, ..) is our own group and kill(-1, ..) every process we may signal.
    if pgid <= 1 {
        return false;
    }
    // SAFETY: kill has no memory-safety preconditions.
    unsafe { libc::kill(-pgid, signal) == 0 }
}

/// Whether any process in group `pgid` exists (zombies count until reaped).
pub fn group_alive(pgid: i32) -> bool {
    if pgid <= 1 {
        return false;
    }
    // SAFETY: as above; signal 0 only checks.
    let alive = unsafe { libc::kill(-pgid, 0) } == 0;
    alive || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Forgets `pgid` once the group is gone.
pub fn forget_group(pgid: i32) {
    GROUPS.lock().unwrap_or_else(|e| e.into_inner()).remove(&pgid);
}

/// SIGTERM to every group still running, then SIGKILL to those left after `grace`. For a
/// panic, when the normal shutdown can't run.
pub fn terminate_all(grace: Duration) {
    let Ok(groups) = GROUPS.try_lock().map(|g| g.clone()) else { return };
    for &pgid in &groups {
        signal_group(pgid, libc::SIGTERM);
    }
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < grace && groups.iter().any(|&g| group_alive(g)) {
        thread::sleep(step);
        waited += step;
    }
    for &pgid in &groups {
        signal_group(pgid, libc::SIGKILL);
    }
}

/// How a process exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    /// The exit code, unless a signal ended it.
    pub code: Option<i32>,
    /// The signal that ended it.
    pub signal: Option<String>,
}

impl Exit {
    /// Exited with code 0.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.signal, self.code) {
            (Some(signal), _) => write!(f, "signal: {signal}"),
            (None, Some(code)) => write!(f, "exit status: {code}"),
            (None, None) => write!(f, "exited"),
        }
    }
}

/// What a process is connected to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Io {
    /// A pseudo-terminal of this size: colors and progress bars as in a terminal.
    Pty { cols: u16, rows: u16 },
    /// Pipes for stdout and stderr, and a stdin pipe the app watches.
    Pipes,
}

/// A command to start.
#[derive(Debug, Clone)]
pub struct Spec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(String, OsString)>,
    pub cwd: PathBuf,
}

impl Spec {
    /// `sh -c <command>`.
    pub fn shell(command: &str, cwd: PathBuf) -> Self {
        Self { program: "sh".into(), args: vec!["-c".into(), command.into()], env: vec![], cwd }
    }
}

/// A running (or exited, not yet reaped) process `kiln dev` started.
pub struct Child {
    kind: Kind,
    pgid: i32,
    exit: Option<Exit>,
}

enum Kind {
    Pipes {
        child: std::process::Child,
    },
    Pty {
        child: Box<dyn portable_pty::Child + Send + Sync>,
        /// Kept open until the group is gone: closing it hangs up the terminal, and a
        /// process still shutting down would fail to write its last lines.
        master: Box<dyn MasterPty + Send>,
    },
}

impl Child {
    /// Starts `spec` in a new process group; its output arrives on `events` as
    /// [`Event::Output`] for `id`.
    pub fn spawn(spec: &Spec, io: Io, id: usize, events: &Sender<Event>) -> Result<Self, String> {
        match io {
            Io::Pipes => {
                use std::os::unix::process::CommandExt;
                let mut command = Command::new(&spec.program);
                command
                    .args(&spec.args)
                    .envs(spec.env.iter().map(|(k, v)| (k, v)))
                    .current_dir(&spec.cwd)
                    .process_group(0)
                    // Kiln apps exit when this pipe closes, so they can't outlive `kiln dev`
                    // even if it's killed outright (SIGKILL) and can't stop them itself.
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                let mut child = command.spawn().map_err(|e| e.to_string())?;
                let pgid = child.id() as i32;
                remember(pgid);
                read(child.stdout.take().expect("piped"), id, Stream::Stdout, events.clone());
                read(child.stderr.take().expect("piped"), id, Stream::Stderr, events.clone());
                Ok(Self { kind: Kind::Pipes { child }, pgid, exit: None })
            }
            Io::Pty { cols, rows } => {
                let pair = portable_pty::native_pty_system()
                    .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
                    .map_err(|e| e.to_string())?;
                let mut command = CommandBuilder::new(&spec.program);
                command.args(&spec.args);
                command.cwd(&spec.cwd);
                for (key, value) in &spec.env {
                    command.env(key, value);
                }
                // The new session (setsid) makes the process its group's leader.
                let child = pair.slave.spawn_command(command).map_err(|e| e.to_string())?;
                drop(pair.slave);
                let pgid = child.process_id().ok_or("the process has no id")? as i32;
                remember(pgid);
                let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
                read(reader, id, Stream::Pty, events.clone());
                // No writer is taken: dropping one sends EOF, which a kiln app reads as
                // "kiln dev is gone".
                Ok(Self { kind: Kind::Pty { child, master: pair.master }, pgid, exit: None })
            }
        }
    }

    /// Wraps a process started elsewhere in its own process group (the build).
    pub fn from_std(child: std::process::Child) -> Self {
        let pgid = child.id() as i32;
        remember(pgid);
        Self { kind: Kind::Pipes { child }, pgid, exit: None }
    }

    /// The process (and process group) id.
    pub fn pgid(&self) -> i32 {
        self.pgid
    }

    /// The exit, once the process (the group's leader) has exited.
    pub fn try_wait(&mut self) -> Option<Exit> {
        if self.exit.is_none() {
            self.exit = match &mut self.kind {
                Kind::Pipes { child } => child.try_wait().ok().flatten().map(|status| {
                    use std::os::unix::process::ExitStatusExt;
                    Exit { code: status.code(), signal: status.signal().map(signal_name) }
                }),
                Kind::Pty { child, .. } => child.try_wait().ok().flatten().map(|status| match status.signal() {
                    Some(signal) => Exit { code: None, signal: Some(signal.to_owned()) },
                    None => Exit { code: Some(status.exit_code() as i32), signal: None },
                }),
            };
        }
        self.exit.clone()
    }

    /// Whether the leader has exited and every other process in its group is gone.
    pub fn gone(&mut self) -> bool {
        self.try_wait().is_some() && !group_alive(self.pgid)
    }

    /// Sends `signal` to the whole group.
    pub fn signal(&self, signal: i32) {
        signal_group(self.pgid, signal);
    }

    /// Resizes the process's terminal, if it has one.
    pub fn resize(&self, cols: u16, rows: u16) {
        if let Kind::Pty { master, .. } = &self.kind {
            let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if self.gone() {
            forget_group(self.pgid);
        }
    }
}

fn remember(pgid: i32) {
    GROUPS.lock().unwrap_or_else(|e| e.into_inner()).insert(pgid);
}

fn read(mut output: impl Read + Send + 'static, id: usize, stream: Stream, events: Sender<Event>) {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        // A PTY reports EIO once every process on it is gone.
        while let Ok(n) = output.read(&mut buf) {
            if n == 0 || events.send(Event::Output { id, stream, bytes: buf[..n].to_vec() }).is_err() {
                break;
            }
        }
        let _ = events.send(Event::Closed { id, stream });
    });
}

fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGHUP => "SIGHUP".into(),
        libc::SIGINT => "SIGINT".into(),
        libc::SIGKILL => "SIGKILL".into(),
        libc::SIGSEGV => "SIGSEGV".into(),
        libc::SIGTERM => "SIGTERM".into(),
        libc::SIGABRT => "SIGABRT".into(),
        other => format!("{other}"),
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Instant};

    use super::*;

    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if done() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// A shell that starts a grandchild ignoring nothing: the group signal reaches both.
    fn stops_the_whole_group(io: Io) {
        let (tx, rx) = mpsc::channel();
        let spec = Spec::shell("sleep 30 & sleep 30; wait", std::env::temp_dir());
        let mut child = Child::spawn(&spec, io, 0, &tx).unwrap();
        let pgid = child.pgid();
        assert!(pgid > 1);
        // SAFETY: getpgid has no preconditions.
        assert_eq!(unsafe { libc::getpgid(pgid) }, pgid, "leads its own group");
        assert!(group_alive(pgid));
        child.signal(libc::SIGTERM);
        assert!(wait_until(|| child.gone()), "the shell and both sleeps exit");
        assert!(child.try_wait().is_some());
        drop(child);
        drop(rx);
    }

    #[test]
    fn pipes_group_is_stopped() {
        stops_the_whole_group(Io::Pipes);
    }

    #[test]
    fn pty_group_is_stopped() {
        stops_the_whole_group(Io::Pty { cols: 80, rows: 24 });
    }

    #[test]
    fn pty_output_arrives() {
        let (tx, rx) = mpsc::channel();
        let spec = Spec::shell("printf 'hi\\n'; [ -t 1 ] && echo tty", std::env::temp_dir());
        let mut child = Child::spawn(&spec, Io::Pty { cols: 80, rows: 24 }, 3, &tx).unwrap();
        let mut out = vec![];
        while let Ok(event) = rx.recv_timeout(Duration::from_secs(5)) {
            match event {
                Event::Output { id, bytes, .. } => {
                    assert_eq!(id, 3);
                    out.extend(bytes);
                }
                Event::Closed { .. } => break,
                _ => {}
            }
        }
        assert!(wait_until(|| child.gone()));
        assert_eq!(child.try_wait().unwrap(), Exit { code: Some(0), signal: None });
        assert_eq!(String::from_utf8_lossy(&out).replace('\r', ""), "hi\ntty\n");
    }

    #[test]
    fn never_signals_reserved_groups() {
        assert!(!signal_group(0, 0));
        assert!(!signal_group(1, 0));
        assert!(!signal_group(-5, 0));
        assert!(!group_alive(1));
    }
}
