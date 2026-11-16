//! What the model and network pages' tests share: the settings the bridge
//! reads from the config and key files, the pages' requests as they arrive
//! on the command channel, and the keyboard's stand-in for picking an item
//! from a select, whose list the test observer does not see.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use limen_core::{
    ConfigChange, KeyChange, KeySource, ModelRole, Network, Paths, Provider, ProxyPassword,
    load_network, load_provider,
};

use super::Ui;
use crate::bridge::{BridgeCommand, BridgeEvent, ModelRequest};

/// How long the page waits, on the wall clock, after an edit to the server's
/// address or key before it asks that server for its models.
const DISCOVERY_WAIT: Duration = Duration::from_millis(700);

/// Run `read` on a directory holding the config and key files with this
/// text, as the bridge reads them; a file whose text is empty is left out.
fn read_files<T>(config: &str, keys: &str, read: impl FnOnce(&Paths) -> T) -> T {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "limen-ui-tests-{}-files-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).expect("create the settings directory");
    for (name, text) in [("config.toml", config), ("keys.toml", keys)] {
        match text {
            "" => {}
            text => std::fs::write(root.join(name), text).expect("write the settings file"),
        }
    }
    let paths = Paths {
        config_file: root.join("config.toml"),
        socket: root.join("limen.sock"),
        log_file: root.join("daemon.log"),
        cache_dir: root.join("cache"),
    };
    let read = read(&paths);
    let _ = std::fs::remove_dir_all(&root);
    read
}

/// Both roles' settings as the bridge reads them from the files.
#[derive(Clone)]
pub(super) struct Settings {
    asr: (Provider, KeySource),
    polish: (Provider, KeySource),
}

impl Default for Settings {
    /// What a first run reads: no config and no keys.
    fn default() -> Self {
        Self::read("", "")
    }
}

impl Settings {
    /// The settings `config` and `keys`, the text of the two files, hold.
    pub(super) fn read(config: &str, keys: &str) -> Self {
        read_files(config, keys, |paths| Self {
            asr: load_provider(paths, ModelRole::Asr).expect("read the ASR settings"),
            polish: load_provider(paths, ModelRole::Polish).expect("read the polish settings"),
        })
    }

    fn of(&self, role: ModelRole) -> (Provider, KeySource) {
        match role {
            ModelRole::Asr => self.asr.clone(),
            ModelRole::Polish => self.polish.clone(),
        }
    }
}

/// The proxy settings `config` and `keys` hold, as the bridge reads them.
pub(super) fn read_network(config: &str, keys: &str) -> (Network, ProxyPassword) {
    read_files(config, keys, |paths| {
        load_network(paths).expect("read the network settings")
    })
}

/// The reads of settings among `sent`: the role and ID of each.
pub(super) fn loads(sent: &[BridgeCommand]) -> Vec<(ModelRole, u64)> {
    sent.iter()
        .filter_map(|command| match command {
            BridgeCommand::Model(ModelRequest::LoadProvider { role, id }) => Some((*role, *id)),
            _ => None,
        })
        .collect()
}

/// Answer the reads of settings among `sent` as the bridge does.
pub(super) fn answer_loads(ui: &mut Ui, sent: &[BridgeCommand], settings: &Settings) {
    for (role, id) in loads(sent) {
        ui.receive(BridgeEvent::ProviderLoaded {
            role,
            id,
            result: Ok(settings.of(role)),
        });
    }
}

/// Open the Providers page and answer its reads of settings from
/// `settings`; returns the requests the page sent.
pub(super) fn open_providers(ui: &mut Ui, settings: &Settings) -> Vec<BridgeCommand> {
    ui.click("providers-page");
    let sent = ui.sent();
    answer_loads(ui, &sent, settings);
    sent
}

/// The saves among `sent`: the ID, the provider, and the key change of each.
pub(super) fn saves(sent: &[BridgeCommand]) -> Vec<(u64, Provider, KeyChange)> {
    sent.iter()
        .filter_map(|command| match command {
            BridgeCommand::Model(ModelRequest::SaveProvider { id, provider, key }) => {
                Some((*id, provider.clone(), key.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The one save among `sent`; panics unless there is exactly one.
pub(super) fn save_of(sent: &[BridgeCommand]) -> (u64, Provider, KeyChange) {
    match <[_; 1]>::try_from(saves(sent)) {
        Ok([save]) => save,
        Err(saves) => panic!("expected one save, got {saves:?} in {sent:?}"),
    }
}

/// How the bridge answers a save.
pub(super) fn save_replied(
    ui: &mut Ui,
    role: ModelRole,
    id: u64,
    result: Result<ConfigChange, String>,
) {
    ui.receive(BridgeEvent::ProviderSaved { role, id, result });
}

/// The requests among `sent` for the models a server lists: the role, the ID
/// the reply carries, the provider the fields name, and the key typed.
///
/// The page asks again whenever the fields it names change, such as once
/// for the server and again for the key typed after it; the last request is
/// the one for the fields as they stand.
pub(super) fn lists(sent: &[BridgeCommand]) -> Vec<(ModelRole, u64, Provider, KeyChange)> {
    sent.iter()
        .filter_map(|command| match command {
            BridgeCommand::Model(ModelRequest::ListModels {
                role,
                request,
                provider,
                key,
            }) => Some((*role, *request, provider.clone(), key.clone())),
            _ => None,
        })
        .collect()
}

/// The last request among `sent` for the models a server lists; panics when
/// there is none.
pub(super) fn last_list(sent: &[BridgeCommand]) -> (ModelRole, u64, Provider, KeyChange) {
    lists(sent)
        .pop()
        .unwrap_or_else(|| panic!("no request for models in {sent:?}"))
}

/// The one request among `sent` to try a model: the role, the ID the reply
/// carries, the provider the fields name, and the key typed. Panics unless
/// there is exactly one.
pub(super) fn check_of(sent: &[BridgeCommand]) -> (ModelRole, u64, Provider, KeyChange) {
    let checks: Vec<_> = sent
        .iter()
        .filter_map(|command| match command {
            BridgeCommand::Model(ModelRequest::CheckModel {
                role,
                id,
                provider,
                key,
            }) => Some((*role, *id, provider.clone(), key.clone())),
            _ => None,
        })
        .collect();
    match <[_; 1]>::try_from(checks) {
        Ok([check]) => check,
        Err(checks) => panic!("expected one model check, got {checks:?} in {sent:?}"),
    }
}

/// Let the fields stand unedited for as long as the page waits before it
/// asks a server for its models, draw the frame that asks, and return what
/// the page sent. The wait is on the wall clock, which the test clock does
/// not move, so a test that types into the fields and does not follow the
/// listing reads its saves apart from these requests.
pub(super) fn after_discovery_wait(ui: &mut Ui) -> Vec<BridgeCommand> {
    std::thread::sleep(DISCOVERY_WAIT);
    ui.find("current-page");
    ui.sent()
}

/// Open `select`, press `keys` to move its cursor, and confirm the item it
/// rests on: the items of a select are not observed, so the keyboard stands
/// in for a click on one.
pub(super) fn choose(ui: &mut Ui, select: &str, keys: &[&str]) {
    ui.click(select);
    for key in keys {
        ui.press(key);
    }
    ui.press("enter");
}

/// Type `model` into the search of the select `{prefix}-model` and choose
/// the first match, which is `model` itself whether or not the server listed
/// it. With no model chosen yet the list's cursor rests on none, so it is
/// moved onto the match before confirming.
pub(super) fn type_model(ui: &mut Ui, prefix: &str, model: &str) {
    ui.click(&format!("{prefix}-model"));
    ui.input(model);
    ui.press("down");
    ui.press("enter");
}
