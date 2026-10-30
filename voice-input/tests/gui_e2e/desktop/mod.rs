//! A private desktop: an Xvfb display, session and accessibility buses, the
//! AT-SPI registry, and isolated XDG directories, torn down in reverse
//! order of startup.

mod a11y;
mod portal;
mod x11;

pub use a11y::{A11y, Bounds, Node, describe, viewport};
pub use x11::{Button, Key, WmRequest, X11};

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// Programs the application may start, replaced by recorders.
const SHIMS: [&str; 2] = ["xdg-open", "gnome-control-center"];

/// The GNOME schemas the GNOME custom-shortcut backend writes, reduced to
/// the keys it uses; real `gsettings` stores them in a keyfile.
const SCHEMAS: &str = r#"<schemalist>
  <schema id="org.gnome.settings-daemon.plugins.media-keys"
          path="/org/gnome/settings-daemon/plugins/media-keys/">
    <key name="custom-keybindings" type="as"><default>[]</default></key>
  </schema>
  <schema id="org.gnome.settings-daemon.plugins.media-keys.custom-keybinding">
    <key name="name" type="s"><default>''</default></key>
    <key name="command" type="s"><default>''</default></key>
    <key name="binding" type="s"><default>''</default></key>
  </schema>
</schemalist>
"#;

/// The color scheme the desktop reports through its settings portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemScheme {
    /// No portal answers, as on a desktop without one.
    Unset,
    PreferDark,
}

/// Environment variables the dev shell sets for the suite.
const REGISTRYD: &str = "VOICE_INPUT_E2E_REGISTRYD";
const VULKAN_ICD: &str = "VOICE_INPUT_E2E_VK_ICD";
const FONTCONFIG: &str = "VOICE_INPUT_E2E_FONTCONFIG";

pub struct Desktop {
    pub root: PathBuf,
    display: String,
    session_bus: String,
    a11y_bus: String,
    _processes: Processes,
    _status: zbus::blocking::Connection,
    _portal: Option<zbus::blocking::Connection>,
    pub x11: X11,
    pub a11y: A11y,
}

/// The `org.a11y.Bus` service an AT-SPI bus launcher normally provides.
struct A11yBus {
    address: String,
}

#[zbus::interface(name = "org.a11y.Bus")]
impl A11yBus {
    fn get_address(&self) -> String {
        self.address.clone()
    }
}

/// Reports assistive technology as running, which activates AccessKit.
struct A11yStatus;

#[zbus::interface(name = "org.a11y.Status")]
impl A11yStatus {
    #[zbus(property)]
    fn is_enabled(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn screen_reader_enabled(&self) -> bool {
        true
    }
}

impl Desktop {
    pub fn start(root: &Path, scheme: SystemScheme) -> Result<Self> {
        let registryd = required(REGISTRYD)?;
        for dir in [
            "home", "config", "data", "state", "cache", "run", "bin", "bus", "schemas",
        ] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        std::fs::write(root.join("schemas/e2e.gschema.xml"), SCHEMAS)?;
        let compiled = Command::new("glib-compile-schemas")
            .arg(root.join("schemas"))
            .status()
            .context("cannot run glib-compile-schemas")?;
        if !compiled.success() {
            bail!("glib-compile-schemas failed: {compiled}");
        }
        std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700))?;
        write_shims(root)?;
        let mut processes = Processes(Vec::new());

        let (display, xvfb) = start_xvfb(&root.join("xvfb.log"))?;
        processes.0.push(("Xvfb", xvfb));
        let config = root.join("bus").join("bus.conf");
        std::fs::write(&config, bus_config(&root.join("bus")))?;
        let (session_bus, session) = start_bus(&config)?;
        processes.0.push(("session bus", session));
        let (a11y_bus, accessibility) = start_bus(&config)?;
        processes.0.push(("accessibility bus", accessibility));
        processes.0.push((
            "at-spi2-registryd",
            Command::new(registryd)
                .env_clear()
                .env("DISPLAY", &display)
                .env("DBUS_SESSION_BUS_ADDRESS", &session_bus)
                .env("AT_SPI_BUS_ADDRESS", &a11y_bus)
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(root.join("registryd.log"))?)
                .spawn()
                .context("cannot start at-spi2-registryd")?,
        ));
        let status = zbus::blocking::connection::Builder::address(session_bus.as_str())?
            .name("org.a11y.Bus")?
            .serve_at(
                "/org/a11y/bus",
                A11yBus {
                    address: a11y_bus.clone(),
                },
            )?
            .serve_at("/org/a11y/bus", A11yStatus)?
            .build()
            .context("cannot serve org.a11y.Bus")?;
        let portal = match scheme {
            SystemScheme::Unset => None,
            SystemScheme::PreferDark => Some(
                zbus::blocking::connection::Builder::address(session_bus.as_str())?
                    .name("org.freedesktop.portal.Desktop")?
                    .serve_at("/org/freedesktop/portal/desktop", portal::DarkSettings)?
                    .build()
                    .context("cannot serve the settings portal")?,
            ),
        };
        let a11y = A11y::connect(&a11y_bus)?;
        wait_for_name(&a11y_bus, "org.a11y.atspi.Registry")?;
        let x11 = X11::connect(&display)?;
        Ok(Self {
            root: root.to_path_buf(),
            display,
            session_bus,
            a11y_bus,
            _processes: processes,
            _status: status,
            _portal: portal,
            x11,
            a11y,
        })
    }

    /// The complete environment of the application and anything it starts.
    pub fn env(&self) -> Result<Vec<(&'static str, OsString)>> {
        let dir = |name: &str| self.root.join(name).into_os_string();
        let path = std::iter::once(self.root.join("bin"))
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ))
            .collect::<Vec<_>>();
        Ok(vec![
            ("DISPLAY", self.display.clone().into()),
            ("DBUS_SESSION_BUS_ADDRESS", self.session_bus.clone().into()),
            ("AT_SPI_BUS_ADDRESS", self.a11y_bus.clone().into()),
            ("HOME", dir("home")),
            ("XDG_CONFIG_HOME", dir("config")),
            ("XDG_DATA_HOME", dir("data")),
            ("XDG_STATE_HOME", dir("state")),
            ("XDG_CACHE_HOME", dir("cache")),
            ("XDG_RUNTIME_DIR", dir("run")),
            ("PATH", std::env::join_paths(path)?),
            ("LANG", "en_US.UTF-8".into()),
            // The GNOME custom-shortcut backend runs only inside GNOME.
            ("XDG_CURRENT_DESKTOP", "GNOME".into()),
            ("GSETTINGS_BACKEND", "keyfile".into()),
            ("GSETTINGS_SCHEMA_DIR", dir("schemas")),
            ("GPUI_X11_SCALE_FACTOR", "1".into()),
            ("VK_ICD_FILENAMES", required(VULKAN_ICD)?),
            ("FONTCONFIG_FILE", required(FONTCONFIG)?),
            (
                "LD_LIBRARY_PATH",
                std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
            ),
            (
                "RUST_LOG",
                std::env::var_os("RUST_LOG").unwrap_or_else(|| "warn".into()),
            ),
        ])
    }

    /// Read a GNOME setting through the same `gsettings` the application uses.
    pub fn gsettings(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("gsettings")
            .args(args)
            .env_clear()
            .envs(self.env()?)
            .output()
            .context("cannot run gsettings")?;
        match output.status.success() {
            true => Ok(String::from_utf8(output.stdout)?.trim_end().to_string()),
            false => bail!(
                "gsettings {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ),
        }
    }

    /// Invocations of the shimmed programs, one argument vector per line.
    pub fn launched(&self) -> Result<Vec<Vec<String>>> {
        match std::fs::read_to_string(self.root.join("launched.log")) {
            Ok(text) => Ok(text
                .lines()
                .map(|line| line.split('\t').map(str::to_string).collect())
                .collect()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }
}

/// Children killed and reaped in reverse order of startup when dropped, so a
/// desktop that fails halfway through starting leaves nothing running.
struct Processes(Vec<(&'static str, Child)>);

impl Drop for Processes {
    fn drop(&mut self) {
        for (_, process) in self.0.iter_mut().rev() {
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

fn required(name: &str) -> Result<OsString> {
    std::env::var_os(name)
        .with_context(|| format!("{name} is unset; run the suite inside `nix develop`"))
}

/// Start Xvfb on the first free display, which it reports through a pipe.
fn start_xvfb(log: &Path) -> Result<(String, Child)> {
    let (mut reader, writer) = std::io::pipe()?;
    let fd = writer.as_raw_fd();
    let mut command = Command::new("Xvfb");
    command
        .args(["-displayfd", &fd.to_string()])
        .args([
            "-screen",
            "0",
            "1280x800x24",
            "-nolisten",
            "tcp",
            "-noreset",
        ])
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(log)?);
    // SAFETY: the closure runs in the forked child before exec and only calls
    // fcntl on a descriptor this process owns, which is async-signal-safe.
    unsafe {
        command.pre_exec(move || match libc::fcntl(fd, libc::F_SETFD, 0) {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    let child = command.spawn().context("cannot start Xvfb")?;
    drop(writer);
    let mut number = String::new();
    reader.read_to_string(&mut number)?;
    match number.trim() {
        "" => bail!("Xvfb exited before choosing a display"),
        number => Ok((format!(":{number}"), child)),
    }
}

/// A bus that activates nothing, so no user service leaks into a case.
fn bus_config(dir: &Path) -> String {
    format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:dir={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
        dir.display()
    )
}

fn start_bus(config: &Path) -> Result<(String, Child)> {
    let mut child = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--nopidfile", "--print-address"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("cannot start dbus-daemon")?;
    let mut address = String::new();
    BufReader::new(child.stdout.take().context("no dbus-daemon stdout")?)
        .read_line(&mut address)?;
    match address.trim() {
        "" => bail!("dbus-daemon exited before printing its address"),
        address => Ok((address.to_string(), child)),
    }
}

fn wait_for_name(address: &str, name: &str) -> Result<()> {
    let connection = zbus::blocking::connection::Builder::address(address)?.build()?;
    let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !bus.name_has_owner(name.try_into()?)? {
        match Instant::now() > deadline {
            true => bail!("{name} did not appear on {address}"),
            false => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    Ok(())
}

/// Shell scripts that record their argument vector and succeed.
fn write_shims(root: &Path) -> Result<()> {
    let log = root.join("launched.log");
    for name in SHIMS {
        let path = root.join("bin").join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n{{ printf '%s' {name}; for arg in \"$@\"; do printf '\\t%s' \"$arg\"; done; printf '\\n'; }} >> '{}'\n",
                log.display()
            ),
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}
