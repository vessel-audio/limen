//! One launched control panel inside a private desktop, with waiting
//! queries over its accessibility tree and teardown that leaves evidence.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use atspi::{Role, State};
use voice_input_core::{Paths, v1};

use crate::daemon::{FakeDaemon, Scenario};
use crate::desktop::{
    Button, Desktop, Key, Node, SystemScheme, WmRequest, X11, describe, viewport,
};

/// `WM_CLASS` of the control panel, its application ID.
pub const APP_ID: &str = "io.github.Emin017.VoiceInput";

/// The idle dictation status in each interface language.
const IDLE: [&str; 2] = ["Idle", "空闲"];

/// How long any expected change may take; Lavapipe startup is the slowest.
const TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(50);

static CASE: Mutex<&str> = Mutex::new("");
static SESSIONS: AtomicUsize = AtomicUsize::new(0);

/// Record which case runs, which names its artifact directory.
pub fn begin_case(name: &'static str) {
    *CASE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = name;
    let dir = artifacts_for(name);
    let _ = std::fs::remove_dir_all(&dir);
}

/// One directory per case, nested by module: CI artifact paths forbid `:`.
pub fn artifacts_for(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("gui-e2e")
        .join(name.replace("::", "/"))
}

/// How a case names a node: by accessible ID, or by role for the controls
/// whose components set no ID.
#[derive(Debug, Clone, Copy)]
pub enum Target<'a> {
    Id(&'a str),
    Role(Role),
}

impl Target<'_> {
    fn matches(&self, node: &Node) -> bool {
        match self {
            Self::Id(id) => node.id == *id,
            Self::Role(role) => node.role == *role,
        }
    }
}

impl<'a> From<&'a str> for Target<'a> {
    fn from(id: &'a str) -> Self {
        Self::Id(id)
    }
}

impl From<Role> for Target<'_> {
    fn from(role: Role) -> Self {
        Self::Role(role)
    }
}

/// Which daemon the control panel finds when it starts.
pub enum Daemon {
    /// The scripted fake, listening before the window opens.
    Fake(Box<Scenario>),
    /// Nothing; the control panel starts the real daemon itself.
    Real,
}

/// Everything a case can choose about the desktop it starts in.
pub struct Launch {
    pub daemon: Daemon,
    /// A config file written before anything reads it.
    pub config: Option<&'static str>,
    pub lang: &'static str,
    pub scheme: SystemScheme,
}

impl Default for Launch {
    fn default() -> Self {
        Self {
            daemon: Daemon::Fake(Box::default()),
            config: None,
            lang: "en_US.UTF-8",
            scheme: SystemScheme::Unset,
        }
    }
}

pub struct Session {
    artifacts: PathBuf,
    paths: Paths,
    env: Vec<(&'static str, OsString)>,
    fake: Option<FakeDaemon>,
    gui: Child,
    desktop: Desktop,
}

impl Session {
    pub fn fake(scenario: Scenario) -> Result<Self> {
        Self::launch(Launch {
            daemon: Daemon::Fake(Box::new(scenario)),
            ..Launch::default()
        })
    }

    pub fn real() -> Result<Self> {
        Self::launch(Launch {
            daemon: Daemon::Real,
            ..Launch::default()
        })
    }

    pub fn launch(launch: Launch) -> Result<Self> {
        let case = *CASE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let artifacts = artifacts_for(case);
        std::fs::create_dir_all(&artifacts)?;
        let root = std::env::temp_dir().join(format!(
            "vi-e2e-{}-{}",
            std::process::id(),
            SESSIONS.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let desktop = Desktop::start(&root, launch.scheme).inspect_err(|_| {
            let _ = std::fs::remove_dir_all(&root);
        })?;
        let env = desktop
            .env()?
            .into_iter()
            .map(|(name, value)| match name {
                "LANG" => (name, launch.lang.into()),
                _ => (name, value),
            })
            .collect::<Vec<_>>();
        let paths = Paths {
            config_file: root.join("config/voice-input/config.toml"),
            socket: root.join("run/voice-input.sock"),
            log_file: root.join("state/voice-input/daemon.log"),
            cache_dir: root.join("cache/voice-input"),
        };
        if let Some(config) = launch.config {
            std::fs::create_dir_all(paths.config_file.parent().context("config has no parent")?)?;
            std::fs::write(&paths.config_file, config)?;
        }
        let waits_for_snapshot =
            matches!(&launch.daemon, Daemon::Fake(scenario) if !scenario.outdated);
        let fake = match launch.daemon {
            Daemon::Fake(scenario) => Some(FakeDaemon::spawn(
                &env,
                &paths.socket,
                &artifacts,
                &scenario,
            )?),
            Daemon::Real => None,
        };
        let log = std::fs::File::create(artifacts.join("gui.log"))?;
        let gui = Command::new(env!("CARGO_BIN_EXE_voice-input"))
            .arg("gui")
            .env_clear()
            .envs(env.iter().cloned())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("cannot start the control panel")?;
        let session = Self {
            artifacts,
            paths,
            env,
            fake,
            gui,
            desktop,
        };
        session.node("open-settings")?;
        session.x11().focus_window(APP_ID)?;
        // Controls stay disabled until the daemon's first snapshot; a slow
        // machine would otherwise take a case's first press on one of them.
        if waits_for_snapshot {
            session.wait("the daemon's first snapshot", |nodes| {
                nodes
                    .iter()
                    .any(|node| node.id == "dictation-status" && IDLE.contains(&node.name.as_str()))
                    .then_some(())
            })?;
        }
        Ok(session)
    }

    pub fn x11(&self) -> &X11 {
        &self.desktop.x11
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn fake_daemon(&mut self) -> Result<&mut FakeDaemon> {
        self.fake
            .as_mut()
            .context("this session runs the real daemon")
    }

    /// The requests the fake daemon received, without subscriptions.
    pub fn requests(&self) -> Result<Vec<v1::Request>> {
        Ok(self
            .fake
            .as_ref()
            .context("this session runs the real daemon")?
            .requests()?
            .into_iter()
            .filter(|request| *request != v1::Request::Subscribe)
            .collect())
    }

    /// Programs the control panel started, as argument vectors.
    pub fn launched(&self) -> Result<Vec<Vec<String>>> {
        self.desktop.launched()
    }

    /// Poll the accessibility tree until `probe` finds what it looks for.
    pub fn wait<T>(&self, what: &str, probe: impl Fn(&[Node]) -> Option<T>) -> Result<T> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let nodes = self.desktop.a11y.nodes();
            if let Some(found) = nodes.as_deref().ok().and_then(&probe) {
                return Ok(found);
            }
            if Instant::now() > deadline {
                bail!(
                    "timed out waiting for {what}\n{}",
                    nodes.map_or_else(|error| format!("{error:#}"), |nodes| describe(&nodes))
                );
            }
            std::thread::sleep(POLL);
        }
    }

    /// Poll anything else until it holds.
    pub fn until(&self, what: &str, mut check: impl FnMut() -> Result<bool>) -> Result<()> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match check() {
                Ok(true) => return Ok(()),
                Ok(false) | Err(_) if Instant::now() <= deadline => std::thread::sleep(POLL),
                Ok(false) => bail!("timed out waiting for {what}"),
                Err(error) => return Err(error.context(format!("waiting for {what}"))),
            }
        }
    }

    /// The first on-screen node the target names.
    pub fn node<'a>(&self, target: impl Into<Target<'a>>) -> Result<Node> {
        let target = target.into();
        self.wait(&format!("{target:?}"), |nodes| {
            nodes
                .iter()
                .find(|node| target.matches(node) && node.bounds.is_some())
                .cloned()
        })
    }

    /// Wait until the node's name reads `name`.
    pub fn named(&self, id: &str, name: &str) -> Result<Node> {
        self.wait(&format!("#{id} named {name:?}"), |nodes| {
            nodes
                .iter()
                .find(|node| node.id == id && node.name == name)
                .cloned()
        })
    }

    /// Wait until the node has, or lacks, a state.
    pub fn state(&self, id: &str, state: State, present: bool) -> Result<Node> {
        self.wait(&format!("#{id} {state:?}={present}"), |nodes| {
            nodes
                .iter()
                .find(|node| node.id == id && node.has(state) == present)
                .cloned()
        })
    }

    pub fn absent(&self, id: &str) -> Result<()> {
        self.wait(&format!("#{id} to disappear"), |nodes| {
            (!nodes.iter().any(|node| node.id == id)).then_some(())
        })
    }

    /// Wait until the node's bounds stop moving, so motion has settled.
    pub fn settled<'a>(&self, target: impl Into<Target<'a>>) -> Result<Node> {
        let target = target.into();
        self.settled_by(&format!("{target:?}"), |nodes| {
            nodes
                .iter()
                .find(|node| target.matches(node) && node.bounds.is_some())
                .cloned()
        })
    }

    /// Wait until the node `find` picks stops moving.
    pub fn settled_by(&self, what: &str, find: impl Fn(&[Node]) -> Option<Node>) -> Result<Node> {
        let mut previous = self.wait(what, &find)?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            std::thread::sleep(Duration::from_millis(80));
            let current = self.wait(what, &find)?;
            if current.bounds == previous.bounds {
                return Ok(current);
            }
            if Instant::now() > deadline {
                bail!("{what} kept moving: {:?}", current.bounds);
            }
            previous = current;
        }
    }

    /// Turn the wheel over the node's scroll view until the whole node shows,
    /// as a user scrolls to a control before pressing it.
    pub fn reveal<'a>(&self, target: impl Into<Target<'a>>) -> Result<Node> {
        let target = target.into();
        for _ in 0..40 {
            let node = self.settled(target)?;
            // A snapshot fails when a node leaves mid-walk; `wait` retries.
            let nodes = self.wait("the accessibility tree", |nodes| Some(nodes.to_vec()))?;
            let found = nodes.iter().position(|node| target.matches(node));
            let (Some(bounds), Some(view)) =
                (node.bounds, found.and_then(|index| viewport(&nodes, index)))
            else {
                return Ok(node);
            };
            let button = match (bounds.y < view.y, bounds.bottom() > view.bottom()) {
                (true, _) => Button::WheelUp,
                (false, true) => Button::WheelDown,
                (false, false) => return Ok(node),
            };
            let (x, y) = view.center();
            self.x11().scroll(x, y, button, 1)?;
        }
        bail!("{target:?} never scrolled fully into view")
    }

    /// Click the center of a node once it shows and has settled.
    pub fn click<'a>(&self, target: impl Into<Target<'a>>) -> Result<Node> {
        let node = self.reveal(target)?;
        self.click_node(&node)?;
        Ok(node)
    }

    /// Click a control that only shows, or only takes the press, while
    /// hovered: rest on it until hovering stops moving it, then press there.
    pub fn click_hovered(&self, what: &str, find: impl Fn(&[Node]) -> Option<Node>) -> Result<()> {
        for _ in 0..10 {
            let (x, y) = self
                .settled_by(what, &find)?
                .bounds
                .with_context(|| format!("{what} is off screen"))?
                .center();
            self.x11().move_to(x, y)?;
            std::thread::sleep(Duration::from_millis(300));
            let hovered = self
                .settled_by(what, &find)?
                .bounds
                .map(|bounds| bounds.center());
            if hovered == Some((x, y)) {
                self.x11().press(Button::Left)?;
                return self.x11().release(Button::Left);
            }
        }
        bail!("{what} kept moving under the pointer")
    }

    pub fn click_node(&self, node: &Node) -> Result<()> {
        let (x, y) = node
            .bounds
            .with_context(|| format!("{node:?} is off screen"))?
            .center();
        self.x11().click(x, y)
    }

    pub fn chord(&self, keys: &[Key]) -> Result<()> {
        self.x11().chord(keys)
    }

    /// The window-manager requests of one kind received so far.
    pub fn wm_requests(&self, kind: &str) -> Result<Vec<WmRequest>> {
        Ok(self
            .x11()
            .wm_requests()?
            .into_iter()
            .filter(|request| request.kind == kind)
            .collect())
    }

    /// Wait until the clipboard holds `text`.
    pub fn clipboard(&self, text: &str) -> Result<()> {
        self.until(&format!("clipboard {text:?}"), || {
            Ok(self.x11().clipboard()?.as_deref() == Some(text))
        })
    }

    /// Rest the pointer on a node, arriving in a few steps as a real pointer
    /// does, so the window sees it enter rather than appear.
    pub fn hover<'a>(&self, target: impl Into<Target<'a>>) -> Result<Node> {
        let node = self.reveal(target)?;
        let (x, y) = node.bounds.context("hovered node is off screen")?.center();
        for offset in [6, 3, 0] {
            self.x11().move_to(x - offset, y)?;
        }
        Ok(node)
    }

    /// Hover a node until its tooltip shows, nudging the pointer while it
    /// does not, since a busy window may miss the pointer's arrival.
    pub fn tooltip<'a>(&self, target: impl Into<Target<'a>>) -> Result<(Node, Node)> {
        let target = target.into();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let node = self.hover(target)?;
            let shown = Instant::now() + Duration::from_secs(3);
            while Instant::now() < shown {
                let tooltip = self.desktop.a11y.nodes().ok().and_then(|nodes| {
                    nodes
                        .into_iter()
                        .find(|node| node.role == Role::ToolTip && node.bounds.is_some())
                });
                if let Some(tooltip) = tooltip {
                    return Ok((node, tooltip));
                }
                std::thread::sleep(POLL);
            }
            if Instant::now() > deadline {
                bail!("{target:?} showed no tooltip");
            }
            self.x11().move_to(0, 0)?;
        }
    }

    /// Press the left button twice in quick succession at a point.
    pub fn double_click(&self, (x, y): (i32, i32)) -> Result<()> {
        self.x11().move_to(x, y)?;
        for _ in 0..2 {
            self.x11().press(Button::Left)?;
            self.x11().release(Button::Left)?;
        }
        Ok(())
    }

    /// Wait for the control panel to exit and report how it did.
    pub fn gui_exit(&mut self) -> Result<std::process::ExitStatus> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.gui.try_wait()? {
                return Ok(status);
            }
            if Instant::now() > deadline {
                bail!("the control panel kept running");
            }
            std::thread::sleep(POLL);
        }
    }

    pub fn gui_running(&mut self) -> Result<bool> {
        Ok(self.gui.try_wait()?.is_none())
    }

    /// Run `voice-input gui` once more in the same desktop and session, and
    /// collect its output once it exits; one that keeps running is killed.
    pub fn launch_again(&self) -> Result<std::process::Output> {
        let mut second = Command::new(env!("CARGO_BIN_EXE_voice-input"))
            .arg("gui")
            .env_clear()
            .envs(self.env.iter().cloned())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let deadline = Instant::now() + TIMEOUT;
        while second.try_wait()?.is_none() {
            if Instant::now() > deadline {
                let _ = second.kill();
                let _ = second.wait();
                bail!("the second launch kept running");
            }
            std::thread::sleep(POLL);
        }
        Ok(second.wait_with_output()?)
    }

    /// Read a GNOME setting the way the application does.
    pub fn gsettings(&self, args: &[&str]) -> Result<String> {
        self.desktop.gsettings(args)
    }

    fn save_evidence(&mut self) -> Result<()> {
        self.x11().screenshot(&self.artifacts.join("final.png"))?;
        let tree = self
            .desktop
            .a11y
            .nodes()
            .map_or_else(|error| format!("{error:#}"), |nodes| describe(&nodes));
        std::fs::write(self.artifacts.join("tree.txt"), tree)?;
        for (source, name) in [
            (self.paths.log_file.clone(), "daemon.log"),
            (self.desktop.root.join("launched.log"), "launched.log"),
        ] {
            if source.exists() {
                std::fs::copy(source, self.artifacts.join(name))?;
            }
        }
        Ok(())
    }

    /// The process serving the daemon socket now.
    pub fn daemon_pid(&self) -> Result<u32> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(voice_input_core::daemon_peer_pid(&self.paths))
    }

    /// Kill whatever daemon owns the socket now, including one the control
    /// panel started, which outlives it by design.
    fn stop_daemon(&self) -> Result<()> {
        if !self.paths.socket.exists() {
            return Ok(());
        }
        let pid = self.daemon_pid()?;
        // SAFETY: kill takes a PID read from the socket's peer credentials,
        // which belongs to this user, and no pointers.
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = self.save_evidence() {
            eprintln!("cannot save evidence: {error:#}");
        }
        let _ = self.gui.kill();
        let _ = self.gui.wait();
        if let Err(error) = self.stop_daemon() {
            eprintln!("cannot stop the daemon: {error:#}");
        }
        self.fake = None;
        let _ = std::fs::remove_dir_all(&self.desktop.root);
    }
}
