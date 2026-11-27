//! The Audio routing page.

use std::path::Path;

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, StyledExt, button::ButtonVariants,
    scroll::ScrollbarMode,
};
use gpui_kit::*;
use limen_core::{Playback, PluginEntry};
use plugin_graph::{NodeId, NodeKind, NodeSpec, Sidechain};

use super::canvas::{canvas_view, title};
use super::state::{Notice, Plugins, Selection, TestState};
use crate::a11y;
use crate::app::{AppView, Page};
use crate::bridge::{AudioRequest, BridgeCommand};

/// How long a test recording lasts, in seconds.
const TEST_SECONDS: u32 = 5;

/// Which recording of the test is playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Original,
    Processed,
}

/// A test recording playing through the speakers.
pub struct Playing {
    which: Which,
    playback: Playback,
}

impl AppView {
    /// Send a request to the daemon, if there is one to send.
    pub(crate) fn send_audio(&self, request: Option<AudioRequest>) {
        if let Some(request) = request {
            let _ = self.commands.try_send(BridgeCommand::Audio(request));
        }
    }

    fn play(&mut self, which: Which) {
        let TestState::Ready(files) = &self.routing.test else {
            return;
        };
        let wav = match which {
            Which::Original => &files.dry,
            Which::Processed => &files.wet,
        };
        // The other recording starts where this one is, so that the two can
        // be compared on the same words.
        let from = self
            .playing
            .as_ref()
            .map(|playing| playing.playback.position())
            .unwrap_or_default();
        match Playback::start_at(Path::new(wav), from) {
            Ok(playback) => self.playing = Some(Playing { which, playback }),
            Err(error) => {
                self.playing = None;
                self.routing.notice = Some(Notice::Daemon(format!("{error:#}")));
            }
        }
    }
}

/// Called as the page is drawn: ask the daemon for the graph when the page is
/// shown, and let go of a recording that has played to its end.
pub(crate) fn follow(view: &mut AppView) {
    let dictation = view.model.snapshot.as_ref().map(|snapshot| {
        (
            snapshot.state.take,
            matches!(
                snapshot.state.dictation.state,
                limen_core::DictationState::Idle
            ),
        )
    });
    let request = view.routing.follow(view.page == Page::Routing, dictation);
    view.send_audio(request);
    if view
        .playing
        .as_mut()
        .is_some_and(|playing| playing.playback.finished())
    {
        view.playing = None;
    }
}

pub(crate) fn page(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let routing = &view.routing;
    let theme = cx.theme();
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let scroll = &view.routing_scroll;

    let notice = routing.notice.as_ref().map(|notice| {
        let text = match notice {
            Notice::WouldLoop => rust_i18n::t!("routing.loop").to_string(),
            Notice::Daemon(message) => message.clone(),
        };
        a11y::alert("routing-notice", text.clone())
            .text_sm()
            .text_color(danger)
            .child(text)
    });
    let hint = routing.wiring.map(|_| {
        let text = rust_i18n::t!("routing.wiring").to_string();
        a11y::status("routing-hint", text.clone())
            .text_sm()
            .text_color(muted)
            .child(text)
    });

    let body = match routing.graph.is_some() {
        true => div()
            .v_flex()
            .gap_5()
            .child(canvas_view(view, cx))
            .child(nodes_list(view, cx))
            .child(connections_list(view, cx))
            .child(test_panel(view, cx)),
        false => div()
            .v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(rust_i18n::t!("routing.loading").to_string()),
            )
            .child(
                a11y::button("routing-retry")
                    .outline()
                    .small()
                    .label(rust_i18n::t!("routing.retry").to_string())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.routing.retry();
                        cx.notify();
                    })),
            ),
    };

    let content = div()
        .v_flex()
        .gap_4()
        .p_8()
        .on_scroll(scroll.on_scroll())
        .child(header(view, cx))
        .children(hint)
        .children(notice)
        .children(palette(view, cx))
        .child(body);
    scroll
        .container(
            div().id("routing-content").test_support().child(content),
            ScrollbarMode::Always,
        )
        .flex_1()
}

fn header(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let editable = view.routing.graph.is_some();
    // A wire can always go; a node only if it is not the input or the output.
    let removable = match view.routing.selection {
        Selection::Wire(_) => true,
        Selection::Node(id) => view.routing.graph.as_ref().is_some_and(|graph| {
            graph.nodes.iter().any(|node| {
                node.id == id
                    && match node.kind {
                        NodeKind::Gain { .. } | NodeKind::Plugin(_) => true,
                        NodeKind::Input | NodeKind::Output => false,
                    }
            })
        }),
        Selection::Nothing => false,
    };
    div()
        .h_flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w(px(240.))
                .gap_0p5()
                .child(
                    div()
                        .text_xl()
                        .font_semibold()
                        .child(rust_i18n::t!("routing.title").to_string()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(rust_i18n::t!("routing.hint").to_string()),
                ),
        )
        .child(
            div()
                .h_flex()
                .gap_2()
                .child(
                    a11y::button("add-plugin")
                        .outline()
                        .small()
                        .icon(IconName::Plus)
                        .label(rust_i18n::t!("routing.add_plugin").to_string())
                        .disabled(!editable)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let request = this.routing.open_palette();
                            this.send_audio(request);
                            cx.notify();
                        })),
                )
                .child(
                    a11y::button("remove-selected")
                        .outline()
                        .small()
                        .icon(IconName::Close)
                        .label(rust_i18n::t!("routing.remove_selected").to_string())
                        .disabled(!removable)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let request = this.routing.remove_selected();
                            this.send_audio(request);
                            cx.notify();
                        })),
                )
                .child(
                    a11y::button("add-gain")
                        .outline()
                        .small()
                        .icon(IconName::Plus)
                        .label(rust_i18n::t!("routing.add_gain").to_string())
                        .disabled(!editable)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let request = this.routing.add_gain();
                            this.send_audio(request);
                            cx.notify();
                        })),
                ),
        )
}

/// The installed effect plugins, to pick one to add.
fn palette(view: &AppView, cx: &mut Context<AppView>) -> Option<Stateful<Div>> {
    let routing = &view.routing;
    if !routing.palette {
        return None;
    }
    let theme = cx.theme();
    let (border, muted) = (theme.border, theme.muted_foreground);
    let message = |text: String| div().text_sm().text_color(muted).child(text);
    let rows = match &routing.plugins {
        Plugins::NotListed | Plugins::Listing(_) => {
            vec![message(rust_i18n::t!("routing.scanning").to_string())]
        }
        Plugins::Failed(error) => vec![message(error.clone())],
        Plugins::Listed(plugins) => {
            let effects = plugins
                .iter()
                .enumerate()
                .filter(|(_, plugin)| plugin.effect)
                .map(|(index, plugin)| plugin_row(index, plugin, cx))
                .collect::<Vec<_>>();
            match effects.is_empty() {
                true => vec![message(rust_i18n::t!("routing.no_plugins").to_string())],
                false => effects,
            }
        }
    };
    Some(
        div()
            .id("plugin-list")
            .v_flex()
            .gap_1()
            .p_3()
            .max_h(px(260.))
            .overflow_y_scroll()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .children(rows)
            .child(
                div().h_flex().justify_end().child(
                    a11y::button("close-plugin-list")
                        .ghost()
                        .small()
                        .label(rust_i18n::t!("routing.close").to_string())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.routing.palette = false;
                            cx.notify();
                        })),
                ),
            ),
    )
}

fn plugin_row(index: usize, plugin: &PluginEntry, cx: &mut Context<AppView>) -> Div {
    let entry = plugin.clone();
    div()
        .h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            a11y::button(format!("plugin-{index}"))
                .ghost()
                .small()
                .label(format!("{} · {}", plugin.name, plugin.vendor))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let request = this.routing.add_plugin(&entry);
                    this.send_audio(request);
                    cx.notify();
                })),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(plugin.category.clone()),
        )
}

fn section(heading: &str, cx: &mut Context<AppView>) -> Div {
    div().v_flex().gap_2().child(
        div()
            .text_sm()
            .font_semibold()
            .text_color(cx.theme().foreground)
            .child(rust_i18n::t!(heading).to_string()),
    )
}

/// What can be done with each node besides moving it: the same controls
/// with names, for the keyboard and for screen readers.
fn nodes_list(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let routing = &view.routing;
    let muted = cx.theme().muted_foreground;
    let rows = routing
        .graph
        .iter()
        .flat_map(|graph| graph.nodes.iter())
        .filter(|node| match node.kind {
            NodeKind::Gain { .. } | NodeKind::Plugin(_) => true,
            NodeKind::Input | NodeKind::Output => false,
        })
        .map(|node| node_row(view, node, cx))
        .collect::<Vec<_>>();
    let body = match rows.is_empty() {
        true => div()
            .text_sm()
            .text_color(muted)
            .child(rust_i18n::t!("routing.no_nodes").to_string())
            .into_any_element(),
        false => div().v_flex().gap_1().children(rows).into_any_element(),
    };
    section("routing.nodes", cx).child(body)
}

fn node_row(view: &AppView, node: &NodeSpec, cx: &mut Context<AppView>) -> Div {
    let id: NodeId = node.id;
    let (name, _) = title(node);
    let theme = cx.theme();
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let problem = view
        .routing
        .status(id)
        .and_then(|status| status.problem.clone());
    let remove = a11y::button(format!("remove-node-{id}"))
        .ghost()
        .small()
        .icon(IconName::Close)
        .tooltip(rust_i18n::t!("routing.remove").to_string())
        .accessibility_label(format!("{} {name}", rust_i18n::t!("routing.remove")))
        .on_click(cx.listener(move |this, _, _, cx| {
            let request = this.routing.remove_node(id);
            this.send_audio(request);
            cx.notify();
        }));
    let controls = match &node.kind {
        NodeKind::Plugin(plugin) => {
            let (enabled, sidechain) = (plugin.enabled, plugin.sidechain);
            div()
                .h_flex()
                .gap_1()
                .child(
                    a11y::button(format!("toggle-node-{id}"))
                        .outline()
                        .small()
                        .label(match enabled {
                            true => rust_i18n::t!("settings.on").to_string(),
                            false => rust_i18n::t!("settings.off").to_string(),
                        })
                        .accessibility_label(format!(
                            "{name}: {}",
                            rust_i18n::t!("routing.enabled")
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let request = this.routing.toggle_enabled(id);
                            this.send_audio(request);
                            cx.notify();
                        })),
                )
                .child(
                    a11y::button(format!("sidechain-node-{id}"))
                        .outline()
                        .small()
                        .label(format!(
                            "{}: {}",
                            rust_i18n::t!("routing.sidechain"),
                            match sidechain {
                                Sidechain::On => rust_i18n::t!("settings.on"),
                                Sidechain::Off => rust_i18n::t!("settings.off"),
                            }
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let request = this.routing.toggle_sidechain(id);
                            this.send_audio(request);
                            cx.notify();
                        })),
                )
                .child(
                    a11y::button(format!("edit-node-{id}"))
                        .outline()
                        .small()
                        .icon(IconName::Settings2)
                        .label(rust_i18n::t!("routing.edit").to_string())
                        .on_click(cx.listener(move |this, _, _, _| {
                            this.send_audio(Some(AudioRequest::OpenEditor { node: id }));
                        })),
                )
        }
        NodeKind::Gain { .. } => div()
            .h_flex()
            .gap_1()
            .child(
                a11y::button(format!("gain-down-{id}"))
                    .outline()
                    .small()
                    .label("−")
                    .accessibility_label(format!("{name}: −1 dB"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let request = this.routing.nudge_gain(id, -1.);
                        this.send_audio(request);
                        cx.notify();
                    })),
            )
            .child(
                a11y::button(format!("gain-up-{id}"))
                    .outline()
                    .small()
                    .label("+")
                    .accessibility_label(format!("{name}: +1 dB"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let request = this.routing.nudge_gain(id, 1.);
                        this.send_audio(request);
                        cx.notify();
                    })),
            ),
        NodeKind::Input | NodeKind::Output => div(),
    };
    div()
        .h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .v_flex()
                .min_w_0()
                .child(div().text_sm().child(name))
                .children(problem.map(|problem| div().text_xs().text_color(danger).child(problem)))
                .child(div().text_xs().text_color(muted).child(String::new())),
        )
        .child(div().h_flex().gap_2().child(controls).child(remove))
}

/// Every wire, named by the nodes it joins, with a button to remove it.
fn connections_list(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let routing = &view.routing;
    let muted = cx.theme().muted_foreground;
    let graph = routing.graph.clone().unwrap_or(plugin_graph::GraphSpec {
        nodes: vec![],
        edges: vec![],
    });
    let name = |id: NodeId| {
        graph
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map_or_else(String::new, |node| title(node).0)
    };
    let rows = graph
        .edges
        .iter()
        .enumerate()
        .map(|(index, edge)| {
            let edge = *edge;
            let label = format!(
                "{} {} → {} {}",
                name(edge.from.node),
                edge.from.port + 1,
                name(edge.to.node),
                edge.to.port + 1,
            );
            div()
                .id(SharedString::from(format!("wire-{index}")))
                .test_support()
                .accessibility_id(SharedString::from(format!("wire-{index}")))
                .role(Role::ListItem)
                .aria_label(label.clone())
                .h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(div().text_sm().child(label.clone()))
                .child(
                    a11y::button(format!("remove-wire-{index}"))
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .tooltip(rust_i18n::t!("routing.remove").to_string())
                        .accessibility_label(format!("{} {label}", rust_i18n::t!("routing.remove")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let request = this.routing.remove_wire(edge);
                            this.send_audio(request);
                            cx.notify();
                        })),
                )
        })
        .collect::<Vec<_>>();
    let body = match rows.is_empty() {
        true => div()
            .text_sm()
            .text_color(muted)
            .child(rust_i18n::t!("routing.no_connections").to_string())
            .into_any_element(),
        false => div().v_flex().gap_1().children(rows).into_any_element(),
    };
    // Wiring without the canvas: pick an output and an input, then connect.
    let label = |port: Option<plugin_graph::Port>, input: bool| match port {
        Some(port) => format!(
            "{} {} {}",
            name(port.node),
            match input {
                true => rust_i18n::t!("routing.port_in"),
                false => rust_i18n::t!("routing.port_out"),
            },
            port.port + 1
        ),
        None => String::new(),
    };
    let (from, to) = (routing.picked_output(), routing.picked_input());
    let form = div()
        .h_flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(
            a11y::button("connect-from")
                .outline()
                .small()
                .label(format!(
                    "{}: {}",
                    rust_i18n::t!("routing.connect_from"),
                    label(from, false)
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.routing.next_output();
                    cx.notify();
                })),
        )
        .child(
            a11y::button("connect-to")
                .outline()
                .small()
                .label(format!(
                    "{}: {}",
                    rust_i18n::t!("routing.connect_to"),
                    label(to, true)
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.routing.next_input();
                    cx.notify();
                })),
        )
        .child(
            a11y::button("connect-add")
                .primary()
                .small()
                .icon(IconName::Plus)
                .label(rust_i18n::t!("routing.connect").to_string())
                .on_click(cx.listener(|this, _, _, cx| {
                    let request = this.routing.connect_picked();
                    this.send_audio(request);
                    cx.notify();
                })),
        );
    section("routing.connections", cx).child(body).child(form)
}

/// Record a few seconds and hear them before and after the plugins.
fn test_panel(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let routing = &view.routing;
    let recording = matches!(routing.test, TestState::Recording(_));
    let playing = view.playing.as_ref().map(|playing| playing.which);
    let record = a11y::button("record-test")
        .outline()
        .small()
        .icon(IconName::Mic)
        .label(match recording {
            true => rust_i18n::t!("routing.recording").to_string(),
            false => format!("{} ({TEST_SECONDS} s)", rust_i18n::t!("routing.record")),
        })
        .disabled(recording)
        .on_click(cx.listener(|this, _, _, cx| {
            this.playing = None;
            let request = this.routing.record_test(TEST_SECONDS);
            this.send_audio(request);
            cx.notify();
        }));
    let play = |id: &'static str, which: Which, key: &str, cx: &mut Context<AppView>| {
        a11y::button(id)
            .outline()
            .small()
            .icon(IconName::Play)
            .label(rust_i18n::t!(key).to_string())
            .selected(playing == Some(which))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.play(which);
                cx.notify();
            }))
    };
    let after = match routing.test {
        TestState::Ready(_) => Some(
            div()
                .h_flex()
                .gap_2()
                .child(play(
                    "play-original",
                    Which::Original,
                    "routing.play_original",
                    cx,
                ))
                .child(play(
                    "play-processed",
                    Which::Processed,
                    "routing.play_processed",
                    cx,
                ))
                .child(
                    a11y::button("stop-playback")
                        .ghost()
                        .small()
                        .icon(IconName::Square)
                        .label(rust_i18n::t!("routing.stop").to_string())
                        .disabled(playing.is_none())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.playing = None;
                            cx.notify();
                        })),
                ),
        ),
        TestState::Idle | TestState::Recording(_) => None,
    };
    section("routing.test", cx)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(rust_i18n::t!("routing.test_hint").to_string()),
        )
        .child(
            div()
                .h_flex()
                .flex_wrap()
                .gap_2()
                .child(record)
                .children(after),
        )
}
