//! The Plugins settings page: the folders plugins are looked for in, the
//! plugins found, and the bundles added or removed by hand. It is the only
//! place plugins are scanned; the routing page adds from what is listed here.

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, StyledExt, button::ButtonVariants, scroll::ScrollbarMode,
};
use gpui_kit::*;
use limen_core::PluginLibrary;

use super::plugin_library::{Demand, LibraryState, Task};
use super::{SettingsPage, card, header};
use crate::a11y;
use crate::app::{AppView, Page};

/// The page's library, and its two text fields.
pub struct PluginsForm {
    pub library: LibraryState,
    folder: Entity<InputState>,
    bundle: Entity<InputState>,
    /// Narrows the list of plugins to those that match what is typed.
    search: Entity<InputState>,
}

/// What a path picked from the file dialog or typed becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Folder,
    Bundle,
}

impl PluginsForm {
    /// The page's fields, and the subscriptions that add what is typed when
    /// Enter is pressed.
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> (Self, Vec<Subscription>) {
        let folder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("plugins.folder_placeholder").to_string())
        });
        let bundle = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("plugins.bundle_placeholder").to_string())
        });
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(rust_i18n::t!("plugins.search").to_string())
        });
        let filtering =
            cx.subscribe_in(
                &search,
                window,
                |_, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
                },
            );
        let subscriptions = [(&folder, Pick::Folder), (&bundle, Pick::Bundle)]
            .into_iter()
            .map(|(input, pick)| {
                cx.subscribe_in(
                    input,
                    window,
                    move |this, _, event: &InputEvent, window, cx| match event {
                        InputEvent::PressEnter { .. } => this.add_typed(pick, window, cx),
                        InputEvent::Change | InputEvent::Focus | InputEvent::Blur => {}
                    },
                )
            })
            .chain([filtering])
            .collect();
        (
            Self {
                library: LibraryState::default(),
                folder,
                bundle,
                search,
            },
            subscriptions,
        )
    }
}

/// Called as the app is drawn: read the library when a page that lists
/// plugins is shown, which is this one, or the routing page with its list of
/// plugins to add open.
pub fn follow(view: &mut AppView) {
    let demand = match (view.page, view.routing.palette) {
        (Page::Settings(SettingsPage::Plugins), _) | (Page::Routing, true) => Demand::Wanted,
        (
            Page::Settings(SettingsPage::General | SettingsPage::Network | SettingsPage::About)
            | Page::Dictation
            | Page::Shortcuts
            | Page::Routing
            | Page::Providers
            | Page::Usage,
            _,
        ) => Demand::Idle,
    };
    let request = view.plugins.library.follow(demand);
    view.send_audio(request);
}

impl AppView {
    /// Add the folder or bundle typed in its field, and empty the field.
    fn add_typed(&mut self, pick: Pick, window: &mut Window, cx: &mut Context<Self>) {
        let input = match pick {
            Pick::Folder => &self.plugins.folder,
            Pick::Bundle => &self.plugins.bundle,
        };
        let text = input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        let request = match pick {
            Pick::Folder => self.plugins.library.add_folder(&text),
            Pick::Bundle => self.plugins.library.add_bundle(&text),
        };
        if request.is_some() {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.send_audio(request);
        cx.notify();
    }

    /// Ask for a folder or a bundle with the system's file dialog, and add
    /// what is picked.
    fn browse(&mut self, pick: Pick, cx: &mut Context<Self>) {
        let (files, prompt) = match pick {
            Pick::Folder => (false, "plugins.add_folder"),
            Pick::Bundle => (true, "plugins.add_bundle"),
        };
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files,
            directories: true,
            multiple: false,
            prompt: Some(rust_i18n::t!(prompt).to_string().into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.first().map(|path| path.display().to_string()) else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let request = match pick {
                    Pick::Folder => this.plugins.library.add_folder(&path),
                    Pick::Bundle => this.plugins.library.add_bundle(&path),
                };
                this.send_audio(request);
                cx.notify();
            });
        })
        .detach();
    }
}

/// A path, a button that browses for one, and a button that adds the path
/// typed.
fn adder(
    pick: Pick,
    field: &Entity<InputState>,
    ids: [&'static str; 3],
    label_key: &str,
    busy: bool,
    cx: &mut Context<AppView>,
) -> Div {
    let [input_id, browse_id, add_id] = ids;
    let label = rust_i18n::t!(label_key).to_string();
    div()
        .h_flex()
        .gap_2()
        .items_center()
        .child(
            div().flex_1().child(
                Input::new(field)
                    .id(input_id)
                    .accessibility_id(input_id)
                    .aria_label(label.clone()),
            ),
        )
        .child(
            a11y::button(browse_id)
                .outline()
                .small()
                .label(rust_i18n::t!("plugins.browse").to_string())
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| this.browse(pick, cx))),
        )
        .child(
            a11y::button(add_id)
                .outline()
                .small()
                .icon(IconName::Plus)
                .label(label)
                .disabled(busy)
                .on_click(cx.listener(move |this, _, window, cx| this.add_typed(pick, window, cx))),
        )
}

fn folders(view: &AppView, library: &PluginLibrary, busy: bool, cx: &mut Context<AppView>) -> Div {
    let muted = cx.theme().muted_foreground;
    let rows = match library.folders.is_empty() {
        true => vec![
            div()
                .text_sm()
                .text_color(muted)
                .child(rust_i18n::t!("plugins.no_folders").to_string()),
        ],
        false => library
            .folders
            .iter()
            .enumerate()
            .map(|(index, folder)| {
                let removed = folder.clone();
                let label = rust_i18n::t!("plugins.remove_folder").to_string();
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(format!("plugin-folder-{index}"))
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(folder.clone()),
                    )
                    .child(
                        a11y::button(format!("plugin-folder-remove-{index}"))
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .tooltip(label.clone())
                            .accessibility_label(label)
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let request = this.plugins.library.remove_folder(&removed);
                                this.send_audio(request);
                                cx.notify();
                            })),
                    )
            })
            .collect(),
    };
    div()
        .v_flex()
        .gap_3()
        .px_5()
        .py_4()
        .child(
            div()
                .v_flex()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .child(rust_i18n::t!("plugins.folders").to_string()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(rust_i18n::t!("plugins.folders_hint").to_string()),
                ),
        )
        .children(rows)
        .child(adder(
            Pick::Folder,
            &view.plugins.folder,
            [
                "plugin-folder-input",
                "plugin-folder-browse",
                "plugin-folder-add",
            ],
            "plugins.add_folder",
            busy,
            cx,
        ))
}

fn plugins(view: &AppView, library: &PluginLibrary, busy: bool, cx: &mut Context<AppView>) -> Div {
    let state = &view.plugins.library;
    let (muted, danger) = (cx.theme().muted_foreground, cx.theme().danger);
    let scanning = state.is_busy_with(Task::Scanning) || state.is_busy_with(Task::Restoring);
    let restore = match library.removed.is_empty() {
        true => None,
        false => Some(
            a11y::button("plugins-restore")
                .ghost()
                .small()
                .label(rust_i18n::t!("plugins.restore", count = library.removed.len()).to_string())
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| {
                    let request = this.plugins.library.restore();
                    this.send_audio(request);
                    cx.notify();
                })),
        ),
    };
    let query = view.plugins.search.read(cx).value().trim().to_lowercase();
    let listed = library
        .plugins
        .iter()
        .enumerate()
        .filter(|(_, plugin)| {
            [&plugin.name, &plugin.vendor, &plugin.category, &plugin.path]
                .iter()
                .any(|text| text.to_lowercase().contains(&query))
        })
        .map(|(index, plugin)| {
            let removed = plugin.clone();
            let label = rust_i18n::t!("plugins.remove").to_string();
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .truncate()
                                .child(format!("{} · {}", plugin.name, plugin.vendor)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(plugin.path.clone()),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(plugin.category.clone()),
                )
                .child(
                    a11y::button(format!("library-plugin-remove-{index}"))
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .tooltip(label.clone())
                        .accessibility_label(label)
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let request = this.plugins.library.remove(&removed);
                            this.send_audio(request);
                            cx.notify();
                        })),
                )
        })
        .collect::<Vec<_>>();
    let shown = listed.len();
    let rows = match (listed.is_empty(), library.plugins.is_empty()) {
        (false, _) => listed,
        (true, true) => vec![
            div()
                .text_sm()
                .text_color(muted)
                .child(rust_i18n::t!("plugins.none").to_string()),
        ],
        (true, false) => vec![
            div()
                .text_sm()
                .text_color(muted)
                .child(rust_i18n::t!("plugins.no_match").to_string()),
        ],
    };
    let count = match shown == library.plugins.len() {
        true => rust_i18n::t!("plugins.count", count = shown).to_string(),
        false => rust_i18n::t!(
            "plugins.count_of",
            shown = shown,
            count = library.plugins.len()
        )
        .to_string(),
    };
    // The list is a box of its own, so a long one scrolls in place rather
    // than pushing the rest of the page away.
    let list = view
        .plugins_list_scroll
        .container(
            div().id("library-plugins").child(
                div()
                    .v_flex()
                    .gap_2()
                    .p_3()
                    .on_scroll(view.plugins_list_scroll.on_scroll())
                    .children(rows),
            ),
            ScrollbarMode::Always,
        )
        .h(px(320.))
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background.opacity(0.5));
    let skipped = library.failures.iter().map(|failure| {
        div().text_xs().text_color(danger).child(format!(
            "{}: {} ({})",
            rust_i18n::t!("plugins.skipped"),
            failure.path,
            failure.error
        ))
    });
    div()
        .v_flex()
        .gap_3()
        .px_5()
        .py_4()
        .child(
            div()
                .h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .v_flex()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .child(rust_i18n::t!("plugins.library").to_string()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(rust_i18n::t!("plugins.library_hint").to_string()),
                        ),
                )
                .child(
                    div().h_flex().gap_2().children(restore).child(
                        a11y::button("plugins-scan")
                            .outline()
                            .small()
                            .label(match scanning {
                                true => rust_i18n::t!("plugins.scanning").to_string(),
                                false => rust_i18n::t!("plugins.scan").to_string(),
                            })
                            .loading(scanning)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let request = this.plugins.library.scan();
                                this.send_audio(request);
                                cx.notify();
                            })),
                    ),
                ),
        )
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(
                    div().flex_1().child(
                        Input::new(&view.plugins.search)
                            .id("plugin-search")
                            .accessibility_id("plugin-search")
                            .aria_label(rust_i18n::t!("plugins.search").to_string()),
                    ),
                )
                .child(
                    a11y::status("plugin-count", count.clone())
                        .text_xs()
                        .text_color(muted)
                        .child(count),
                ),
        )
        .child(list)
        .children(skipped)
        .child(adder(
            Pick::Bundle,
            &view.plugins.bundle,
            [
                "plugin-bundle-input",
                "plugin-bundle-browse",
                "plugin-bundle-add",
            ],
            "plugins.add_bundle",
            busy,
            cx,
        ))
}

/// The Plugins page.
pub fn page(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let state = &view.plugins.library;
    let busy = state.busy.is_some();
    let sections = match &state.library {
        Some(library) => card(
            [
                folders(view, library, busy, cx),
                plugins(view, library, busy, cx),
            ],
            cx,
        ),
        None => div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(rust_i18n::t!("plugins.loading").to_string()),
    };
    let error = state.error.clone().map(|error| {
        a11y::alert("plugins-error", error.clone())
            .text_sm()
            .text_color(cx.theme().danger)
            .child(error)
    });
    let content = div()
        .v_flex()
        .gap_5()
        .p_8()
        .on_scroll(view.plugins_scroll.on_scroll())
        .child(header(view, cx))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(rust_i18n::t!("plugins.hint").to_string()),
        )
        .child(sections)
        .children(error);
    view.plugins_scroll
        .container(
            div().id("settings-plugins").test_support().child(content),
            ScrollbarMode::Always,
        )
        .flex_1()
}
