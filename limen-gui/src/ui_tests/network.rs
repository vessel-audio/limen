//! Network settings: the proxy requests to model services go through. The
//! Linux desktop suite keeps one manual proxy saved through a daemon onto
//! disk, which is the bridge's and the core's to do.

use gpui_kit::TestAppContext;
use limen_core::{ApiKey, ConfigChange, KeyChange, Network, ProxyMode};
use rust_i18n::t;

use super::model_support::read_network;
use super::{Daemon, Ui};
use crate::bridge::{BridgeCommand, BridgeEvent, NetworkRequest};

/// Open the Network page and answer its read of the settings as the bridge
/// does from the files with this text; returns the requests the page sent.
fn open_network(ui: &mut Ui, config: &str, keys: &str) -> Vec<BridgeCommand> {
    ui.click("settings-page");
    ui.click("settings-tab-network");
    let sent = ui.sent();
    let loads = sent.iter().filter_map(|command| match command {
        BridgeCommand::Network(NetworkRequest::Load { id }) => Some(*id),
        _ => None,
    });
    for id in loads {
        ui.receive(BridgeEvent::NetworkLoaded {
            id,
            result: Ok(read_network(config, keys)),
        });
    }
    sent
}

/// A manual proxy, its login, and the hosts it skips are saved from the page
/// together, the password apart from the rest; once the bridge applies them
/// a toast says so and the page reads its settings anew.
#[gpui_kit::test]
fn a_manual_proxy_is_saved_from_its_page(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    let opened = open_network(&mut ui, "", "");
    let page = (ui.label("current-page"), ui.try_find("proxy-url").is_some());
    ui.click("proxy-manual");
    ui.retype("proxy-url", "socks5h://127.0.0.1:1080");
    ui.retype("proxy-username", "me");
    ui.click("proxy-password");
    ui.input("secret");
    ui.retype("proxy-bypass", "localhost, .corp.example");
    ui.click("save-network");
    let saved = ui.sent();
    ui.receive(BridgeEvent::NetworkSaved {
        id: 2,
        result: Ok(ConfigChange::Applied),
    });
    assert_eq!(
        (opened, page, saved, ui.label("toast"), ui.sent()),
        (
            vec![BridgeCommand::Network(NetworkRequest::Load { id: 1 })],
            (t!("settings.network").into(), false),
            vec![BridgeCommand::Network(NetworkRequest::Save {
                id: 2,
                network: Network {
                    mode: ProxyMode::Manual,
                    url: "socks5h://127.0.0.1:1080".to_string(),
                    username: "me".to_string(),
                    bypass: "localhost, .corp.example".to_string(),
                },
                password: KeyChange::Set(ApiKey::new("secret".to_string())),
            })],
            t!("toast.network_applied").into(),
            vec![BridgeCommand::Network(NetworkRequest::Load { id: 3 })],
        )
    );
}

/// A proxy URL holding a login is sent as typed, for the bridge to judge; its
/// refusal shows on the page with the edits kept, ready to save again.
#[gpui_kit::test]
fn a_proxy_url_with_a_login_is_refused(cx: &mut TestAppContext) {
    const REFUSED: &str = "network.proxy_url cannot hold a login: set network.proxy_username \
                           and save the password from the GUI";
    let mut ui = Ui::connected(cx, Daemon::default());
    open_network(&mut ui, "", "");
    ui.click("proxy-manual");
    ui.retype("proxy-url", "http://me:secret@proxy:8080");
    ui.click("save-network");
    let saved = ui.sent();
    ui.receive(BridgeEvent::NetworkSaved {
        id: 2,
        result: Err(REFUSED.to_string()),
    });
    let (loaded, _) = read_network("", "");
    assert_eq!(
        (
            saved,
            ui.label("network-error"),
            ui.find("save-network").disabled(),
            ui.try_find("toast").is_some()
        ),
        (
            vec![BridgeCommand::Network(NetworkRequest::Save {
                id: 2,
                network: Network {
                    mode: ProxyMode::Manual,
                    url: "http://me:secret@proxy:8080".to_string(),
                    ..loaded
                },
                password: KeyChange::Keep,
            })],
            REFUSED.to_string(),
            None,
            false,
        )
    );
}
