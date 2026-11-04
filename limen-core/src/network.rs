//! The proxy as the GUI edits it: which one requests to model services go
//! through, and who signs in to it.

use anyhow::{Context, Result};

use crate::config::{self, ApiKey, Config, Key, KeyChange, Paths, Proxy, ProxyMode, Value};
use crate::ipc::v1;
use crate::reload::{ConfigChange, reload};

/// The `[network]` section as the GUI edits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub mode: ProxyMode,
    /// The manual proxy's URL, kept while another mode is chosen.
    pub url: String,
    /// Who signs in to the proxy; empty for one without a login.
    pub username: String,
    /// The hosts a manual proxy is not used for, as `NO_PROXY` lists them.
    pub bypass: String,
}

/// Whether the proxy's password is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyPassword {
    Saved,
    Missing,
}

/// The proxy settings as saved, read without validating the config so that
/// a broken one can be shown and repaired. A missing config reads as the
/// annotated default.
pub fn load_network(paths: &Paths) -> Result<(Network, ProxyPassword)> {
    let _lock = config::lock_dir(&paths.config_file)?;
    config::recover_provider_save(&paths.config_file)?;
    let network = Config::read_unchecked(&paths.config_file)?.network;
    let password = match config::saved_keys(&paths.config_file)?.proxy {
        Some(_) => ProxyPassword::Saved,
        None => ProxyPassword::Missing,
    };
    Ok((
        Network {
            mode: network.proxy,
            url: network.proxy_url,
            username: network.proxy_username.unwrap_or_default(),
            bypass: network.no_proxy,
        },
        password,
    ))
}

/// Save the proxy settings and apply `password` to the proxy's password,
/// which a proxy without a username does not keep. A running daemon sends
/// both roles' requests through the proxy from the next one on.
pub async fn save_network(
    paths: &Paths,
    network: Network,
    password: KeyChange,
) -> Result<ConfigChange> {
    let username = network.username.trim().to_string();
    let text = |text: &str| Value::Text(text.trim().to_string());
    let values = [
        (
            Key::NetworkProxy,
            Value::Text(network.mode.as_str().to_string()),
        ),
        (Key::NetworkProxyUrl, text(&network.url)),
        (
            Key::NetworkProxyUsername,
            match username.is_empty() {
                true => Value::Unset,
                false => Value::Text(username.clone()),
            },
        ),
        (Key::NetworkNoProxy, text(&network.bypass)),
    ];
    let file = paths.config_file.clone();
    // The writes wait on a file lock; keep them off the caller's runtime thread.
    tokio::task::spawn_blocking(move || {
        // Clearing a password that is not saved would still write the key
        // file, which a managed one refuses.
        let saved = config::saved_keys(&file)?.proxy.is_some();
        let password = match (username.is_empty(), saved, password) {
            (true, true, _) => KeyChange::Clear,
            (true, false, _) => KeyChange::Keep,
            (false, _, change) => change,
        };
        config::update(&file, &values, config::Secret::ProxyPassword, &password)
    })
    .await
    .context("the config writer stopped")??;
    let asr = reload(paths, &v1::Request::ProviderReload).await?;
    let polish = reload(paths, &v1::Request::PolishReload).await?;
    Ok(match (asr, polish) {
        (ConfigChange::Applied, ConfigChange::Applied) => ConfigChange::Applied,
        (ConfigChange::Saved, _) | (_, ConfigChange::Saved) => ConfigChange::Saved,
    })
}

/// The proxy the config and the saved password name, for requests no
/// role's connection carries, such as a subscription's sign-in. Read
/// without validating the rest of the config, which a sign-in does not use.
pub fn configured_proxy(paths: &Paths) -> Result<Proxy> {
    let _lock = config::lock_dir(&paths.config_file)?;
    config::recover_provider_save(&paths.config_file)?;
    let network = Config::read_unchecked(&paths.config_file)?.network;
    let password = config::saved_keys(&paths.config_file)?.proxy;
    Ok(network.proxy(password.map(ApiKey::new)))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{Network, ProxyPassword, load_network, save_network};
    use crate::config::{ApiKey, Config, KeyChange, Paths, Proxy, ProxyLogin, ProxyMode};
    use crate::reload::ConfigChange;

    const CONFIG: &str =
        "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n";

    /// Paths in a fresh directory holding `CONFIG`, where no daemon listens.
    fn scratch(name: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("limen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), CONFIG).unwrap();
        Paths {
            socket: dir.join("limen.sock"),
            log_file: dir.join("daemon.log"),
            cache_dir: dir.join("cache"),
            config_file: dir.join("config.toml"),
        }
    }

    fn manual(username: &str) -> Network {
        Network {
            mode: ProxyMode::Manual,
            url: " socks5h://127.0.0.1:1080 ".to_string(),
            username: username.to_string(),
            bypass: "localhost, .corp.example".to_string(),
        }
    }

    #[tokio::test]
    async fn a_saved_proxy_loads_back_and_routes_both_roles() {
        let paths = scratch("network-save");

        let change = save_network(
            &paths,
            manual("me"),
            KeyChange::Set(ApiKey::new("secret".to_string())),
        )
        .await
        .unwrap();

        let config = Config::load(&paths.config_file).unwrap();
        let proxy = Proxy::Manual {
            url: "socks5h://127.0.0.1:1080".to_string(),
            login: Some(ProxyLogin {
                username: "me".to_string(),
                password: Some(ApiKey::new("secret".to_string())),
            }),
            bypass: "localhost, .corp.example".to_string(),
        };
        assert_eq!(
            (
                change,
                load_network(&paths).unwrap(),
                config.provider.proxy,
                config.polish.proxy,
                fs::read_to_string(paths.config_file.with_file_name("keys.toml")).unwrap(),
            ),
            (
                ConfigChange::Saved,
                (
                    Network {
                        url: "socks5h://127.0.0.1:1080".to_string(),
                        ..manual("me")
                    },
                    ProxyPassword::Saved,
                ),
                proxy.clone(),
                proxy,
                "proxy = \"secret\"\n".to_string(),
            )
        );
    }

    #[tokio::test]
    async fn removing_the_username_forgets_the_password() {
        let paths = scratch("network-forget");
        save_network(
            &paths,
            manual("me"),
            KeyChange::Set(ApiKey::new("secret".to_string())),
        )
        .await
        .unwrap();

        save_network(&paths, manual(""), KeyChange::Keep)
            .await
            .unwrap();

        assert_eq!(
            load_network(&paths).unwrap(),
            (
                Network {
                    url: "socks5h://127.0.0.1:1080".to_string(),
                    ..manual("")
                },
                ProxyPassword::Missing
            )
        );
    }

    #[tokio::test]
    async fn a_refused_proxy_leaves_the_files_as_they_were() {
        let paths = scratch("network-refused");

        let error = save_network(
            &paths,
            Network {
                url: "http://me:secret@proxy:8080".to_string(),
                ..manual("me")
            },
            KeyChange::Set(ApiKey::new("secret".to_string())),
        )
        .await
        .unwrap_err();

        assert_eq!(
            (
                format!("{error:#}").contains("cannot hold a login"),
                fs::read_to_string(&paths.config_file).unwrap(),
                paths.config_file.with_file_name("keys.toml").exists(),
            ),
            (true, CONFIG.to_string(), false)
        );
    }
}
