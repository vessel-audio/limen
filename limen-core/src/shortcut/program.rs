//! Where the installed `limen` program is, for things that run it
//! later: a GNOME custom keybinding and the desktop entry.
//!
//! A Nix package runs as `<out>/bin/.limen-wrapped` inside the store,
//! a path that changes with every upgrade and disappears after garbage
//! collection. A profile link such as `/etc/profiles/per-user/<user>/bin/
//! limen` that resolves to the running package keeps working across
//! upgrades, so it is preferred.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const NIX_STORE: &str = "/nix/store";

/// The program path to run, and whether it survives upgrades.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Program {
    /// A path outside the Nix store that remains valid across upgrades.
    Stable(PathBuf),
    /// Inside the Nix store: breaks after an upgrade or garbage collection.
    Unstable(PathBuf),
}

impl Program {
    pub(crate) fn path(&self) -> &Path {
        match self {
            Self::Stable(path) | Self::Unstable(path) => path,
        }
    }

    /// Whether `path` survives upgrades.
    pub(crate) fn of(path: PathBuf) -> Self {
        match path.starts_with(NIX_STORE) {
            true => Self::Unstable(path),
            false => Self::Stable(path),
        }
    }

    /// Find the path to run: the running executable, unless it is in the Nix
    /// store and a `limen` on `PATH` outside the store resolves to it.
    pub(crate) fn find() -> Result<Self> {
        let current_exe = std::env::current_exe().context("cannot find the running executable")?;
        let dirs = std::env::var_os("PATH").unwrap_or_default();
        let candidates = std::env::split_paths(&dirs)
            .map(|dir| dir.join("limen"))
            .filter_map(|candidate| {
                std::fs::canonicalize(&candidate)
                    .ok()
                    .map(|resolved| (candidate, resolved))
            });
        Ok(Self::choose(&current_exe, candidates))
    }

    /// `candidates` pairs each `limen` on `PATH` with what it resolves to.
    fn choose(
        current_exe: &Path,
        candidates: impl IntoIterator<Item = (PathBuf, PathBuf)>,
    ) -> Self {
        // wrapProgram renames the binary and puts a wrapper that sets up its
        // environment in its place; run the wrapper.
        let program = match current_exe.file_name().and_then(OsStr::to_str) {
            Some(".limen-wrapped") => current_exe.with_file_name("limen"),
            Some(_) | None => current_exe.to_path_buf(),
        };
        match program.starts_with(NIX_STORE) {
            false => Self::Stable(program),
            true => candidates
                .into_iter()
                .find(|(candidate, resolved)| {
                    // A relative directory on PATH would resolve against the
                    // working directory of whatever runs the program later.
                    candidate.is_absolute()
                        && !candidate.starts_with(NIX_STORE)
                        && *resolved == program
                })
                .map_or_else(
                    || Self::Unstable(program.clone()),
                    |(candidate, _)| Self::Stable(candidate),
                ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Program;

    const OUT: &str = "/nix/store/abc-limen-0.1.0/bin";

    fn wrapped() -> PathBuf {
        PathBuf::from(OUT).join(".limen-wrapped")
    }

    fn pair(candidate: &str, resolved: &str) -> (PathBuf, PathBuf) {
        (PathBuf::from(candidate), PathBuf::from(resolved))
    }

    #[test]
    fn a_profile_link_into_the_running_package_is_preferred() {
        let profile = "/etc/profiles/per-user/me/bin/limen";
        assert_eq!(
            Program::choose(
                &wrapped(),
                [
                    pair("/home/me/.local/bin/limen", "/home/me/.local/bin/limen"),
                    pair(profile, &format!("{OUT}/limen")),
                ]
            ),
            Program::Stable(PathBuf::from(profile))
        );
    }

    #[test]
    fn a_build_outside_the_store_is_run_directly() {
        let cargo = PathBuf::from("/home/me/limen/target/release/limen");
        assert_eq!(
            Program::choose(&cargo, [pair("/usr/bin/limen", "/usr/bin/limen")]),
            Program::Stable(cargo)
        );
    }

    #[test]
    fn without_a_link_into_the_package_the_store_path_is_unstable() {
        assert_eq!(
            Program::choose(
                &wrapped(),
                [
                    pair(&format!("{OUT}/limen"), &format!("{OUT}/limen")),
                    pair("result/bin/limen", &format!("{OUT}/limen")),
                    pair("/home/me/bin/limen", &format!("{OUT}/.limen-wrapped"),),
                    pair(
                        "/home/me/.nix-profile/bin/limen",
                        "/nix/store/old-limen-0.0.9/bin/limen",
                    ),
                ]
            ),
            Program::Unstable(PathBuf::from(OUT).join("limen"))
        );
    }
}
