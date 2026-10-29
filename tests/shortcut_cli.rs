//! Command-level coverage for `voice-input shortcut`, using the real binary
//! in an isolated HOME. No GNOME settings, portal, or user socket is touched.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::Output;
use std::sync::{Arc, Mutex};

const ENTRY_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const OUR_PATH: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/voice-input-toggle/";

/// A fake `gsettings` backed by files in the sandbox's state directory.
const GSETTINGS: &str = r##"#!/bin/sh
set -eu
state="$FAKE_GSETTINGS_STATE"
command="$1"
schema="${2:-}"
key="${3:-}"
value="${4:-}"
entry_dir() {
    printf '%s' "$1" | sed 's#[/:]#_#g'
}
case "$command" in
    get)
        if [ "$schema" = "org.gnome.settings-daemon.plugins.media-keys" ] && [ "$key" = "custom-keybindings" ]; then
            cat "$state/list"
        else
            dir="$state/entries/$(entry_dir "$schema")"
            cat "$dir/$key" 2>/dev/null || printf "''\n"
        fi
        ;;
    set)
        if [ "$schema" = "org.gnome.settings-daemon.plugins.media-keys" ] && [ "$key" = "custom-keybindings" ]; then
            printf '%s\n' "$value" > "$state/list"
        else
            dir="$state/entries/$(entry_dir "$schema")"
            mkdir -p "$dir"
            printf '%s\n' "$value" > "$dir/$key"
        fi
        printf 'set %s %s %s\n' "$schema" "$key" "$value" >> "$state/writes"
        ;;
    reset)
        dir="$state/entries/$(entry_dir "$schema")"
        rm -f "$dir/$key"
        printf 'reset %s %s\n' "$schema" "$key" >> "$state/writes"
        ;;
    list-schemas)
        printf '%s\n' \
            org.gnome.desktop.wm.keybindings \
            org.gnome.shell.keybindings \
            org.gnome.mutter.keybindings \
            org.gnome.mutter.wayland.keybindings \
            org.gnome.settings-daemon.plugins.media-keys
        ;;
    list-recursively)
        case "$schema" in
            org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:*)
                dir="$state/entries/$(entry_dir "$schema")"
                for field in name command binding; do
                    contents=$(cat "$dir/$field" 2>/dev/null || printf "''")
                    printf '%s %s %s\n' \
                        org.gnome.settings-daemon.plugins.media-keys.custom-keybinding \
                        "$field" "$contents"
                done
                ;;
            *)
                ;;
        esac
        ;;
    *)
        printf 'unexpected fake gsettings call: %s\n' "$*" >&2
        exit 1
        ;;
esac
"##;

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "voice-input-cli-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for dir in ["config/voice-input", "run", "data", "bin", "state/entries"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("state/list"), "@as []\n").unwrap();
        let gsettings = root.join("bin/gsettings");
        fs::write(&gsettings, GSETTINGS).unwrap();
        fs::set_permissions(&gsettings, fs::Permissions::from_mode(0o755)).unwrap();
        Self { root }
    }

    fn run(&self, desktop: &str, args: &[&str]) -> Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_voice-input"))
            .args(args)
            .env_clear()
            .env("HOME", &self.root)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_DATA_DIRS", self.root.join("system-data"))
            .env("XDG_CURRENT_DESKTOP", desktop)
            .env("FAKE_GSETTINGS_STATE", self.root.join("state"))
            .output()
            .unwrap()
    }

    fn write_config(&self, backend: &str) {
        fs::write(
            self.root.join("config/voice-input/config.toml"),
            format!(
                "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n\n[shortcut]\nbackend = \"{backend}\"\ntoggle = \"Super+Z\"\nhold = \"Super+X\"\n"
            ),
        )
        .unwrap();
    }

    fn config(&self) -> String {
        fs::read_to_string(self.root.join("config/voice-input/config.toml")).unwrap()
    }

    fn list(&self) -> String {
        fs::read_to_string(self.root.join("state/list")).unwrap()
    }

    fn entry(&self, path: &str, field: &str) -> String {
        let dir = format!("{ENTRY_SCHEMA}:{path}").replace(['/', ':'], "_");
        fs::read_to_string(self.root.join("state/entries").join(dir).join(field)).unwrap()
    }

    fn seed_other_entry(&self, id: &str, binding: &str) {
        let path =
            format!("/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/{id}/");
        let dir = format!("{ENTRY_SCHEMA}:{path}").replace(['/', ':'], "_");
        let dir = self.root.join("state/entries").join(dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("name"), "'WeChat'\n").unwrap();
        fs::write(dir.join("command"), "'/bin/sh -c screenshot'\n").unwrap();
        fs::write(dir.join("binding"), format!("'{binding}'\n")).unwrap();
    }
}

fn strings(output: &Output) -> (i32, String, String) {
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn seed_wechat(sandbox: &Sandbox) -> String {
    sandbox.seed_other_entry("wechat1", "<Control><Alt>w");
    sandbox.seed_other_entry("wechat2", "<Alt>a");
    let list = "['/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/wechat1/', '/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/wechat2/']\n".to_string();
    fs::write(sandbox.root.join("state/list"), &list).unwrap();
    list
}

/// A small IPC server answering one rebind and one status request.
fn fake_daemon(sandbox: &Sandbox, status: String) -> Arc<Mutex<Vec<String>>> {
    let socket = sandbox.root.join("run/voice-input.sock");
    let listener = UnixListener::bind(socket).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    std::thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            use std::io::{BufRead, Write};
            let mut stream = stream.unwrap();
            let mut request = String::new();
            std::io::BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            recorded.lock().unwrap().push(request.clone());
            let reply = match request.contains("shortcut_rebind") {
                true => r#"{"type":"ok"}"#.to_string(),
                false => status.clone(),
            };
            stream.write_all(format!("{reply}\n").as_bytes()).unwrap();
        }
    });
    requests
}

#[test]
fn status_none_and_invalid_keys_follow_the_exit_contract() {
    let sandbox = Sandbox::new("status-none");
    let (code, stdout, stderr) = strings(&sandbox.run("KDE", &["shortcut", "status"]));
    assert_eq!((code, stdout, stderr), (
        0,
        "backend:  none\ntoggle:   bind `voice-input toggle` to a key in your desktop settings\nhold:     not supported by this backend; for hold-to-talk run `voice-input shortcut setup portal`\n".to_string(),
        String::new(),
    ));

    let (code, _, stderr) = strings(&sandbox.run("KDE", &["shortcut", "set", "toggle", "Shift+A"]));
    assert_eq!(code, 2);
    assert!(
        stderr.contains("Shift+A would get in the way of typing"),
        "{stderr}"
    );
}

#[test]
fn gnome_setup_is_repeatable_and_preserves_other_entries_without_a_daemon() {
    let sandbox = Sandbox::new("gnome-repeat");
    let before = seed_wechat(&sandbox);
    let (code, stdout, stderr) = strings(&sandbox.run(
        "GNOME",
        &[
            "shortcut",
            "setup",
            "gnome-keybinding",
            "--toggle",
            "Super+Z",
        ],
    ));
    assert_eq!((code, stderr), (0, String::new()), "{stdout}");
    assert!(stdout.contains("The daemon is not running"), "{stdout}");
    assert!(sandbox.config().contains("backend = \"gnome-keybinding\""));
    assert_eq!(
        sandbox.list(),
        before.replace("]", &format!(", '{OUR_PATH}']"))
    );
    assert_eq!(sandbox.entry(OUR_PATH, "binding"), "'<Super>z'\n");
    assert!(
        sandbox
            .entry(OUR_PATH, "command")
            .ends_with("voice-input toggle'\n")
    );

    let once = sandbox.list();
    let (code, _, _) = strings(&sandbox.run("GNOME", &["shortcut", "setup", "gnome-keybinding"]));
    assert_eq!(code, 0);
    assert_eq!(sandbox.list(), once);
}

#[test]
fn gnome_setup_refuses_a_symlinked_config_before_changing_dconf() {
    let sandbox = Sandbox::new("gnome-symlink");
    let before = seed_wechat(&sandbox);
    let managed = sandbox.root.join("managed.toml");
    fs::write(
        &managed,
        "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n",
    )
    .unwrap();
    symlink(
        &managed,
        sandbox.root.join("config/voice-input/config.toml"),
    )
    .unwrap();

    let (code, _, stderr) = strings(&sandbox.run(
        "GNOME",
        &[
            "shortcut",
            "setup",
            "gnome-keybinding",
            "--toggle",
            "Super+Z",
        ],
    ));
    assert_eq!(code, 1);
    assert!(
        stderr.contains("Add this to the config instead"),
        "{stderr}"
    );
    assert_eq!(sandbox.list(), before);
    assert!(!sandbox.root.join("state/writes").exists());
}

#[test]
fn portal_setup_without_a_daemon_defers_and_installs_the_entry() {
    let sandbox = Sandbox::new("portal-no-daemon");
    let (code, stdout, stderr) = strings(&sandbox.run("KDE", &["shortcut", "setup", "portal"]));
    assert_eq!((code, stderr), (0, String::new()), "{stdout}");
    assert!(stdout.contains("The daemon is not running"), "{stdout}");
    assert!(sandbox.config().contains("backend = \"portal\""));
    let entry = fs::read_to_string(
        sandbox
            .root
            .join("data/applications/io.github.Emin017.VoiceInput.desktop"),
    )
    .unwrap();
    assert!(entry.contains(&format!(
        "Exec={} daemon",
        env!("CARGO_BIN_EXE_voice-input")
    )));
}

#[test]
fn portal_set_without_a_daemon_defers_without_claiming_confirmation() {
    let sandbox = Sandbox::new("portal-set-no-daemon");
    sandbox.write_config("portal");
    let (code, stdout, stderr) =
        strings(&sandbox.run("KDE", &["shortcut", "set", "toggle", "Super+Y"]));
    assert_eq!(
        (code, stdout, stderr),
        (
            0,
            "The daemon is not running; the change takes effect when it starts.\n".to_string(),
            String::new(),
        )
    );
}

#[test]
fn portal_setup_reports_a_cancelled_dialog_from_the_daemon() {
    let sandbox = Sandbox::new("portal-cancelled");
    let status = r#"{"type":"shortcut_status","backend":"portal","session":"cancelled","preferred":{"toggle":"Super+Z","hold":"Super+X"},"actual":{"toggle":null,"hold":null},"last_error":null}"#.to_string();
    let requests = fake_daemon(&sandbox, status);
    let (code, _, stderr) = strings(&sandbox.run("KDE", &["shortcut", "setup", "portal"]));
    assert_eq!(code, 1);
    assert!(
        stderr.contains("the GNOME dialog was cancelled"),
        "{stderr}"
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("shortcut_rebind"));
    assert!(requests[1].contains("shortcut_status"));
}

#[test]
fn portal_set_reports_the_binding_gnome_kept() {
    let sandbox = Sandbox::new("portal-kept");
    sandbox.write_config("portal");
    let status = r#"{"type":"shortcut_status","backend":"portal","session":"bound","preferred":{"toggle":"Super+Y","hold":"Super+X"},"actual":{"toggle":{"description":"Press <Super>z","shortcuts":["Super+Z"]},"hold":{"description":"Press <Super>x","shortcuts":["Super+X"]}},"last_error":null}"#.to_string();
    let _requests = fake_daemon(&sandbox, status);
    let (code, _, stderr) = strings(&sandbox.run("KDE", &["shortcut", "set", "toggle", "Super+Y"]));
    assert_eq!(code, 1);
    assert!(
        stderr.contains("GNOME kept the saved binding Super+Z"),
        "{stderr}"
    );
    assert!(sandbox.config().contains("toggle = \"Super+Y\""));
}

#[test]
fn gnome_conflict_names_the_other_custom_entry() {
    let sandbox = Sandbox::new("gnome-conflict");
    seed_wechat(&sandbox);
    let (code, _, stderr) = strings(&sandbox.run(
        "GNOME",
        &[
            "shortcut",
            "setup",
            "gnome-keybinding",
            "--toggle",
            "Ctrl+Alt+W",
        ],
    ));
    assert_eq!(code, 1);
    assert!(
        stderr.contains("Ctrl+Alt+W is already used by the custom shortcut \"WeChat\""),
        "{stderr}"
    );
    assert!(!sandbox.root.join("state/writes").exists());
}
