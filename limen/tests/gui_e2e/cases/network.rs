//! Network settings: the proxy requests to model services go through. One
//! manual proxy is saved through a real daemon onto disk: the config file, the
//! private file the password goes to, and the reloads the daemon is asked for
//! are what only the whole chain shows. The page itself, and the requests it
//! sends, are tested in-process in limen-gui's UI tests.

use std::os::unix::fs::PermissionsExt;

use anyhow::{Result, ensure};
use limen_core::{ApiKey, Config, Proxy, ProxyLogin, v1};

use super::{reloads, retype};
use crate::daemon::Scenario;
use crate::session::Session;

/// A manual proxy is saved with its login, the password beside the API
/// keys rather than in the config, and both roles go through it once the
/// daemon reloads.
pub fn a_manual_proxy_is_saved_and_applied() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("settings-page")?;
    session.click("settings-tab-network")?;
    session.named("current-page", "Network")?;
    session.absent("proxy-url")?;
    session.click("proxy-manual")?;
    retype(&session, "proxy-url", "socks5h://127.0.0.1:1080")?;
    retype(&session, "proxy-username", "me")?;
    session.click("proxy-password")?;
    session.x11().type_text("secret")?;
    retype(&session, "proxy-bypass", "localhost, .corp.example")?;
    session.click("save-network")?;
    session.named("toast", "Network settings applied")?;
    let config_file = &session.paths().config_file;
    let config = Config::load(config_file)?;
    let keys = config_file.with_file_name("keys.toml");
    let proxy = Proxy::Manual {
        url: "socks5h://127.0.0.1:1080".to_string(),
        login: Some(ProxyLogin {
            username: "me".to_string(),
            password: Some(ApiKey::new("secret".to_string())),
        }),
        bypass: "localhost, .corp.example".to_string(),
    };
    ensure!(
        (
            &config.provider.proxy,
            &config.polish.proxy,
            std::fs::metadata(&keys)?.permissions().mode() & 0o777,
            std::fs::read_to_string(config_file)?.contains("secret"),
        ) == (&proxy, &proxy, 0o600, false),
        "saved {:?} and {:?}",
        config.provider.proxy,
        config.polish.proxy
    );
    ensure!(
        (
            reloads(&session, &v1::Request::ProviderReload)?,
            reloads(&session, &v1::Request::PolishReload)?
        ) == (1, 1),
        "the daemon was not asked to reload each role once"
    );
    Ok(())
}
