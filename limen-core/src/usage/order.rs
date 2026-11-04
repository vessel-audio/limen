//! The order the user dragged the Usage page's provider cards into, kept
//! beside the usage ledger.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::{Paths, Service};

fn order_file(paths: &Paths) -> PathBuf {
    paths.log_file.with_file_name("usage-cards.json")
}

/// The providers in the order last saved; empty before any drag. A file
/// that cannot be read is an error; a service this build no longer knows
/// is left out.
pub fn read_card_order(paths: &Paths) -> Result<Vec<Service>> {
    let path = order_file(paths);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Vec<serde_json::Value>>(&text)
            .map(|names| {
                names
                    .into_iter()
                    .filter_map(|name| serde_json::from_value(name).ok())
                    .collect()
            })
            .with_context(|| format!("failed to read {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// Keep `order` as the providers' order.
pub fn save_card_order(paths: &Paths, order: &[Service]) -> Result<()> {
    let path = order_file(paths);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::config::replace(&path, &serde_json::to_string(order)?, None)
}

#[cfg(test)]
mod tests {
    use super::{read_card_order, save_card_order};
    use crate::config::{Paths, Service};

    #[test]
    fn a_saved_order_reads_back_without_services_it_no_longer_knows() {
        let dir = std::env::temp_dir().join(format!("limen-card-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths {
            socket: dir.join("limen.sock"),
            log_file: dir.join("daemon.log"),
            cache_dir: dir.join("cache"),
            config_file: dir.join("config.toml"),
        };
        assert_eq!(read_card_order(&paths).unwrap(), Vec::new());

        save_card_order(&paths, &[Service::DeepSeek, Service::Groq]).unwrap();
        std::fs::write(
            dir.join("usage-cards.json"),
            r#"["deepseek","retired-vendor","groq"]"#,
        )
        .unwrap();

        assert_eq!(
            read_card_order(&paths).unwrap(),
            vec![Service::DeepSeek, Service::Groq]
        );
    }
}
