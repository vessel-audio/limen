//! The `[network]` section: whether requests to model services go through
//! a proxy, and which.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::ApiKey;

/// Which proxy requests go through.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    /// The one the environment names, if any.
    #[default]
    System,
    /// None: requests always go directly.
    None,
    /// The one `proxy_url` names.
    Manual,
}

impl ProxyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::None => "none",
            Self::Manual => "manual",
        }
    }
}

/// The hosts a manual proxy is not used for until the user names others:
/// local servers, such as llama.cpp's.
pub const DEFAULT_NO_PROXY: &str = "localhost, 127.0.0.1, ::1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    pub proxy: ProxyMode,
    /// The proxy a manual one is; kept while another mode is chosen.
    pub proxy_url: String,
    /// Who signs in to the proxy; the password is saved from the GUI.
    pub proxy_username: Option<String>,
    /// Hosts and domains a manual proxy is not used for, as `NO_PROXY`
    /// lists them.
    pub no_proxy: String,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            proxy: ProxyMode::System,
            proxy_url: String::new(),
            proxy_username: None,
            no_proxy: DEFAULT_NO_PROXY.to_string(),
        }
    }
}

/// How a request reaches its server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Proxy {
    /// Through the proxy `HTTPS_PROXY`, `HTTP_PROXY`, or `ALL_PROXY` names,
    /// except to the hosts `NO_PROXY` lists; directly when none is named.
    #[default]
    System,
    /// Directly, whatever the environment names.
    Direct,
    /// Through the proxy at `url`, signed in to with `login`, except to the
    /// hosts `bypass` lists.
    Manual {
        url: String,
        login: Option<ProxyLogin>,
        bypass: String,
    },
}

/// Who signs in to a proxy. The password's `Debug` output never shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyLogin {
    pub username: String,
    pub password: Option<ApiKey>,
}

impl Proxy {
    /// `builder`, its requests sent this way.
    pub fn route(&self, builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder> {
        match self {
            Self::System => Ok(builder),
            Self::Direct => Ok(builder.no_proxy()),
            Self::Manual { url, login, bypass } => {
                let mut url = reqwest::Url::parse(url).context("invalid network.proxy_url")?;
                // The login travels in the URL, which the HTTP client takes
                // it from for HTTP and SOCKS proxies alike.
                if let Some(ProxyLogin { username, password }) = login {
                    url.set_username(username)
                        .and_then(|()| url.set_password(password.as_ref().map(ApiKey::expose)))
                        .map_err(|()| anyhow::anyhow!("network.proxy_url cannot take a login"))?;
                }
                let proxy = reqwest::Proxy::all(url)
                    .context("invalid network.proxy_url")?
                    .no_proxy(reqwest::NoProxy::from_string(bypass));
                Ok(builder.proxy(proxy))
            }
        }
    }
}

impl NetworkConfig {
    /// How requests go, signing in to a manual proxy with `password`.
    pub(crate) fn proxy(&self, password: Option<ApiKey>) -> Proxy {
        match self.proxy {
            ProxyMode::System => Proxy::System,
            ProxyMode::None => Proxy::Direct,
            ProxyMode::Manual => Proxy::Manual {
                url: self.proxy_url.trim().to_string(),
                login: self
                    .proxy_username
                    .clone()
                    .filter(|username| !username.is_empty())
                    .map(|username| ProxyLogin { username, password }),
                bypass: self.no_proxy.clone(),
            },
        }
    }

    /// A manual proxy must be named, and a named one, whichever mode is
    /// chosen, must be a proxy URL with a host and nothing past its port.
    pub(super) fn check(&self) -> Result<()> {
        let url = self.proxy_url.trim();
        match (self.proxy, url.is_empty()) {
            (ProxyMode::Manual, true) => {
                bail!("network.proxy_url is required when network.proxy is \"manual\"")
            }
            (ProxyMode::System | ProxyMode::None, true) => {}
            (ProxyMode::System | ProxyMode::None | ProxyMode::Manual, false) => {
                let parsed = reqwest::Url::parse(url).context("invalid network.proxy_url")?;
                match (parsed.scheme(), parsed.host_str()) {
                    ("http" | "https" | "socks5" | "socks5h", Some(_)) => {}
                    _ => bail!(
                        "network.proxy_url must be an http://, https://, socks5://, or socks5h:// URL with a host"
                    ),
                }
                if !parsed.username().is_empty() || parsed.password().is_some() {
                    bail!(
                        "network.proxy_url cannot hold a login: set network.proxy_username and save the password from the GUI"
                    );
                }
                match (parsed.path(), parsed.query(), parsed.fragment()) {
                    ("" | "/", None, None) => {}
                    _ => bail!("network.proxy_url must name only the proxy's host and port"),
                }
            }
        }
        // The username goes into a request header, where these would split it.
        if self
            .proxy_username
            .as_deref()
            .is_some_and(|username| username.chars().any(char::is_control))
        {
            bail!("network.proxy_username cannot contain control characters");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_NO_PROXY, NetworkConfig, Proxy, ProxyLogin, ProxyMode};
    use crate::config::ApiKey;

    fn manual(url: &str) -> NetworkConfig {
        NetworkConfig {
            proxy: ProxyMode::Manual,
            proxy_url: url.to_string(),
            ..NetworkConfig::default()
        }
    }

    #[test]
    fn a_proxy_url_names_only_a_proxy() {
        let refused = [
            ("", "is required"),
            ("ftp://proxy:21", "must be an http://"),
            ("socks4://proxy:1080", "must be an http://"),
            ("http://me:secret@proxy:8080", "cannot hold a login"),
            ("http://proxy:8080/path", "only the proxy's host and port"),
            ("http://proxy:8080?a=b", "only the proxy's host and port"),
        ]
        .map(|(url, reason)| {
            (
                url,
                manual(url)
                    .check()
                    .unwrap_err()
                    .to_string()
                    .contains(reason),
            )
        });
        assert_eq!(refused, refused.map(|(url, _)| (url, true)));
        for url in [
            "http://127.0.0.1:7890",
            "https://proxy.example/",
            "socks5://proxy:1080",
            "socks5h://proxy",
        ] {
            manual(url).check().unwrap();
        }
    }

    #[test]
    fn a_proxy_url_is_checked_and_kept_in_every_mode() {
        let system = NetworkConfig {
            proxy: ProxyMode::System,
            ..manual("ftp://proxy")
        };
        assert!(system.check().is_err());
        let none = NetworkConfig {
            proxy: ProxyMode::None,
            ..manual("")
        };
        none.check().unwrap();
    }

    #[test]
    fn a_manual_proxy_signs_in_only_with_a_username() {
        let password = Some(ApiKey::new("secret".to_string()));
        assert_eq!(
            [None, Some(""), Some("me")].map(|username| NetworkConfig {
                proxy_username: username.map(str::to_string),
                ..manual(" http://proxy:8080 ")
            }
            .proxy(password.clone())),
            [None, None, Some("me")].map(|username: Option<&str>| Proxy::Manual {
                url: "http://proxy:8080".to_string(),
                login: username.map(|username| ProxyLogin {
                    username: username.to_string(),
                    password: password.clone(),
                }),
                bypass: DEFAULT_NO_PROXY.to_string(),
            })
        );
    }

    #[test]
    fn a_login_with_reserved_characters_still_builds_a_client() {
        let proxy = Proxy::Manual {
            url: "socks5h://proxy:1080".to_string(),
            login: Some(ProxyLogin {
                username: "me@corp".to_string(),
                password: Some(ApiKey::new("p:a/s@s".to_string())),
            }),
            bypass: DEFAULT_NO_PROXY.to_string(),
        };
        proxy
            .route(reqwest::Client::builder())
            .unwrap()
            .build()
            .unwrap();
    }
}
