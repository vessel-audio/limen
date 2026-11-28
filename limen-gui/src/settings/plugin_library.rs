//! The plugin library as the interface has it: what the daemon last said of
//! it, and the one request that is out. The Plugins settings page changes it
//! and the routing page's list of plugins to add reads it, so it lives
//! outside both.

use limen_core::{PluginEntry, PluginLibrary};

use crate::bridge::{AudioRequest, LibraryRequest};

/// What the request in flight is for, so the page can say what it is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Loading,
    Folders,
    Scanning,
    Adding,
    Removing,
    Restoring,
}

/// Whether a page that needs the library is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Demand {
    Wanted,
    Idle,
}

/// What follows a change of folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Then {
    Nothing,
    /// A folder was added: look in it.
    Scan,
}

pub struct LibraryState {
    /// The library as the daemon last said it.
    pub library: Option<PluginLibrary>,
    /// Why the last request failed.
    pub error: Option<String>,
    /// The request out, which the daemon has not answered. Only one goes at
    /// a time, so that a reply is always to the request that made it.
    pub busy: Option<(u64, Task)>,
    next_request: u64,
    /// Whether the library was asked for since a page last needed it.
    asked: bool,
    then: Then,
}

impl Default for LibraryState {
    fn default() -> Self {
        Self {
            library: None,
            error: None,
            busy: None,
            next_request: 0,
            asked: false,
            then: Then::Nothing,
        }
    }
}

impl LibraryState {
    fn send(&mut self, task: Task, request: LibraryRequest) -> Option<AudioRequest> {
        if self.busy.is_some() {
            return None;
        }
        self.next_request += 1;
        let id = self.next_request;
        self.busy = Some((id, task));
        self.error = None;
        Some(AudioRequest::Library { id, request })
    }

    /// Whether `task` is what is being waited for.
    pub fn is_busy_with(&self, task: Task) -> bool {
        self.busy.is_some_and(|(_, busy)| busy == task)
    }

    /// Called as pages are drawn: read the library each time a page that
    /// needs it is shown, so that it shows what the daemon has. Reads the
    /// file, never the plugins.
    pub fn follow(&mut self, demand: Demand) -> Option<AudioRequest> {
        match (demand, self.asked) {
            (Demand::Wanted, false) => {
                let request = self.send(Task::Loading, LibraryRequest::Get);
                self.asked = request.is_some();
                request
            }
            (Demand::Idle, true) if self.busy.is_none() => {
                self.asked = false;
                None
            }
            (Demand::Wanted, true) | (Demand::Idle, true) | (Demand::Idle, false) => None,
        }
    }

    /// Take the daemon's answer to request `id`, and send what was waiting
    /// on it. A reply to anything else is ignored.
    pub fn replied(
        &mut self,
        id: u64,
        result: Result<PluginLibrary, String>,
    ) -> Option<AudioRequest> {
        let (_, task) = self.busy.filter(|(busy, _)| *busy == id)?;
        self.busy = None;
        let then = std::mem::replace(&mut self.then, Then::Nothing);
        match (result, task, then) {
            (Ok(library), Task::Folders, Then::Scan) => {
                self.library = Some(library);
                self.scan()
            }
            (Ok(library), _, _) => {
                self.library = Some(library);
                None
            }
            (Err(message), _, _) => {
                self.error = Some(message);
                None
            }
        }
    }

    pub fn scan(&mut self) -> Option<AudioRequest> {
        self.send(Task::Scanning, LibraryRequest::Scan)
    }

    pub fn restore(&mut self) -> Option<AudioRequest> {
        self.send(Task::Restoring, LibraryRequest::Restore)
    }

    pub fn add_bundle(&mut self, path: &str) -> Option<AudioRequest> {
        self.send(Task::Adding, LibraryRequest::Add(path.trim().to_string()))
    }

    pub fn remove(&mut self, plugin: &PluginEntry) -> Option<AudioRequest> {
        self.send(
            Task::Removing,
            LibraryRequest::Remove {
                path: plugin.path.clone(),
                class_id: plugin.class_id.clone(),
            },
        )
    }

    /// Look in `folder` as well, and scan once it is saved.
    pub fn add_folder(&mut self, folder: &str) -> Option<AudioRequest> {
        let mut folders = self.library.as_ref()?.folders.clone();
        folders.push(folder.trim().to_string());
        let request = self.send(Task::Folders, LibraryRequest::Folders(folders));
        if request.is_some() {
            self.then = Then::Scan;
        }
        request
    }

    /// Stop looking in `folder`; the plugins found only there leave the list.
    pub fn remove_folder(&mut self, folder: &str) -> Option<AudioRequest> {
        let mut folders = self.library.as_ref()?.folders.clone();
        folders.retain(|kept| kept != folder);
        self.send(Task::Folders, LibraryRequest::Folders(folders))
    }
}

#[cfg(test)]
mod tests {
    use limen_core::PluginLibrary;

    use super::{Demand, LibraryState, Task};
    use crate::bridge::{AudioRequest, LibraryRequest};

    fn library(folders: &[&str]) -> PluginLibrary {
        PluginLibrary {
            folders: folders.iter().map(|folder| folder.to_string()).collect(),
            ..PluginLibrary::default()
        }
    }

    fn loaded(folders: &[&str]) -> LibraryState {
        let mut state = LibraryState::default();
        state.follow(Demand::Wanted);
        state.replied(1, Ok(library(folders)));
        state
    }

    fn library_request(id: u64, request: LibraryRequest) -> Option<AudioRequest> {
        Some(AudioRequest::Library { id, request })
    }

    #[test]
    fn the_library_is_read_once_each_time_a_page_that_needs_it_is_shown() {
        let mut state = LibraryState::default();
        let first = state.follow(Demand::Wanted);
        let again = state.follow(Demand::Wanted);
        state.replied(1, Ok(library(&["/a"])));
        let still_shown = state.follow(Demand::Wanted);
        state.follow(Demand::Idle);
        let shown_again = state.follow(Demand::Wanted);
        assert_eq!(
            (first, again, still_shown, shown_again),
            (
                library_request(1, LibraryRequest::Get),
                None,
                None,
                library_request(2, LibraryRequest::Get)
            )
        );
    }

    #[test]
    fn one_request_goes_at_a_time_and_a_reply_to_any_other_is_ignored() {
        let mut state = loaded(&["/a"]);
        let scan = state.scan();
        let refused = (state.restore(), state.add_bundle("/x/Y.vst3"));
        state.replied(7, Ok(library(&["/stale"])));
        let during = state.library.clone();
        state.replied(2, Ok(library(&["/b"])));
        assert_eq!(
            (scan, refused, during, state.library, state.busy),
            (
                library_request(2, LibraryRequest::Scan),
                (None, None),
                Some(library(&["/a"])),
                Some(library(&["/b"])),
                None
            )
        );
    }

    #[test]
    fn a_folder_that_is_added_is_scanned_once_it_is_saved() {
        let mut state = loaded(&["/a"]);
        let save = state.add_folder("  /b ");
        let scan = state.replied(2, Ok(library(&["/a", "/b"])));
        assert_eq!(
            (save, scan, state.busy),
            (
                library_request(
                    2,
                    LibraryRequest::Folders(vec!["/a".to_string(), "/b".to_string()])
                ),
                library_request(3, LibraryRequest::Scan),
                Some((3, Task::Scanning))
            )
        );
    }

    #[test]
    fn a_refused_folder_is_not_scanned_and_its_reason_is_kept() {
        let mut state = loaded(&["/a"]);
        state.add_folder("/nope");
        let next = state.replied(2, Err("/nope is not a folder".to_string()));
        let error = state.error.clone();
        let removal = state.remove_folder("/a");
        assert_eq!(
            (next, error, removal, state.library, state.error),
            (
                None,
                Some("/nope is not a folder".to_string()),
                library_request(3, LibraryRequest::Folders(Vec::new())),
                Some(library(&["/a"])),
                None
            )
        );
    }
}
