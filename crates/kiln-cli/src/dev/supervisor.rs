//! The processes `kiln dev` runs and what happens to them: the build, the `Procfile.dev`
//! services and one-off tasks (`kiln check`), each with a state machine for starting,
//! stopping (the whole process group, SIGTERM then SIGKILL), crashing and restarting.
//!
//! The supervisor never blocks: the event loop feeds it [`Event`]s and calls
//! [`Supervisor::tick`], and a frontend (inline, JSON or the TUI) reads its processes and
//! drains its [`Notice`]s.

use std::{
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use super::{
    build::{self, Activity, Diagnostics, Snapshot},
    desktop,
    lines::{Assembled, Assembler, Line, Ring},
    process::{Child, Exit, Io, Spec},
    procfile::Entry,
};

/// How long a process gets to exit after SIGTERM before its group gets SIGKILL. A kiln app
/// gives running jobs two seconds in development.
pub const GRACE: Duration = Duration::from_secs(3);
/// How long to wait for a group to disappear after SIGKILL.
const KILL_WAIT: Duration = Duration::from_secs(2);
/// Lines kept per process.
pub const LINES_PER_PROCESS: usize = 5000;
/// Lines kept across all processes, for the stream view and the exit flush.
pub const COMBINED_LINES: usize = 10_000;
/// How often sources are checked for changes.
const POLL: Duration = Duration::from_millis(300);
/// How long to let editors finish writing (format-on-save writes twice).
const SETTLE: Duration = Duration::from_millis(150);

/// Something for the supervisor (or the frontend) to handle.
#[derive(Debug)]
pub enum Event {
    /// Output from process `id`.
    Output { id: usize, stream: Stream, bytes: Vec<u8> },
    /// Process `id`'s `stream` ended.
    Closed { id: usize, stream: Stream },
    /// The build's stdout (cargo's JSON messages), once it exits.
    BuildMessages { generation: u64, stdout: String },
    /// A key press or resize, for the TUI.
    Input(ratatui::crossterm::event::Event),
}

/// Where output came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stream {
    Stdout,
    Stderr,
    /// A PTY's combined output.
    Pty,
}

impl Stream {
    /// `stdout` or `stderr` (a PTY's output counts as stdout).
    pub fn name(self) -> &'static str {
        match self {
            Stream::Stdout | Stream::Pty => "stdout",
            Stream::Stderr => "stderr",
        }
    }
}

/// What a process is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A `Procfile.dev` entry.
    Service { command: String, uses_app: bool },
    /// `cargo build` of the app.
    Build,
    /// A one-off `kiln` command, e.g. `check`.
    Task { args: Vec<String> },
}

/// What a process is doing, for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Not started: waiting for the first build.
    Waiting,
    Running,
    /// Running, and its output shows cargo compiling.
    Compiling,
    /// Running, and its output shows a failed cargo build.
    CompileFailed,
    /// The build is running.
    Building,
    /// The last build succeeded.
    Built,
    /// The last build failed.
    BuildFailed,
    /// Crashed; restarting shortly.
    Restarting,
    Stopping,
    /// Stopped by `kiln dev` or the user.
    Stopped,
    /// Exited with status 0.
    Exited,
    /// Exited with an error and isn't restarted.
    Crashed,
    /// Crashed too often (or right after starting) to keep restarting.
    Failed,
}

impl Status {
    /// A short label, e.g. `running`.
    pub fn label(self) -> &'static str {
        match self {
            Status::Waiting => "waiting",
            Status::Running => "running",
            Status::Compiling => "compiling",
            Status::CompileFailed => "build failed",
            Status::Building => "building",
            Status::Built => "built",
            Status::BuildFailed => "build failed",
            Status::Restarting => "restarting",
            Status::Stopping => "stopping",
            Status::Stopped => "stopped",
            Status::Exited => "exited",
            Status::Crashed => "crashed",
            Status::Failed => "failed",
        }
    }
}

/// When to restart a crashed service.
#[derive(Debug, Clone, Copy)]
pub struct RestartPolicy {
    /// Wait this long before restarting.
    pub delay: Duration,
    /// Give up after this many restarts in a row.
    pub max_attempts: u32,
    /// A process that dies sooner than this after starting isn't restarted: it would just
    /// die again (a typo in the command, the port taken).
    pub min_uptime: Duration,
    /// A process that ran this long before crashing starts counting attempts afresh.
    pub reset_after: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            delay: Duration::from_secs(1),
            max_attempts: 5,
            min_uptime: Duration::from_secs(1),
            reset_after: Duration::from_secs(30),
        }
    }
}

/// What to do about a service that exited on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Restart after `after`; this is restart number `attempt`.
    Restart { attempt: u32, after: Duration },
    /// Stop trying, for this reason.
    GiveUp(String),
    /// It finished cleanly: leave it.
    Leave,
}

impl RestartPolicy {
    /// What to do after `exit`, `uptime` after starting, with `attempts` restarts so far.
    pub fn decide(&self, exit: &Exit, uptime: Duration, attempts: u32) -> Decision {
        if exit.success() {
            return Decision::Leave;
        }
        if uptime < self.min_uptime {
            return Decision::GiveUp(format!("exited within {}s of starting", self.min_uptime.as_secs_f32()));
        }
        let attempts = if uptime >= self.reset_after { 0 } else { attempts };
        if attempts >= self.max_attempts {
            return Decision::GiveUp(format!("crashed {} times in a row", attempts + 1));
        }
        Decision::Restart { attempt: attempts + 1, after: self.delay }
    }
}

/// Something that happened, for the frontend to show.
#[derive(Debug, Clone)]
pub enum Notice {
    /// A line of output.
    Output {
        id: usize,
        stream: Stream,
        raw: String,
        line: Rc<Line>,
    },
    Started {
        id: usize,
        pid: i32,
    },
    /// Exited; `requested` when `kiln dev` stopped it.
    Exited {
        id: usize,
        exit: Exit,
        requested: bool,
    },
    Restarting {
        id: usize,
        attempt: u32,
        delay: Duration,
    },
    Failed {
        id: usize,
        reason: String,
    },
    /// A message from `kiln dev` itself.
    Say(String),
    BuildStarted,
    /// The build succeeded; `first` the first time.
    BuildSucceeded {
        first: bool,
    },
    BuildFailed,
}

/// What to do once a process is stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Then {
    Start,
    Stay,
}

enum State {
    /// Never started (waiting for the first build).
    Idle,
    Running {
        child: Child,
        since: Instant,
    },
    /// SIGTERM sent; SIGKILL at `deadline` unless `killed` already.
    Stopping {
        child: Child,
        deadline: Instant,
        killed: bool,
        then: Then,
    },
    /// Restarting at `until`.
    Backoff {
        until: Instant,
    },
    Done(Outcome),
}

/// How a process ended.
#[derive(Debug, Clone)]
enum Outcome {
    Exited(Exit),
    Stopped,
    Failed(String),
}

/// One process: its kind, state and recent output.
pub struct Proc {
    /// The label, e.g. `web`.
    pub name: String,
    pub kind: Kind,
    /// Recent output, oldest first.
    pub lines: Ring<Rc<Line>>,
    assemblers: HashMap<Stream, Assembler>,
    state: State,
    attempts: u32,
    activity: Option<Activity>,
}

impl Proc {
    fn new(name: &str, kind: Kind) -> Self {
        Self {
            name: name.to_owned(),
            kind,
            lines: Ring::new(LINES_PER_PROCESS),
            assemblers: HashMap::new(),
            state: State::Idle,
            attempts: 0,
            activity: None,
        }
    }

    /// What it's doing.
    pub fn status(&self) -> Status {
        match (&self.state, &self.kind) {
            (State::Idle, _) => Status::Waiting,
            (State::Running { .. }, Kind::Build) => Status::Building,
            (State::Running { .. }, _) => match self.activity {
                Some(Activity::Compiling) => Status::Compiling,
                Some(Activity::Failed) => Status::CompileFailed,
                _ => Status::Running,
            },
            (State::Stopping { .. }, _) => Status::Stopping,
            (State::Backoff { .. }, _) => Status::Restarting,
            (State::Done(Outcome::Exited(exit)), Kind::Build) => {
                if exit.success() {
                    Status::Built
                } else {
                    Status::BuildFailed
                }
            }
            (State::Done(Outcome::Failed(_)), Kind::Build) => Status::BuildFailed,
            (State::Done(Outcome::Exited(exit)), _) => {
                if exit.success() {
                    Status::Exited
                } else {
                    Status::Crashed
                }
            }
            (State::Done(Outcome::Stopped), _) => Status::Stopped,
            (State::Done(Outcome::Failed(_)), _) => Status::Failed,
        }
    }

    /// The process id while it runs.
    pub fn pid(&self) -> Option<i32> {
        match &self.state {
            State::Running { child, .. } | State::Stopping { child, .. } => Some(child.pgid()),
            _ => None,
        }
    }

    /// The line being written (a progress bar), if any.
    pub fn partial(&self) -> Option<Line> {
        [Stream::Pty, Stream::Stderr, Stream::Stdout]
            .iter()
            .find_map(|s| self.assemblers.get(s).and_then(Assembler::partial))
    }

    /// Why it failed for good, if it did.
    pub fn failure(&self) -> Option<&str> {
        match &self.state {
            State::Done(Outcome::Failed(reason)) => Some(reason),
            _ => None,
        }
    }

    /// What it runs, for display.
    pub fn command(&self) -> String {
        match &self.kind {
            Kind::Service { command, .. } => command.clone(),
            Kind::Build => "cargo build".into(),
            Kind::Task { args } => format!("kiln {}", args.join(" ")),
        }
    }

    fn live(&self) -> bool {
        matches!(self.state, State::Running { .. } | State::Stopping { .. })
    }
}

/// What the supervisor needs to know about the app and how to run things.
#[derive(Debug, Clone)]
pub struct Config {
    /// The app's directory (watched for changes).
    pub root: PathBuf,
    pub package: String,
    pub crate_name: String,
    /// Where processes run.
    pub cwd: PathBuf,
    /// `PORT` for the processes.
    pub port: String,
    pub io: Io,
    pub diagnostics: Diagnostics,
    /// Ask processes for colors (`CLICOLOR_FORCE=1`).
    pub color: bool,
    /// Show a desktop notification when a process fails for good.
    pub notify: bool,
}

struct Orphan {
    /// A group whose leader exited but whose other processes didn't.
    child: Child,
    deadline: Instant,
    killed: bool,
}

/// Runs and watches every process.
pub struct Supervisor {
    pub config: Config,
    /// Every process: services in `Procfile.dev` order, then the build, then tasks.
    pub procs: Vec<Proc>,
    /// Recent lines from every process, oldest first.
    pub combined: Ring<(usize, Rc<Line>)>,
    /// What happened since the frontend last looked.
    pub notices: Vec<Notice>,
    /// The build's index in `procs`.
    pub build: usize,
    events: Sender<Event>,
    policy: RestartPolicy,
    binary: Option<PathBuf>,
    generation: u64,
    build_exit: Option<Exit>,
    build_messages: Option<String>,
    build_queued: bool,
    seen: Snapshot,
    last_poll: Instant,
    change_at: Option<Instant>,
    orphans: Vec<Orphan>,
    quitting: bool,
}

impl Supervisor {
    /// A supervisor for `entries`; call [`Supervisor::begin`] to build and start them.
    pub fn new(config: Config, entries: &[Entry], events: Sender<Event>) -> Self {
        let mut procs: Vec<Proc> = entries
            .iter()
            .map(|e| Proc::new(&e.name, Kind::Service { command: e.command.clone(), uses_app: e.uses_app() }))
            .collect();
        let build = procs.len();
        procs.push(Proc::new("build", Kind::Build));
        Self {
            seen: build::snapshot(&config.root),
            config,
            procs,
            combined: Ring::new(COMBINED_LINES),
            notices: vec![],
            build,
            events,
            policy: RestartPolicy::default(),
            binary: None,
            generation: 0,
            build_exit: None,
            build_messages: None,
            build_queued: false,
            last_poll: Instant::now(),
            change_at: None,
            orphans: vec![],
            quitting: false,
        }
    }

    /// Starts the first build; the services start once it succeeds.
    pub fn begin(&mut self) {
        self.start(self.build);
    }

    /// The app's URL.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.config.port)
    }

    /// Whether the app has been built at least once.
    pub fn built(&self) -> bool {
        self.binary.is_some()
    }

    /// Whether `quit` was called.
    pub fn quitting(&self) -> bool {
        self.quitting
    }

    /// Whether everything has stopped after `quit`.
    pub fn done(&self) -> bool {
        self.quitting && !self.procs.iter().any(Proc::live) && self.orphans.is_empty()
    }

    /// Handles output and build results.
    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Output { id, stream, bytes } => {
                let Some(proc) = self.procs.get_mut(id) else { return };
                let done = proc.assemblers.entry(stream).or_default().feed(&bytes);
                for line in done {
                    self.push(id, stream, line);
                }
            }
            Event::Closed { id, stream } => {
                let line = self.procs.get_mut(id).and_then(|p| p.assemblers.get_mut(&stream)?.finish());
                if let Some(line) = line {
                    self.push(id, stream, line);
                }
            }
            Event::BuildMessages { generation, stdout } => {
                if generation == self.generation {
                    self.build_messages = Some(stdout);
                    self.finish_build();
                }
            }
            Event::Input(_) => {}
        }
    }

    /// Notices exits, restarts crashed services, escalates slow stops and rebuilds on changes.
    pub fn tick(&mut self, now: Instant) {
        for id in 0..self.procs.len() {
            self.tick_proc(id, now);
        }
        self.orphans.retain_mut(|orphan| {
            if orphan.child.gone() {
                return false;
            }
            if now >= orphan.deadline {
                if orphan.killed {
                    return false;
                }
                orphan.child.signal(libc::SIGKILL);
                orphan.killed = true;
                orphan.deadline = now + KILL_WAIT;
            }
            true
        });
        if !self.quitting {
            self.watch(now);
        }
    }

    fn tick_proc(&mut self, id: usize, now: Instant) {
        let proc = &mut self.procs[id];
        match &mut proc.state {
            State::Running { child, since } => {
                if let Some(exit) = child.try_wait() {
                    let since = *since;
                    let State::Running { child, .. } = std::mem::replace(&mut proc.state, State::Idle) else {
                        unreachable!()
                    };
                    self.exited(id, child, exit, now - since, now);
                }
            }
            State::Stopping { child, deadline, killed, then } => {
                if child.gone() {
                    let then = *then;
                    let exit = child.try_wait().expect("gone");
                    proc.state = State::Done(Outcome::Stopped);
                    self.notices.push(Notice::Exited { id, exit, requested: true });
                    match then {
                        Then::Start => self.start(id),
                        Then::Stay => self.system(id, "stopped"),
                    }
                } else if now >= *deadline {
                    if *killed {
                        // Nothing more to send; stop waiting.
                        let name = proc.name.clone();
                        proc.state = State::Done(Outcome::Stopped);
                        self.say(format!("{name} didn't exit after SIGKILL"));
                    } else {
                        child.signal(libc::SIGKILL);
                        *killed = true;
                        *deadline = now + KILL_WAIT;
                    }
                }
            }
            State::Backoff { until } => {
                if now >= *until {
                    self.start(id);
                }
            }
            State::Idle | State::Done(_) => {}
        }
    }

    /// Process `id` exited on its own.
    fn exited(&mut self, id: usize, mut child: Child, exit: Exit, uptime: Duration, now: Instant) {
        if !child.gone() {
            // Its leader is gone but something it started isn't: stop that too.
            child.signal(libc::SIGTERM);
            self.orphans.push(Orphan { child, deadline: now + GRACE, killed: false });
        }
        self.notices.push(Notice::Exited { id, exit: exit.clone(), requested: false });
        let proc = &mut self.procs[id];
        match proc.kind {
            Kind::Build => {
                proc.state = State::Done(Outcome::Exited(exit.clone()));
                self.build_exit = Some(exit);
                self.finish_build();
            }
            Kind::Task { .. } => {
                proc.state = State::Done(Outcome::Exited(exit.clone()));
                self.system(id, &format!("exited ({exit})"));
            }
            Kind::Service { .. } => {
                let name = proc.name.clone();
                self.system(id, &format!("exited ({exit})"));
                match self.policy.decide(&exit, uptime, self.procs[id].attempts) {
                    Decision::Restart { attempt, after } => {
                        let proc = &mut self.procs[id];
                        proc.attempts = attempt;
                        proc.state = State::Backoff { until: now + after };
                        let max = self.policy.max_attempts;
                        self.system(
                            id,
                            &format!("restarting in {}s (attempt {attempt} of {max})", after.as_secs_f32()),
                        );
                        self.notices.push(Notice::Restarting { id, attempt, delay: after });
                    }
                    Decision::GiveUp(reason) => {
                        self.procs[id].state = State::Done(Outcome::Failed(reason.clone()));
                        self.system(id, &format!("not restarting: {reason}"));
                        self.notices.push(Notice::Failed { id, reason: reason.clone() });
                        if self.config.notify {
                            desktop::notify(&format!("{name} stopped"), &format!("{name} {reason}."));
                        }
                    }
                    Decision::Leave => self.procs[id].state = State::Done(Outcome::Exited(exit)),
                }
            }
        }
    }

    /// Starts process `id` (which isn't running).
    fn start(&mut self, id: usize) {
        let proc = &mut self.procs[id];
        proc.activity = None;
        proc.assemblers.clear();
        let spec = match &proc.kind {
            Kind::Build => {
                self.generation += 1;
                self.build_exit = None;
                self.build_messages = None;
                let manifest = self.config.root.join("Cargo.toml");
                let started = build::start(&manifest, self.config.diagnostics, id, self.generation, &self.events);
                match started {
                    Ok(child) => {
                        let pid = child.pgid();
                        self.procs[id].state = State::Running { child, since: Instant::now() };
                        self.system(id, "cargo build");
                        self.notices.push(Notice::Started { id, pid });
                        self.notices.push(Notice::BuildStarted);
                    }
                    Err(e) => {
                        self.procs[id].state = State::Done(Outcome::Failed(e.clone()));
                        self.say(e);
                    }
                }
                return;
            }
            Kind::Service { command, uses_app } => {
                if *uses_app && self.binary.is_none() {
                    proc.state = State::Idle;
                    return;
                }
                let mut spec = Spec::shell(command, self.config.cwd.clone());
                if let Some(binary) = &self.binary {
                    spec.env.push(("APP".into(), binary.into()));
                }
                spec
            }
            Kind::Task { args } => {
                let exe = std::env::current_exe().unwrap_or_else(|_| "kiln".into());
                let mut full: Vec<std::ffi::OsString> = vec!["--app".into(), self.config.root.clone().into()];
                full.extend(args.iter().map(Into::into));
                Spec { program: exe.into(), args: full, env: vec![], cwd: self.config.cwd.clone() }
            }
        };
        let mut spec = spec;
        spec.env.push(("PORT".into(), self.config.port.clone().into()));
        spec.env.push(("KILN_DEV_STDIN_WATCH".into(), "1".into()));
        if self.config.color {
            spec.env.push(("CLICOLOR_FORCE".into(), "1".into()));
        }
        match Child::spawn(&spec, self.config.io, id, &self.events) {
            Ok(child) => {
                let pid = child.pgid();
                self.procs[id].state = State::Running { child, since: Instant::now() };
                let command = self.procs[id].command();
                self.system(id, &format!("started {command} (pid {pid})"));
                self.notices.push(Notice::Started { id, pid });
            }
            Err(e) => {
                let reason = format!("couldn't start: {e}");
                self.procs[id].state = State::Done(Outcome::Failed(reason.clone()));
                self.system(id, &reason);
                self.notices.push(Notice::Failed { id, reason });
            }
        }
    }

    /// SIGTERM to process `id`'s group; then start it again or leave it stopped.
    fn stop(&mut self, id: usize, then: Then) {
        let proc = &mut self.procs[id];
        match std::mem::replace(&mut proc.state, State::Idle) {
            State::Running { child, .. } => {
                child.signal(libc::SIGTERM);
                proc.state = State::Stopping { child, deadline: Instant::now() + GRACE, killed: false, then };
            }
            State::Stopping { child, deadline, killed, .. } => {
                proc.state = State::Stopping { child, deadline, killed, then };
            }
            other => {
                proc.state = other;
                match then {
                    Then::Start => self.start(id),
                    Then::Stay => {
                        if matches!(self.procs[id].state, State::Idle | State::Backoff { .. }) {
                            self.procs[id].state = State::Done(Outcome::Stopped);
                        }
                    }
                }
            }
        }
    }

    fn finish_build(&mut self) {
        let Some(exit) = self.build_exit.clone() else { return };
        if exit.success() && self.build_messages.is_none() {
            return;
        }
        self.build_exit = None;
        let messages = self.build_messages.take().unwrap_or_default();
        let binary = if exit.success() {
            build::binary(&messages, &self.config.package, &self.config.crate_name).map(Some)
        } else {
            Ok(None)
        };
        match binary {
            Ok(Some(binary)) => {
                let first = self.binary.is_none();
                self.binary = Some(binary);
                self.system(self.build, "build succeeded");
                self.notices.push(Notice::BuildSucceeded { first });
                if first {
                    self.say(format!("{}  (watching src/ and Cargo.toml)", self.url()));
                }
                self.start_services();
                if !first {
                    self.say("restarted".into());
                }
            }
            Ok(None) | Err(_) => {
                if let Err(e) = binary {
                    self.procs[self.build].state = State::Done(Outcome::Failed(e.clone()));
                    self.say(e);
                }
                self.system(self.build, "build failed");
                self.notices.push(Notice::BuildFailed);
                self.say(if self.binary.is_some() {
                    "build failed; still running the last good build".into()
                } else {
                    "build failed; waiting for changes".into()
                });
            }
        }
        if self.build_queued && !self.quitting {
            self.build_queued = false;
            self.start(self.build);
        }
    }

    /// After a build: (re)start the services that run the app, and start the others the
    /// first time.
    fn start_services(&mut self) {
        for id in 0..self.procs.len() {
            let Kind::Service { uses_app, .. } = self.procs[id].kind else { continue };
            let proc = &mut self.procs[id];
            if !uses_app {
                if matches!(proc.state, State::Idle) {
                    self.start(id);
                }
                continue;
            }
            proc.attempts = 0;
            match proc.state {
                State::Done(Outcome::Stopped) => {}
                State::Running { .. } | State::Stopping { .. } => self.stop(id, Then::Start),
                _ => self.start(id),
            }
        }
    }

    fn watch(&mut self, now: Instant) {
        if now.duration_since(self.last_poll) >= POLL {
            self.last_poll = now;
            if self.change_at.is_none() && build::snapshot(&self.config.root) != self.seen {
                self.change_at = Some(now + SETTLE);
            }
        }
        if let Some(at) = self.change_at
            && now >= at
        {
            self.change_at = None;
            self.seen = build::snapshot(&self.config.root);
            self.say("change detected; rebuilding".into());
            self.rebuild();
        }
    }

    /// Builds now, or after the build that's running.
    fn rebuild(&mut self) {
        if self.procs[self.build].live() {
            self.build_queued = true;
        } else {
            self.start(self.build);
        }
    }

    /// Restarts process `id` (the build: builds again).
    pub fn restart(&mut self, id: usize) {
        if self.quitting || id >= self.procs.len() {
            return;
        }
        if id == self.build {
            return self.rebuild();
        }
        self.procs[id].attempts = 0;
        self.stop(id, Then::Start);
    }

    /// Stops process `id` if it runs, or starts it.
    pub fn toggle(&mut self, id: usize) {
        if self.quitting || id >= self.procs.len() {
            return;
        }
        match &self.procs[id].state {
            State::Running { .. } | State::Backoff { .. } => self.stop(id, Then::Stay),
            State::Stopping { then: Then::Start, .. } => self.stop(id, Then::Stay),
            State::Stopping { .. } => self.stop(id, Then::Start),
            State::Idle | State::Done(_) => {
                if id == self.build {
                    return self.rebuild();
                }
                self.procs[id].attempts = 0;
                self.start(id);
            }
        }
    }

    /// Forgets process `id`'s output.
    pub fn clear(&mut self, id: usize) {
        if let Some(proc) = self.procs.get_mut(id) {
            proc.lines.clear();
        }
    }

    /// Runs `kiln <args>` as a task named `name` (again, if it ran before). Its index.
    pub fn task(&mut self, name: &str, args: &[&str]) -> usize {
        if let Some(id) = self.procs.iter().position(|p| p.name == name) {
            self.restart(id);
            return id;
        }
        let args = args.iter().map(|a| a.to_string()).collect();
        self.procs.push(Proc::new(name, Kind::Task { args }));
        let id = self.procs.len() - 1;
        if !self.quitting {
            self.start(id);
        }
        id
    }

    /// Resizes every process's terminal (and the build's progress bar).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if let Io::Pty { .. } = self.config.io {
            self.config.io = Io::Pty { cols, rows };
        }
        if let Diagnostics::Captured { color, .. } = self.config.diagnostics {
            self.config.diagnostics = Diagnostics::Captured { color, width: cols };
        }
        for proc in &self.procs {
            if let State::Running { child, .. } | State::Stopping { child, .. } = &proc.state {
                child.resize(cols, rows);
            }
        }
    }

    /// Stops everything: SIGTERM to every group, SIGKILL after [`GRACE`].
    pub fn quit(&mut self) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        self.build_queued = false;
        self.change_at = None;
        self.say("stopping".into());
        for id in 0..self.procs.len() {
            self.stop(id, Then::Stay);
        }
    }

    /// SIGKILL to every group still running.
    pub fn force(&mut self) {
        self.quit();
        let now = Instant::now();
        for proc in &mut self.procs {
            if let State::Stopping { child, deadline, killed, .. } = &mut proc.state
                && !*killed
            {
                child.signal(libc::SIGKILL);
                *killed = true;
                *deadline = now + KILL_WAIT;
            }
        }
        for orphan in &mut self.orphans {
            if !orphan.killed {
                orphan.child.signal(libc::SIGKILL);
                orphan.killed = true;
                orphan.deadline = now + KILL_WAIT;
            }
        }
    }

    fn push(&mut self, id: usize, stream: Stream, assembled: Assembled) {
        let Assembled { raw, line } = assembled;
        let proc = &mut self.procs[id];
        if matches!(proc.kind, Kind::Service { .. })
            && let Some(activity) = build::cargo_activity(&line.plain)
        {
            proc.activity = (activity != Activity::Done).then_some(activity);
        }
        let line = Rc::new(line);
        proc.lines.push(line.clone());
        self.combined.push((id, line.clone()));
        self.notices.push(Notice::Output { id, stream, raw, line });
    }

    /// A line from `kiln dev` in process `id`'s output.
    fn system(&mut self, id: usize, message: &str) {
        let line = Rc::new(Line::system(message));
        self.procs[id].lines.push(line.clone());
        self.combined.push((id, line));
    }

    /// A message from `kiln dev`, also kept with the build's output.
    fn say(&mut self, message: String) {
        self.system(self.build, &message);
        self.notices.push(Notice::Say(message));
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread};

    use super::*;

    fn exit(code: i32) -> Exit {
        Exit { code: Some(code), signal: None }
    }

    #[test]
    fn policy_restarts_with_a_cap() {
        let policy = RestartPolicy::default();
        let up = Duration::from_secs(5);
        assert_eq!(policy.decide(&exit(1), up, 0), Decision::Restart { attempt: 1, after: Duration::from_secs(1) });
        assert_eq!(policy.decide(&exit(1), up, 4), Decision::Restart { attempt: 5, after: Duration::from_secs(1) });
        assert!(matches!(policy.decide(&exit(1), up, 5), Decision::GiveUp(r) if r.contains("6 times")));
    }

    #[test]
    fn policy_leaves_clean_exits_and_gives_up_on_instant_deaths() {
        let policy = RestartPolicy::default();
        assert_eq!(policy.decide(&exit(0), Duration::from_secs(5), 0), Decision::Leave);
        let instant = policy.decide(&exit(1), Duration::from_millis(200), 0);
        assert!(matches!(instant, Decision::GiveUp(r) if r.contains("within 1s")));
        let signal = Exit { code: None, signal: Some("SIGSEGV".into()) };
        assert!(matches!(policy.decide(&signal, Duration::from_secs(2), 0), Decision::Restart { .. }));
    }

    #[test]
    fn policy_forgets_attempts_after_a_long_run() {
        let policy = RestartPolicy::default();
        assert_eq!(
            policy.decide(&exit(1), Duration::from_secs(60), 5),
            Decision::Restart { attempt: 1, after: Duration::from_secs(1) }
        );
    }

    fn supervisor(procfile: &str) -> (Supervisor, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let dir = std::env::temp_dir();
        let config = Config {
            root: dir.join("kiln-dev-test-no-app"),
            package: "none".into(),
            crate_name: "none".into(),
            cwd: dir,
            port: "0".into(),
            io: Io::Pipes,
            diagnostics: Diagnostics::Captured { color: false, width: 80 },
            color: false,
            notify: false,
        };
        let entries = super::super::procfile::parse(procfile).unwrap();
        (Supervisor::new(config, &entries, tx), rx)
    }

    fn run_until(sup: &mut Supervisor, rx: &mpsc::Receiver<Event>, mut done: impl FnMut(&Supervisor) -> bool) {
        let start = Instant::now();
        while !done(sup) {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out");
            while let Ok(event) = rx.try_recv() {
                sup.handle(event);
            }
            sup.tick(Instant::now());
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn services_without_the_app_run_restart_and_stop() {
        let (mut sup, rx) = supervisor("echo: echo hello; sleep 30\nquick: exit 3");
        // Start the services directly, as after a first build.
        sup.start_services();
        run_until(&mut sup, &rx, |s| s.procs[0].lines.iter().any(|l| l.plain == "hello"));
        assert_eq!(sup.procs[0].status(), Status::Running);
        run_until(&mut sup, &rx, |s| s.procs[1].status() == Status::Failed);
        assert!(sup.notices.iter().any(|n| matches!(n, Notice::Failed { id: 1, .. })));

        let first = sup.procs[0].pid().unwrap();
        sup.restart(0);
        run_until(&mut sup, &rx, |s| s.procs[0].pid().is_some_and(|p| p != first));
        assert!(!super::super::process::group_alive(first), "the old group is gone");

        sup.toggle(0);
        run_until(&mut sup, &rx, |s| s.procs[0].status() == Status::Stopped);
        sup.toggle(0);
        let pid = sup.procs[0].pid().unwrap();
        sup.quit();
        run_until(&mut sup, &rx, Supervisor::done);
        assert!(!super::super::process::group_alive(pid));
        assert_eq!(sup.procs[0].status(), Status::Stopped);
    }

    #[test]
    fn a_crash_after_starting_is_restarted() {
        let (mut sup, rx) = supervisor("flaky: sleep 1.2; exit 1");
        sup.start_services();
        run_until(&mut sup, &rx, |s| s.procs[0].status() == Status::Restarting);
        run_until(&mut sup, &rx, |s| s.procs[0].status() == Status::Running);
        assert_eq!(sup.procs[0].attempts, 1);
        sup.quit();
        run_until(&mut sup, &rx, Supervisor::done);
    }

    #[test]
    fn stubborn_groups_get_sigkill() {
        let (mut sup, rx) = supervisor("stubborn: trap '' TERM; sleep 30 & trap '' TERM; wait");
        sup.start_services();
        run_until(&mut sup, &rx, |s| s.procs[0].pid().is_some());
        let pid = sup.procs[0].pid().unwrap();
        thread::sleep(Duration::from_millis(100));
        sup.quit();
        sup.force();
        run_until(&mut sup, &rx, Supervisor::done);
        assert!(!super::super::process::group_alive(pid));
    }

    #[test]
    fn cargo_output_sets_compiling() {
        let (mut sup, rx) = supervisor("watch: echo '   Compiling app v0.1.0'; sleep 30");
        sup.start_services();
        run_until(&mut sup, &rx, |s| s.procs[0].status() == Status::Compiling);
        sup.quit();
        run_until(&mut sup, &rx, Supervisor::done);
    }
}
