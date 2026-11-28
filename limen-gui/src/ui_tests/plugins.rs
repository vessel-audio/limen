//! The Plugins settings page: the folders plugins are looked for in, the
//! plugins found, and the bundles added or removed. The tests play the
//! daemon's part by feeding back the replies the bridge would send.

use gpui_kit::TestAppContext;
use limen_core::{PluginEntry, PluginLibrary, RemovedPlugin, ScanFailure};
use rust_i18n::t;

use super::{Daemon, Ui};
use crate::bridge::{AudioRequest, BridgeCommand, BridgeEvent, LibraryRequest};

fn plugin(name: &str, category: &str) -> PluginEntry {
    PluginEntry {
        path: format!("/opt/vst3/{name}.vst3"),
        class_id: "00ff".to_string(),
        name: name.to_string(),
        vendor: "Acme".to_string(),
        category: category.to_string(),
        effect: category.starts_with("Fx"),
    }
}

fn library(folders: &[&str], plugins: Vec<PluginEntry>) -> PluginLibrary {
    PluginLibrary {
        folders: folders.iter().map(|folder| folder.to_string()).collect(),
        plugins,
        ..PluginLibrary::default()
    }
}

fn asked(id: u64, request: LibraryRequest) -> Vec<BridgeCommand> {
    vec![BridgeCommand::Audio(AudioRequest::Library { id, request })]
}

/// Open the page and answer its read of the library with `library`.
fn open_plugins(ui: &mut Ui, library: PluginLibrary) -> Vec<BridgeCommand> {
    ui.click("settings-page");
    ui.click("settings-tab-plugins");
    let sent = ui.sent();
    ui.receive(BridgeEvent::AudioLibrary {
        id: 1,
        result: Ok(library),
    });
    sent
}

#[gpui_kit::test]
fn the_page_lists_the_saved_library_and_reads_no_plugin_to_do_it(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    let sent = open_plugins(
        &mut ui,
        PluginLibrary {
            failures: vec![ScanFailure {
                path: "/opt/vst3/Broken.vst3".to_string(),
                error: "did not load".to_string(),
            }],
            ..library(
                &["/opt/vst3", "/home/me/vst3"],
                vec![plugin("Gate", "Fx|Dynamics"), plugin("Synth", "Instrument")],
            )
        },
    );
    let shown = [
        "plugin-folder-0",
        "plugin-folder-1",
        "plugin-folder-2",
        "library-plugin-remove-0",
        "library-plugin-remove-1",
        "library-plugin-remove-2",
        "plugins-restore",
    ]
    .map(|id| ui.try_find(id).is_some());
    assert_eq!(
        (sent, ui.label("current-page"), shown),
        (
            asked(1, LibraryRequest::Get),
            t!("settings.plugins").into(),
            [true, true, false, true, true, false, false]
        )
    );
}

#[gpui_kit::test]
fn a_folder_that_is_typed_is_saved_with_the_others_and_then_scanned(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_plugins(&mut ui, library(&["/opt/vst3"], vec![]));
    ui.retype("plugin-folder-input", "/home/me/vst3");
    ui.click("plugin-folder-add");
    let save = ui.sent();
    ui.receive(BridgeEvent::AudioLibrary {
        id: 2,
        result: Ok(library(&["/opt/vst3", "/home/me/vst3"], vec![])),
    });
    let scan = ui.sent();
    // Nothing is sent while the scan runs, and the buttons wait for it.
    let waiting = (ui.sent(), ui.find("plugins-scan").disabled());
    ui.receive(BridgeEvent::AudioLibrary {
        id: 3,
        result: Ok(library(
            &["/opt/vst3", "/home/me/vst3"],
            vec![plugin("Gate", "Fx|Dynamics")],
        )),
    });
    assert_eq!(
        (save, scan, waiting, ui.find("plugins-scan").disabled()),
        (
            asked(
                2,
                LibraryRequest::Folders(vec!["/opt/vst3".to_string(), "/home/me/vst3".to_string()])
            ),
            asked(3, LibraryRequest::Scan),
            (vec![], Some(true)),
            None
        )
    );
}

#[gpui_kit::test]
fn a_folder_is_dropped_and_scanning_is_only_on_the_button(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_plugins(&mut ui, library(&["/opt/vst3", "/home/me/vst3"], vec![]));
    ui.click("plugin-folder-remove-0");
    let dropped = ui.sent();
    ui.receive(BridgeEvent::AudioLibrary {
        id: 2,
        result: Ok(library(&["/home/me/vst3"], vec![])),
    });
    let after = ui.sent();
    ui.click("plugins-scan");
    assert_eq!(
        (dropped, after, ui.sent()),
        (
            asked(
                2,
                LibraryRequest::Folders(vec!["/home/me/vst3".to_string()])
            ),
            vec![],
            asked(3, LibraryRequest::Scan)
        )
    );
}

#[gpui_kit::test]
fn a_plugin_is_taken_off_the_list_and_what_was_removed_can_be_restored(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_plugins(
        &mut ui,
        library(
            &["/opt/vst3"],
            vec![plugin("Eq", "Fx|EQ"), plugin("Gate", "Fx|Dynamics")],
        ),
    );
    ui.click("library-plugin-remove-1");
    let removal = ui.sent();
    ui.receive(BridgeEvent::AudioLibrary {
        id: 2,
        result: Ok(PluginLibrary {
            removed: vec![RemovedPlugin {
                path: "/opt/vst3/Gate.vst3".to_string(),
                class_id: "00ff".to_string(),
            }],
            ..library(&["/opt/vst3"], vec![plugin("Eq", "Fx|EQ")])
        }),
    });
    let restore = ui.label("plugins-restore");
    ui.click("plugins-restore");
    assert_eq!(
        (removal, restore, ui.sent()),
        (
            asked(
                2,
                LibraryRequest::Remove {
                    path: "/opt/vst3/Gate.vst3".to_string(),
                    class_id: "00ff".to_string()
                }
            ),
            t!("plugins.restore", count = 1).to_string(),
            asked(3, LibraryRequest::Restore)
        )
    );
}

#[gpui_kit::test]
fn a_bundle_that_is_refused_says_why_and_leaves_the_list_as_it_was(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_plugins(
        &mut ui,
        library(&["/opt/vst3"], vec![plugin("Eq", "Fx|EQ")]),
    );
    // The field is below the list's box, at the page's foot.
    ui.scroll(
        "settings-plugins",
        gpui_kit::ScrollDelta::Pixels(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(-3000.))),
    );
    ui.retype("plugin-bundle-input", "/home/me/Odd.vst3");
    ui.click("plugin-bundle-add");
    let add = ui.sent();
    let reason = "/home/me/Odd.vst3 could not be read: did not load".to_string();
    ui.receive(BridgeEvent::AudioLibrary {
        id: 2,
        result: Err(reason.clone()),
    });
    assert_eq!(
        (
            add,
            ui.label("plugins-error"),
            ui.try_find("library-plugin-remove-0").is_some()
        ),
        (
            asked(2, LibraryRequest::Add("/home/me/Odd.vst3".to_string())),
            reason,
            true
        )
    );
}

#[gpui_kit::test]
fn the_list_is_narrowed_by_what_is_typed_and_counts_what_it_shows(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_plugins(
        &mut ui,
        library(
            &["/opt/vst3"],
            vec![
                plugin("Eq", "Fx|EQ"),
                plugin("Gate", "Fx|Dynamics"),
                plugin("Synth", "Instrument"),
            ],
        ),
    );
    let all = ui.label("plugin-count");
    ui.retype("plugin-search", "dynamics");
    let narrowed = (
        ui.label("plugin-count"),
        [0, 1, 2].map(|index| {
            ui.try_find(&format!("library-plugin-remove-{index}"))
                .is_some()
        }),
    );
    ui.retype("plugin-search", "nothing like it");
    let none = ui.label("plugin-count");
    assert_eq!(
        (all, narrowed, none),
        (
            t!("plugins.count", count = 3).to_string(),
            (
                t!("plugins.count_of", shown = 1, count = 3).to_string(),
                [false, true, false]
            ),
            t!("plugins.count_of", shown = 0, count = 3).to_string(),
        )
    );
}
