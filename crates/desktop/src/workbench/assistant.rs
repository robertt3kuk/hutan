//! Workbench behavior around Mawas Home (M16-B, MW-1): ⌘K (or a bound key, or the nav row) opens
//! and focuses it, a history hit that is a Mawas conversation shows it there, and "Send to
//! agent" of a terminal block becomes a structured block attachment when the target is Mawas.
//!
//! Home is a registered panel like the others (`panels::assistant`), shown as one tab of the
//! center; the workbench only reveals it, tells it whether it is on screen, hands it a session
//! or a chip, and puts the keyboard in its composer. Nothing here reads a terminal or a file:
//! chips describe a source and the daemon resolves them when the message is sent.

use gpui::{Context, Entity, Window};
use hutan_core::SessionId;

use super::actions;
use super::chat::ContextChip;
use super::model::PanelId;
use super::panels::{AssistantPanel, MawasMode, MemoryPanel, ScopeKey};
use super::view::Workbench;

impl Workbench {
    /// `assistant.enabled`: off hides Mawas (its nav row, commands and shortcuts).
    pub fn assistant_enabled(&self) -> bool {
        self.settings
            .get::<crate::settings::AssistantSettings>()
            .enabled
    }

    /// The setting changed: closes the panel, drops the commands and the key, and lets the
    /// global shortcut go (or brings them back).
    pub(crate) fn apply_assistant_enabled(&mut self, cx: &mut Context<Self>) {
        if !self.assistant_enabled() {
            for page in ["assistant", "mawas-memory"] {
                let id = PanelId::new(page);
                self.try_mutate(cx, |s| s.close_panel(&id));
                self.panel_views.remove(&id);
            }
        }
        self.commands = std::rc::Rc::new(super::actions::commands(
            &self.presets,
            &self.panels,
            self.assistant_enabled(),
        ));
        self.rebind_keys(cx);
        self.sync_hotkey(cx);
    }

    /// Reveals Mawas Home (as a tab of the center when it is closed) and returns it.
    pub(crate) fn assistant_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<AssistantPanel>> {
        if !self.assistant_enabled() {
            return None;
        }
        self.open_panel("assistant", cx);
        let view = self.panel_view(&PanelId::new("assistant"), window, cx)?;
        view.downcast::<AssistantPanel>().ok()
    }

    /// ⌘K "Mawas: Open" (and any key bound to it, and the nav row): shows Home with the chat of
    /// its scope (the first use asks the daemon to open it) and puts the keyboard in its
    /// composer. No terminal is started.
    pub fn open_assistant(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.assistant_enabled() {
            self.set_notice("Mawas is off (assistant.enabled)", cx);
            return;
        }
        let Some(panel) = self.assistant_panel(window, cx) else {
            self.set_notice("Mawas Home is not available", cx);
            return;
        };
        self.assistant_last = true;
        panel.update(cx, |panel, cx| {
            panel.ensure_open(cx);
            panel.focus_composer(cx);
        });
    }

    /// ⌘K "Mawas: Memory" (and the Today rail's link): the Memory page, a tab of the center (one
    /// instance, shown where it is when it is open already), listing what Mawas remembers.
    pub fn open_memory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.memory_panel(window, cx) else {
            return;
        };
        panel.update(cx, |panel, cx| panel.refresh(cx));
    }

    /// "Edit & approve" of a memory proposal in Today: the Memory page with that proposal's
    /// editor open.
    pub fn edit_memory_proposal(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.memory_panel(window, cx) else {
            return;
        };
        let id = id.to_owned();
        panel.update(cx, |panel, cx| panel.edit_proposal_by_id(&id, cx));
    }

    fn memory_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<MemoryPanel>> {
        if !self.assistant_enabled() {
            self.set_notice("Mawas is off (assistant.enabled)", cx);
            return None;
        }
        self.open_panel("mawas-memory", cx);
        let view = self.panel_view(&PanelId::new("mawas-memory"), window, cx)?;
        view.downcast::<MemoryPanel>().ok()
    }

    /// ⌘K "Mawas: New chat": Home, with a fresh conversation in its scope.
    pub fn new_mawas_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.assistant_enabled() {
            self.set_notice("Mawas is off (assistant.enabled)", cx);
            return;
        }
        let Some(panel) = self.assistant_panel(window, cx) else {
            return;
        };
        self.assistant_last = true;
        panel.update(cx, |panel, cx| {
            panel.new_chat(cx);
        });
    }

    /// Opens the menu of Home's scope dropdown at `position`: Global and the workspace, the
    /// shown one checked.
    pub fn show_assistant_scope_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.panel::<AssistantPanel>("assistant") else {
            return;
        };
        let entries = panel
            .read(cx)
            .scope_entries(cx)
            .into_iter()
            .map(|scope| {
                let mut entry = if scope.enabled {
                    let panel = panel.clone();
                    let choose = scope.choose;
                    Self::entry(scope.label, move |_, _, cx| {
                        panel.update(cx, |panel, cx| panel.choose(choose, cx));
                    })
                } else {
                    Self::disabled_entry(scope.label)
                };
                entry.checked = scope.selected;
                entry
            })
            .collect();
        self.open_menu(position, entries, window, cx);
    }

    /// Opens the menu of Home's mode chip at `position`: Ask before acting and Advisor, the
    /// current one checked. Choosing one switches `assistant.mode`; it applies from the next
    /// message.
    pub fn show_assistant_mode_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.panel::<AssistantPanel>("assistant") else {
            return;
        };
        let current = panel.read(cx).mode();
        let entries = MawasMode::ALL
            .into_iter()
            .map(|mode| {
                let panel = panel.clone();
                let mut entry = Self::entry(mode.label(), move |_, _, cx| {
                    panel.update(cx, |panel, cx| panel.set_mode(mode, cx));
                });
                entry.checked = current == Some(mode);
                entry
            })
            .collect();
        self.open_menu(position, entries, window, cx);
    }

    /// Opens Home's ⋯ menu at `position`: **New chat** (only below 640 pt, where the header
    /// has folded it; above, its button is in the header), **Stop Mawas** (greyed out while
    /// there is nothing to stop), **Today** (checked while it replaces the conversation below
    /// 640 pt; *Hide Today* / *Show Today* above) and **Backend: …**, the backend·model menu.
    pub fn show_assistant_more_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.panel::<AssistantPanel>("assistant") else {
            return;
        };
        let (folded, today_only, rail_open, can_stop, label) = {
            let page = panel.read(cx);
            (
                page.folded(),
                page.today_only(),
                page.rail_open(),
                page.can_stop(cx),
                page.backend_label(cx),
            )
        };
        let mut entries = Vec::new();
        if folded {
            let new = panel.clone();
            entries.push(Self::entry("New chat", move |_, _, cx| {
                new.update(cx, |panel, cx| panel.new_chat(cx));
            }));
        }
        entries.push(if can_stop {
            let stop = panel.clone();
            Self::entry("Stop Mawas", move |_, _, cx| {
                stop.update(cx, |panel, cx| panel.stop(cx));
            })
        } else {
            Self::disabled_entry("Stop Mawas")
        });
        let today = panel.clone();
        entries.push(if folded {
            let mut entry = Self::entry("Today", move |_, _, cx| {
                today.update(cx, |panel, cx| panel.toggle_today_only(cx));
            });
            entry.checked = today_only;
            entry
        } else {
            Self::entry(
                if rail_open {
                    "Hide Today"
                } else {
                    "Show Today"
                },
                move |_, _, cx| today.update(cx, |panel, cx| panel.toggle_rail(cx)),
            )
        });
        entries.push(Self::entry(
            format!("Backend: {label}"),
            move |_, window, cx| {
                window.dispatch_action(Box::new(actions::ShowAssistantModelMenu(position)), cx);
            },
        ));
        self.open_menu(position, entries, window, cx);
    }

    /// Opens the menu of Home's backend·model chip at `position`: Switch model…
    /// (the model of the running assistant) and the chooser of backends and models.
    pub fn show_assistant_model_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.panel::<AssistantPanel>("assistant") else {
            return;
        };
        let switch = match panel.read(cx).switchable_session(cx) {
            Some(session) => Self::entry("Switch model…", move |_, window, cx| {
                window.dispatch_action(
                    Box::new(actions::SwitchChatModelById(session.to_string())),
                    cx,
                );
            }),
            None => Self::disabled_entry("Switch model…"),
        };
        let chooser = Self::entry("Backends and models…", move |_, _, cx| {
            panel.update(cx, |panel, cx| {
                if panel.switcher().is_none() {
                    panel.toggle_switcher(cx);
                }
            });
        });
        self.open_menu(position, vec![switch, chooser], window, cx);
    }

    /// Whether `session` is a Mawas conversation.
    pub(crate) fn is_assistant(&self, session: SessionId, cx: &gpui::App) -> bool {
        self.sessions
            .read(cx)
            .session(session)
            .and_then(|info| info.chat.as_ref())
            .is_some_and(|chat| chat.is_assistant())
    }

    /// Shows the Mawas conversation `session` on Home, in its own scope, with
    /// the keyboard in its composer. `false` (and nothing shown) when it is not one.
    pub fn show_assistant_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.assistant_enabled() || !self.is_assistant(session, cx) {
            return false;
        }
        let Some(panel) = self.assistant_panel(window, cx) else {
            return false;
        };
        self.assistant_last = true;
        panel.update(cx, |panel, cx| {
            let shown = panel.show_session(session, cx);
            if shown {
                panel.focus_composer(cx);
            }
            shown
        })
    }

    /// Attaches `chip` to the composer of the assistant `session` and shows it.
    pub(crate) fn attach_to_assistant(
        &mut self,
        session: SessionId,
        chip: ContextChip,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.assistant_panel(window, cx) else {
            return;
        };
        self.assistant_last = true;
        panel.update(cx, |panel, cx| {
            if panel.attach_to_session(session, chip, cx) {
                panel.focus_composer(cx);
            }
        });
    }

    /// "Send to agent" of block `block` of terminal session `session` with no chat to send it to
    /// (or the Assistant picked from the menu): the block becomes a chip of the assistant of the
    /// terminal's workspace, which the daemon reads when the message is sent.
    pub(crate) fn send_block_to_assistant(
        &mut self,
        session: SessionId,
        block: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (terminal, title, key) = {
            let store = self.sessions.read(cx);
            let Some(info) = store.session(session) else {
                return;
            };
            let Some(terminal) = info.terminal_id else {
                return;
            };
            let key = store
                .workspace(info.workspace_id)
                .map_or(ScopeKey::Global, ScopeKey::of);
            (terminal, info.title.clone(), key)
        };
        let (command, exit_code) = self.block_facts(session, block, cx);
        let chip = ContextChip::block(&title, terminal, block, command.as_deref(), exit_code);
        let Some(panel) = self.assistant_panel(window, cx) else {
            return;
        };
        self.assistant_last = true;
        panel.update(cx, |panel, cx| {
            panel.attach_to_scope(key, chip, cx);
            panel.focus_composer(cx);
        });
    }

    /// What the terminal view knows of block `block`: its command and exit code.
    pub(crate) fn block_facts(
        &self,
        session: SessionId,
        block: u64,
        cx: &gpui::App,
    ) -> (Option<String>, Option<i32>) {
        self.terminal_view_of(session)
            .and_then(|view| {
                view.read(cx)
                    .model()
                    .blocks()
                    .get(block)
                    .map(|b| (b.command.clone(), b.exit_code))
            })
            .unwrap_or((None, None))
    }
}
