#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Mawas's Memory page (MW-2, MEM-SIDEBAR): the page as a body of the left sidebar with its entries,
//! inline editor, history, proposals and keyboard, its entry points (⌘K "Mawas: Memory", the
//! *Memory* nav row, the Today rail), the pending proposals under Requests, the migration of a
//! layout that had it as a tab of the center, the layout at 240 and 480 px, and Settings →
//! Mawas, against a real in-process daemon with the in-memory credential store and a loopback
//! provider. Nothing calls a model.

use crate::common;

use common::assistant::{AssistantEnv, Options, open, open_assistant};
use common::{draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::{ApprovalId, SessionId, WorkspaceId};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::{
    DockMode, DockPosition, ItemRef, PanelDest, PanelId, WorkbenchState,
};
use hutan_desktop::workbench::panels::{
    AssistantPanel, MemoryCursor, MemoryEditorTarget, MemoryPanel, MemoryScopeChoice, TodayKind,
};
use hutan_desktop::workbench::persistence::STATE_FILE;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::chat::{ApprovalList, ApprovalListRequest};
use hutan_protocol::memory::{
    MemoryAuthor, MemoryCategory, MemoryEntries, MemoryEntry, MemoryList, MemoryOp,
    MemoryProposals, MemoryProposalsList, MemoryPropose, MemoryProposed, MemoryReceipt,
    MemoryScope, MemoryState, MemoryWrite, ProposalState,
};
use hutan_protocol::messages::Request;
use hutan_settings::{SettingsPaths, SettingsStore};

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    draw(cx);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
}

fn started() -> AssistantEnv {
    AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    })
}

fn page() -> PanelId {
    PanelId::new("mawas-memory")
}

fn remember(
    env: &AssistantEnv,
    scope: MemoryScope,
    workspace: Option<WorkspaceId>,
    category: MemoryCategory,
    body: &str,
) -> MemoryReceipt {
    env.daemon.call(Request::MemoryWrite(MemoryWrite {
        operation_id: ApprovalId::new().to_string(),
        scope,
        workspace_id: workspace,
        category,
        body: body.to_owned(),
    }))
}

fn stored(env: &AssistantEnv, archived: bool) -> Vec<MemoryEntry> {
    env.daemon
        .call::<MemoryEntries>(Request::MemoryList(MemoryList {
            include_archived: archived,
            ..MemoryList::default()
        }))
        .entries
}

fn proposals_of(env: &AssistantEnv) -> Vec<hutan_protocol::memory::MemoryProposal> {
    env.daemon
        .call::<MemoryProposals>(Request::MemoryProposalsList(MemoryProposalsList {
            include_resolved: true,
        }))
        .proposals
}

fn propose(env: &AssistantEnv, from: SessionId, op: &str, body: &str) -> ApprovalId {
    let pending = env.daemon.call_from_in_background::<MemoryProposed>(
        from,
        Request::MemoryPropose(MemoryPropose {
            operation_id: op.to_owned(),
            op: MemoryOp::Create,
            scope: Some(MemoryScope::Global),
            workspace_id: None,
            category: Some(MemoryCategory::Fact),
            entry_id: None,
            expected_revision: None,
            body: Some(body.to_owned()),
        }),
    );
    let mut proposed = None;
    for _ in 0..400 {
        proposed = pending.try_take();
        if proposed.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let proposed = proposed.expect("the proposal was answered").unwrap();
    assert_eq!(
        (proposed.status.as_str(), proposed.applied),
        ("proposed", false)
    );
    proposed.proposal_id
}

/// Opens the Memory page (the palette's action) and waits until it has listed.
fn open_memory(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<MemoryPanel> {
    cx.dispatch_action(actions::OpenMawasMemory);
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<MemoryPanel>("mawas-memory"))
        .expect("the Memory page is drawn");
    wait_until(cx, "the Memory page to list", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.is_loaded())
    });
    panel
}

fn wait_entries(panel: &Entity<MemoryPanel>, cx: &mut VisualTestContext, count: usize) {
    wait_until(cx, &format!("{count} entr(ies) on the page"), |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.entries().len() == count)
    });
}

/// Waits until the page lists `count` global entries (a workspace's own come on top when the
/// window shows that workspace).
fn wait_global(panel: &Entity<MemoryPanel>, cx: &mut VisualTestContext, count: usize) {
    wait_until(cx, &format!("{count} global entr(ies) on the page"), |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.entries()
                .iter()
                .filter(|e| e.scope == MemoryScope::Global)
                .count()
                == count
        })
    });
}

fn wait_proposals(panel: &Entity<MemoryPanel>, cx: &mut VisualTestContext, count: usize) {
    wait_until(cx, &format!("{count} proposal(s) on the page"), |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.proposals().len() == count)
    });
}

/// The row index the page draws entry `id` at (profile, then facts, then notes).
fn row_of(panel: &Entity<MemoryPanel>, cx: &VisualTestContext, id: &str) -> usize {
    panel.read_with(cx, |p, _| {
        let mut index = 0;
        for category in MemoryCategory::ALL {
            for entry in p.entries().iter().filter(|e| e.category == category) {
                if entry.id == id {
                    return index;
                }
                index += 1;
            }
        }
        panic!("{id} is not listed")
    })
}

/// Tabs of the center that are the Memory page: there is never one.
fn center_pages(view: &Entity<Workbench>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |wb, _| {
        wb.state()
            .panes()
            .iter()
            .flat_map(|pane| pane.items.iter())
            .filter(|item| item.as_panel() == Some(&page()))
            .count()
    })
}

/// The page is the body of the left sidebar: in its dock, the dock's page, expanded.
fn in_sidebar(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| {
        let state = wb.state();
        state.docks.find(&page()) == Some(DockPosition::Left)
            && state.docks.left.active == Some(page())
            && state.docks.left.mode() == DockMode::Expanded
    })
}

fn left_page(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<PanelId> {
    view.read_with(cx, |wb, _| wb.state().docks.left.active.clone())
}

/// The entry ids in the order the page draws them (profile, facts, notes).
fn entry_ids(panel: &Entity<MemoryPanel>, cx: &VisualTestContext) -> Vec<String> {
    panel.read_with(cx, |p, _| {
        let mut ids = Vec::new();
        for category in MemoryCategory::ALL {
            ids.extend(
                p.entries()
                    .iter()
                    .filter(|e| e.category == category)
                    .map(|e| e.id.clone()),
            );
        }
        ids
    })
}

fn cursor(panel: &Entity<MemoryPanel>, cx: &VisualTestContext) -> Option<MemoryCursor> {
    panel.read_with(cx, |p, _| p.cursor().cloned())
}

fn editing(panel: &Entity<MemoryPanel>, cx: &VisualTestContext) -> Option<MemoryEditorTarget> {
    panel.read_with(cx, |p, _| p.editor_target().cloned())
}

// ---- entry points ---------------------------------------------------------------------------

#[gpui::test]
fn every_entry_point_opens_memory_in_the_sidebar_and_never_as_a_center_tab(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);

    // ⌘K lists it, and its action shows Memory as the body of the left sidebar.
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.commands().iter().map(|c| c.title.to_string()).collect()
    });
    assert!(titles.iter().any(|t| t == "Mawas: Memory"), "{titles:?}");
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("Mawas: Memory");
    assert!(view.read_with(cx, |wb, _| {
        wb.palette_titles()
            .iter()
            .any(|t| t.as_ref() == "Mawas: Memory")
    }));
    cx.simulate_keystrokes("escape");
    let panel = open_memory(&view, cx);
    assert!(in_sidebar(&view, cx), "the left dock's page");
    assert_eq!(center_pages(&view, cx), 0, "not a tab");
    assert_ne!(
        view.read_with(cx, |wb, cx| wb.active_title(cx)),
        "Memory",
        "the page a sidebar shows never becomes the window's title"
    );
    // Again: still one instance, still no tab.
    cx.dispatch_action(actions::OpenMawasMemory);
    assert_eq!(center_pages(&view, cx), 0);
    assert!(panel.read_with(cx, |p, _| p.entries().is_empty()));
    draw(cx);
    for region in [
        "panel",
        "header",
        "add",
        "scope-all",
        "scope-global",
        "archived",
    ] {
        assert!(
            view.read_with(cx, |wb, _| wb.region_bounds(Region::Memory(region)))
                .is_some(),
            "{region} is on the page"
        );
    }

    // It replaced Projects and shows the way back (‹ Projects); the row under Mawas toggles it.
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Nav("projects")))
            .is_some()
    );
    click(&view, cx, Region::Nav("projects"));
    assert_eq!(left_page(&view, cx), Some(PanelId::new("sessions")));
    click(&view, cx, Region::Nav("memory"));
    assert!(in_sidebar(&view, cx), "the Memory row opens it");
    click(&view, cx, Region::Nav("memory"));
    assert_eq!(
        left_page(&view, cx),
        Some(PanelId::new("sessions")),
        "and closes it again"
    );

    // ⌘K → Panel: Open Memory.
    cx.dispatch_action(actions::OpenPanel("mawas-memory".into()));
    assert!(in_sidebar(&view, cx));
    assert_eq!(center_pages(&view, cx), 0);

    // A Memory the user left in the center by hand comes back to the sidebar.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.move_panel(&page(), PanelDest::Center).unwrap();
        })
    });
    assert_eq!(center_pages(&view, cx), 1);
    cx.dispatch_action(actions::OpenMawasMemory);
    assert!(in_sidebar(&view, cx));
    assert_eq!(center_pages(&view, cx), 0);

    // A compact sidebar (the icon rail) expands to show it.
    cx.dispatch_action(actions::ToggleLeftDock);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.left.mode()),
        DockMode::Compact
    );
    cx.dispatch_action(actions::OpenMawasMemory);
    assert!(in_sidebar(&view, cx));

    // Mawas Home links to it from the Today rail.
    click(&view, cx, Region::Nav("projects"));
    let _home = open_assistant(&view, cx);
    click(&view, cx, Region::Assistant("today-memory"));
    wait_until(cx, "the Memory page to open", |cx| {
        draw(cx);
        in_sidebar(&view, cx)
    });
    assert_eq!(center_pages(&view, cx), 0);
}

#[gpui::test]
fn turning_mawas_off_takes_the_memory_page_its_row_and_its_command_with_it(
    cx: &mut TestAppContext,
) {
    let env = started();
    let settings =
        common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(env.daemon.home())));
    let (view, cx) = open_with(cx, settings.clone(), env.daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    open_memory(&view, cx);
    assert!(in_sidebar(&view, cx));
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Nav("memory")))
            .is_some(),
        "the row under Mawas"
    );
    settings
        .write_user("assistant.enabled", toml::Value::Boolean(false))
        .unwrap();
    wait_until(cx, "Mawas to go", |cx| {
        view.read_with(cx, |wb, _| {
            !wb.assistant_enabled() && !wb.panel_open("mawas-memory")
        })
    });
    draw(cx);
    assert_eq!(
        left_page(&view, cx),
        Some(PanelId::new("sessions")),
        "Projects is back"
    );
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Nav("memory")))
            .is_none(),
        "the row went with Mawas"
    );
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.commands().iter().map(|c| c.title.to_string()).collect()
    });
    assert!(
        !titles.iter().any(|t| t.contains("Memory")),
        "no command still opens it: {titles:?}"
    );
}

#[gpui::test]
fn a_saved_layout_with_the_old_memory_tab_opens_with_memory_in_the_sidebar(
    cx: &mut TestAppContext,
) {
    let env = started();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    // A version 6 file: Memory was a tab of the center, the one on screen.
    let mut old = WorkbenchState::default();
    old.open_panel_in_center(&page());
    assert_eq!(
        old.active_center_item(),
        Some(ItemRef::panel("mawas-memory"))
    );
    let mut value: serde_json::Value = serde_json::from_str(&old.to_json().unwrap()).unwrap();
    value["version"] = 6.into();
    std::fs::write(&path, value.to_string()).unwrap();

    let settings =
        common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(env.daemon.home())));
    let (view, cx) = open_with(cx, settings, env.daemon.connection(), Some(path.clone()));
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    draw(cx);
    assert!(in_sidebar(&view, cx), "the page moved into the sidebar");
    assert_eq!(center_pages(&view, cx), 0, "and left the center");
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<MemoryPanel>("mawas-memory"))
        .expect("the page is drawn in the sidebar");
    wait_until(cx, "the page to list", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.is_loaded())
    });

    // The next save is version 10 and keeps it there: nothing moves twice.
    cx.dispatch_action(actions::SplitRight);
    view.update(cx, |wb, _| wb.flush_save());
    let saved = WorkbenchState::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(on_disk["version"], 10);
    assert!(
        saved
            .panes()
            .iter()
            .all(|p| p.items.iter().all(|i| i.as_panel() != Some(&page())))
    );
    assert_eq!(saved.docks.left.active, Some(page()));
}

// ---- entries --------------------------------------------------------------------------------

#[gpui::test]
fn the_page_lists_adds_edits_archives_confirms_and_shows_history(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let project = env.project("alpha");
    let workspace = env.daemon.create_workspace("alpha", &[&project]);
    let old = remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Fact,
        "Uses jj.",
    );
    remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Prefers short answers.",
    );
    remember(
        &env,
        MemoryScope::Workspace,
        Some(workspace.id),
        MemoryCategory::Note,
        "Releases come from trunk.",
    );
    // A model's entry: proposed from a Mawas chat, approved by the user.
    let mawas_view = open_assistant(&view, cx);
    let mawas = mawas_view.read_with(cx, |p, cx| p.session(cx).unwrap());
    let proposed = propose(&env, mawas, "p1", "Staging is called kite.");
    env.daemon
        .call::<hutan_protocol::messages::Empty>(Request::ApprovalRespond(
            hutan_protocol::chat::ApprovalRespond {
                approval_id: proposed,
                option_id: "approve".into(),
            },
        ));

    let panel = open_memory(&view, cx);
    // The page lists the global entries (and the shown workspace's own).
    wait_global(&panel, cx, 3);
    let listed: Vec<MemoryEntry> = panel.read_with(cx, |p, _| {
        p.entries()
            .iter()
            .filter(|e| e.scope == MemoryScope::Global)
            .cloned()
            .collect()
    });
    assert_eq!(
        listed.iter().map(|e| e.category).collect::<Vec<_>>(),
        [
            MemoryCategory::Profile,
            MemoryCategory::Fact,
            MemoryCategory::Fact
        ],
        "grouped: profile, then facts"
    );
    let kite = listed
        .iter()
        .find(|e| e.body.contains("kite"))
        .unwrap()
        .clone();
    assert!(
        kite.inferred,
        "a model wrote it and the user has not edited it"
    );
    // Selecting a row (a click) opens its editor under it and shows what one line cannot:
    // where it applies, the inferred badge and the actions.
    let kite_row = row_of(&panel, cx, &kite.id);
    click(&view, cx, Region::MemoryItem("entry", kite_row));
    assert_eq!(
        cursor(&panel, cx),
        Some(MemoryCursor::Entry(kite.id.clone()))
    );
    assert_eq!(
        editing(&panel, cx),
        Some(MemoryEditorTarget::Entry {
            id: kite.id.clone(),
            revision: kite.revision
        })
    );
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::MemoryItem("inferred", kite_row)))
            .is_some(),
        "the inferred badge is drawn"
    );
    click(&view, cx, Region::Memory("cancel"));
    assert_eq!(editing(&panel, cx), None);

    // Add: the editor opens, Save sends it, the page lists the daemon's entry.
    click(&view, cx, Region::Memory("add"));
    assert_eq!(
        panel.read_with(cx, |p, _| p.editor_target().cloned()),
        Some(MemoryEditorTarget::Add)
    );
    click(&view, cx, Region::Memory("editor-note"));
    // Empty text is not saved.
    click(&view, cx, Region::Memory("save"));
    assert!(panel.read_with(cx, |p, _| p.error().is_some()));
    panel.update(cx, |p, cx| p.set_editor_text("Deploys on friday.", cx));
    click(&view, cx, Region::Memory("save"));
    wait_until(cx, "the entry to be stored", |_| {
        stored(&env, false)
            .iter()
            .any(|e| e.body == "Deploys on friday.")
    });
    wait_global(&panel, cx, 4);
    assert!(
        panel.read_with(cx, |p, _| p.editor_target().is_none()),
        "the receipt closed the editor"
    );
    let added = stored(&env, false)
        .into_iter()
        .find(|e| e.body == "Deploys on friday.")
        .unwrap();
    assert_eq!(
        (added.category, added.provenance.client.as_deref()),
        (MemoryCategory::Note, Some("desktop"))
    );
    assert!(!added.inferred);

    // Edit: the editor holds the entry's text; saving makes revision 2.
    let uses = row_of(&panel, cx, &old.entry_id);
    click(&view, cx, Region::MemoryItem("entry", uses));
    assert_eq!(
        panel
            .read_with(cx, |p, _| p.editor_text().map(str::to_owned))
            .as_deref(),
        Some("Uses jj.")
    );
    panel.update(cx, |p, cx| p.set_editor_text("Uses jj and git.", cx));
    click(&view, cx, Region::Memory("save"));
    wait_until(cx, "revision 2", |_| {
        stored(&env, false)
            .iter()
            .any(|e| e.id == old.entry_id && e.revision == 2)
    });
    wait_until(cx, "the page to show it", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.entries().iter().any(|e| e.body == "Uses jj and git.")
        })
    });

    // History: the row opens the revisions below it.
    let uses = row_of(&panel, cx, &old.entry_id);
    click(&view, cx, Region::MemoryItem("history", uses));
    wait_until(cx, "the history", |cx| {
        panel.read_with(cx, |p, _| {
            p.history_revisions().is_some_and(|r| r.len() == 2)
        })
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p.history_revisions().unwrap()[1].body.clone()),
        "Uses jj."
    );
    click(&view, cx, Region::MemoryItem("history", uses));
    assert!(
        panel.read_with(cx, |p, _| p.history_revisions().is_none()),
        "closed again"
    );

    // Confirm: saves the text as it is as the user's own; no longer inferred.
    let kite_row = row_of(&panel, cx, &kite.id);
    click(&view, cx, Region::MemoryItem("entry", kite_row));
    click(&view, cx, Region::MemoryItem("confirm", kite_row));
    wait_until(cx, "the confirmed entry", |_| {
        stored(&env, false)
            .iter()
            .any(|e| e.id == kite.id && !e.inferred && e.revision == 2)
    });
    // The page has caught up (the rows may have moved: the confirmed entry changed last).
    wait_until(cx, "the page to show the confirmation", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.entries().iter().any(|e| e.id == kite.id && !e.inferred)
        })
    });

    // Archive hides it; the archived view shows it, with Restore.
    // Archive asks first: Cancel leaves it, Archive archives it.
    let kite_row = row_of(&panel, cx, &kite.id);
    click(&view, cx, Region::MemoryItem("archive", kite_row));
    assert_eq!(
        panel.read_with(cx, |p, _| p.archive_question().map(str::to_owned)),
        Some(kite.id.clone())
    );
    click(&view, cx, Region::MemoryItem("archive-cancel", kite_row));
    assert!(!panel.read_with(cx, |p, _| p.archive_question().is_some()));
    assert!(
        stored(&env, false).iter().any(|e| e.id == kite.id),
        "Cancel archived nothing"
    );
    click(&view, cx, Region::MemoryItem("archive", kite_row));
    click(&view, cx, Region::MemoryItem("archive-yes", kite_row));
    wait_until(cx, "the archive", |_| {
        stored(&env, false).iter().all(|e| e.id != kite.id)
    });
    wait_global(&panel, cx, 3);
    click(&view, cx, Region::Memory("archived"));
    wait_until(cx, "the archived entry to be listed", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.shows_archived()
                && p.entries()
                    .iter()
                    .any(|e| e.id == kite.id && e.state == MemoryState::Archived)
        })
    });
    let kite_row = row_of(&panel, cx, &kite.id);
    click(&view, cx, Region::MemoryItem("entry", kite_row));
    assert_eq!(editing(&panel, cx), None, "an archived entry is not edited");
    click(&view, cx, Region::MemoryItem("restore", kite_row));
    wait_until(cx, "the restore", |_| {
        stored(&env, false).iter().any(|e| e.id == kite.id)
    });
    let history = env
        .daemon
        .call::<hutan_protocol::memory::MemoryHistory>(Request::MemoryHistory(
            hutan_protocol::memory::MemoryHistoryRequest {
                entry_id: kite.id.clone(),
            },
        ));
    assert_eq!(
        history.revisions.len(),
        4,
        "create, confirm, archive, restore"
    );
    assert_eq!(history.revisions[3].provenance.author, MemoryAuthor::Agent);

    // A change from elsewhere shows up without polling (`memory.changed`).
    remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Likes tabs.",
    );
    wait_until(cx, "the other client's entry", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.entries().iter().any(|e| e.body == "Likes tabs.")
        })
    });

    // The workspace filter lists the workspace's own notes with the global ones.
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx)
    });
    wait_until(cx, "the workspace to be active", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).active_project().is_some()
        })
    });
    // The workspace has a layout of its own: the page opens there too (one instance), in its
    // sidebar.
    cx.dispatch_action(actions::OpenMawasMemory);
    wait_until(cx, "the page in the workspace's layout", |cx| {
        draw(cx);
        in_sidebar(&view, cx)
    });
    assert_eq!(center_pages(&view, cx), 0);
    click(&view, cx, Region::Memory("scope-workspace"));
    wait_until(cx, "the workspace's notes", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.filter() == MemoryScopeChoice::Workspace
                && !p.entries().is_empty()
                && p.entries()
                    .iter()
                    .all(|e| e.scope == MemoryScope::Workspace)
        })
    });
    click(&view, cx, Region::Memory("scope-all"));
    wait_until(cx, "both scopes", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.entries()
                .iter()
                .any(|e| e.scope == MemoryScope::Workspace)
                && p.entries().iter().any(|e| e.scope == MemoryScope::Global)
        })
    });
}

// ---- proposals ------------------------------------------------------------------------------

fn home_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<AssistantPanel> {
    open_assistant(view, cx)
}

#[gpui::test]
fn proposals_wait_on_the_page_and_under_requests_and_are_answered_from_either(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let home = home_panel(&view, cx);
    let mawas = home.read_with(cx, |p, cx| p.session(cx).unwrap());
    let first = propose(&env, mawas, "one", "Staging is called kite.");
    let second = propose(&env, mawas, "two", "REJECTED-CLAIM");
    let third = propose(&env, mawas, "three", "draft wording");

    // Under Requests: memory proposals, each with its options and Edit & approve.
    wait_until(cx, "the proposals under Requests", |cx| {
        home.read_with(cx, |p, cx| {
            p.today(cx)
                .requests
                .iter()
                .filter(|r| matches!(r.kind, TodayKind::Approval { memory: true, .. }))
                .count()
                == 3
        })
    });
    let today = home.read_with(cx, |p, cx| p.today(cx));
    let TodayKind::Approval {
        options,
        memory,
        mawas: by_mawas,
        ..
    } = &today.requests[0].kind
    else {
        panic!("an approval: {:?}", today.requests[0]);
    };
    assert!(*memory && *by_mawas);
    assert_eq!(
        options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["approve", "reject"]
    );
    assert!(
        today.requests[0].title.starts_with("Memory proposal:"),
        "{}",
        today.requests[0].title
    );
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::AssistantItem("today-edit-approve", 0)))
            .is_some(),
        "Edit & approve is on the card"
    );

    // The page lists them on top.
    let panel = open_memory(&view, cx);
    wait_proposals(&panel, cx, 3);
    assert!(
        panel.read_with(cx, |p, _| p.entries().is_empty()),
        "nothing is saved yet"
    );
    for region in ["proposal", "approve", "edit-approve", "reject"] {
        assert!(
            view.read_with(cx, |wb, _| wb.region_bounds(Region::MemoryItem(region, 0)))
                .is_some(),
            "{region} is on the card"
        );
    }

    // Approve from the page: applied exactly as proposed.
    click(&view, cx, Region::MemoryItem("approve", 0));
    wait_until(cx, "the entry", |_| {
        stored(&env, false)
            .iter()
            .any(|e| e.body == "Staging is called kite." && e.inferred)
    });
    wait_proposals(&panel, cx, 2);
    wait_entries(&panel, cx, 1);

    // Reject from the page: nothing is saved, the proposal is rejected.
    click(&view, cx, Region::MemoryItem("reject", 0));
    wait_proposals(&panel, cx, 1);
    assert!(
        stored(&env, true)
            .iter()
            .all(|e| !e.body.contains("REJECTED"))
    );
    let states: Vec<ProposalState> = proposals_of(&env).iter().map(|p| p.state).collect();
    assert!(states.contains(&ProposalState::Rejected), "{states:?}");
    let _ = (first, second);

    // "Edit & approve" from the page: the editor holds the proposal's text.
    click(&view, cx, Region::MemoryItem("edit-approve", 0));
    assert_eq!(
        panel.read_with(cx, |p, _| p.editor_target().cloned()),
        Some(MemoryEditorTarget::Proposal(third))
    );
    assert_eq!(
        panel
            .read_with(cx, |p, _| p.editor_text().map(str::to_owned))
            .as_deref(),
        Some("draft wording")
    );
    panel.update(cx, |p, cx| p.set_editor_text("Final wording.", cx));
    click(&view, cx, Region::Memory("save"));
    wait_until(cx, "the edited entry", |_| {
        stored(&env, false).iter().any(|e| {
            e.body == "Final wording." && !e.inferred && e.provenance.author == MemoryAuthor::User
        })
    });
    wait_proposals(&panel, cx, 0);
    wait_entries(&panel, cx, 2);
}

#[gpui::test]
fn a_card_under_requests_approves_a_proposal_and_edit_and_approve_opens_the_page_on_it(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let home = home_panel(&view, cx);
    let mawas = home.read_with(cx, |p, cx| p.session(cx).unwrap());
    let first = propose(&env, mawas, "one", "Prefers short answers.");
    wait_until(cx, "the card", |cx| {
        home.read_with(cx, |p, cx| p.today(cx).requests.len() == 1)
    });
    draw(cx);
    // Approve: the first option of the card.
    click(&view, cx, Region::AssistantItem("today-option", 0));
    wait_until(cx, "the entry", |_| {
        stored(&env, false)
            .iter()
            .any(|e| e.body == "Prefers short answers.")
    });
    wait_until(cx, "the card to go", |cx| {
        home.read_with(cx, |p, cx| p.today(cx).requests.is_empty())
    });
    let applied = proposals_of(&env);
    assert_eq!(
        (applied[0].id, applied[0].state),
        (first, ProposalState::Applied)
    );

    // Edit & approve: opens the page with that proposal's editor, even from a closed page.
    let second = propose(&env, mawas, "two", "Staging is called kite.");
    wait_until(cx, "the second card", |cx| {
        home.read_with(cx, |p, cx| p.today(cx).requests.len() == 1)
    });
    draw(cx);
    click(&view, cx, Region::AssistantItem("today-edit-approve", 0));
    wait_until(cx, "the Memory page", |cx| {
        draw(cx);
        in_sidebar(&view, cx)
    });
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<MemoryPanel>("mawas-memory"))
        .unwrap();
    wait_until(cx, "its editor", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.editor_target() == Some(&MemoryEditorTarget::Proposal(second))
        })
    });
    assert_eq!(
        panel
            .read_with(cx, |p, _| p.editor_text().map(str::to_owned))
            .as_deref(),
        Some("Staging is called kite.")
    );
    // The approval is the daemon's until the user answers it.
    let approvals: ApprovalList = env
        .daemon
        .call(Request::ApprovalList(ApprovalListRequest::default()));
    assert!(approvals.approvals.iter().any(|a| a.id == second));
}

// ---- hidden ---------------------------------------------------------------------------------

#[gpui::test]
fn a_hidden_memory_page_does_no_work_and_catches_up_when_shown(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_memory(&view, cx);
    draw(cx);
    assert!(panel.read_with(cx, |p, _| p.is_visible()));

    // Back to Projects: the page leaves the screen and is hidden.
    click(&view, cx, Region::Nav("projects"));
    wait_until(cx, "the page to be hidden", |cx| {
        draw(cx);
        !panel.read_with(cx, |p, _| p.is_visible())
    });
    let before = panel.read_with(cx, |p, _| p.render_count());

    // The memory changes and the daemon's sessions change: a hidden page is not woken.
    remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Prefers short answers.",
    );
    let sessions_before = view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().len());
    env.daemon.sh("sleep 600");
    wait_until(cx, "the store to take the session", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).sessions().len() > sessions_before
        })
    });
    for _ in 0..5 {
        draw(cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        panel.read_with(cx, |p, _| p.render_count()),
        before,
        "a hidden page is not drawn again"
    );
    assert!(
        panel.read_with(cx, |p, _| p.entries().is_empty()),
        "and has not listed anything"
    );

    // Shown again, it lists and draws.
    cx.dispatch_action(actions::OpenMawasMemory);
    wait_until(cx, "the page to catch up", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.is_visible() && p.render_count() > before && p.entries().len() == 1
        })
    });
}
// ---- keyboard -------------------------------------------------------------------------------

#[gpui::test]
fn the_keyboard_moves_through_proposals_and_entries_edits_saves_archives_and_answers(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let profile = remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Prefers short answers.",
    );
    let jj = remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Fact,
        "Uses jj.",
    );
    remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Fact,
        "Deploys on friday.",
    );
    let home = home_panel(&view, cx);
    let mawas = home.read_with(cx, |p, cx| p.session(cx).unwrap());
    propose(&env, mawas, "one", "Staging is called kite.");
    propose(&env, mawas, "two", "REJECTED-CLAIM");

    let panel = open_memory(&view, cx);
    wait_proposals(&panel, cx, 2);
    wait_global(&panel, cx, 3);
    draw(cx);
    let waiting: Vec<ApprovalId> =
        panel.read_with(cx, |p, _| p.proposals().iter().map(|p| p.id).collect());
    let ids = entry_ids(&panel, cx);
    assert_eq!(ids[0], profile.entry_id, "profile first");
    assert_eq!(cursor(&panel, cx), None);

    // ↓ goes to the topmost thing (the first proposal) and stops at the last; ↑ goes back.
    cx.simulate_keystrokes("down");
    assert_eq!(cursor(&panel, cx), Some(MemoryCursor::Proposal(waiting[0])));
    cx.simulate_keystrokes("down");
    assert_eq!(cursor(&panel, cx), Some(MemoryCursor::Proposal(waiting[1])));
    cx.simulate_keystrokes("down");
    assert_eq!(cursor(&panel, cx), Some(MemoryCursor::Proposal(waiting[1])));
    cx.simulate_keystrokes("up");
    assert_eq!(cursor(&panel, cx), Some(MemoryCursor::Proposal(waiting[0])));

    // Tab moves between the proposals and the entries; ↓↑ stay in the group they are in.
    cx.simulate_keystrokes("tab");
    assert_eq!(
        cursor(&panel, cx),
        Some(MemoryCursor::Entry(ids[0].clone()))
    );
    cx.simulate_keystrokes("down");
    assert_eq!(
        cursor(&panel, cx),
        Some(MemoryCursor::Entry(ids[1].clone()))
    );
    cx.simulate_keystrokes("tab");
    assert_eq!(cursor(&panel, cx), Some(MemoryCursor::Proposal(waiting[0])));
    cx.simulate_keystrokes("tab");
    assert_eq!(
        cursor(&panel, cx),
        Some(MemoryCursor::Entry(ids[0].clone()))
    );
    assert_eq!(editing(&panel, cx), None, "moving only selects");

    // Return edits; ⌘Return saves; the editor closes with the daemon's receipt.
    cx.simulate_keystrokes("down");
    cx.simulate_keystrokes("enter");
    let revision = panel.read_with(cx, |p, _| {
        p.entries()
            .iter()
            .find(|e| e.id == ids[1])
            .unwrap()
            .revision
    });
    assert_eq!(
        editing(&panel, cx),
        Some(MemoryEditorTarget::Entry {
            id: ids[1].clone(),
            revision
        })
    );
    assert!(
        panel
            .read_with(cx, |p, _| p.editor_text().map(str::to_owned))
            .is_some_and(|t| !t.is_empty()),
        "the editor holds the entry's text"
    );
    panel.update(cx, |p, cx| {
        p.set_editor_text("Edited from the keyboard.", cx)
    });
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "the saved revision", |_| {
        stored(&env, false)
            .iter()
            .any(|e| e.body == "Edited from the keyboard." && e.revision == 2)
    });
    wait_until(cx, "the editor to close", |cx| {
        draw(cx);
        editing(&panel, cx).is_none()
    });

    // Esc closes the editor and leaves the entry as it was.
    let before = stored(&env, false).len();
    cx.simulate_keystrokes("enter");
    assert!(editing(&panel, cx).is_some());
    panel.update(cx, |p, cx| p.set_editor_text("Never saved.", cx));
    cx.simulate_keystrokes("escape");
    assert_eq!(editing(&panel, cx), None);
    assert!(stored(&env, false).iter().all(|e| e.body != "Never saved."));
    assert_eq!(stored(&env, false).len(), before);
    assert!(cursor(&panel, cx).is_some(), "the selection stays");

    // Writing something and clicking another row does not lose it.
    cx.simulate_keystrokes("enter");
    panel.update(cx, |p, cx| p.set_editor_text("Half a thought", cx));
    click(&view, cx, Region::MemoryItem("entry", 0));
    assert!(
        panel.read_with(cx, |p, _| p.error().is_some()),
        "told to save or cancel first"
    );
    assert_eq!(
        panel
            .read_with(cx, |p, _| p.editor_text().map(str::to_owned))
            .as_deref(),
        Some("Half a thought")
    );
    cx.simulate_keystrokes("escape");

    // ⌘⌫ asks first; Return does not answer yes; the second ⌘⌫ archives.
    let victim = match cursor(&panel, cx) {
        Some(MemoryCursor::Entry(id)) => id,
        other => panic!("an entry is selected: {other:?}"),
    };
    cx.simulate_keystrokes("cmd-backspace");
    assert_eq!(
        panel.read_with(cx, |p, _| p.archive_question().map(str::to_owned)),
        Some(victim.clone())
    );
    cx.simulate_keystrokes("enter");
    assert!(
        !panel.read_with(cx, |p, _| p.archive_question().is_some()),
        "Return cancels the question"
    );
    assert!(stored(&env, false).iter().any(|e| e.id == victim));
    cx.simulate_keystrokes("cmd-backspace");
    cx.simulate_keystrokes("cmd-backspace");
    wait_until(cx, "the archive", |_| {
        stored(&env, false).iter().all(|e| e.id != victim)
    });
    let _ = jj;

    // On a proposal: ⌘Return approves it as proposed, ⌘⌫ rejects it.
    wait_until(cx, "the cursor to settle", |cx| {
        draw(cx);
        cursor(&panel, cx).is_none()
    });
    cx.simulate_keystrokes("tab");
    assert_eq!(cursor(&panel, cx), Some(MemoryCursor::Proposal(waiting[0])));
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "the approved entry", |_| {
        stored(&env, false)
            .iter()
            .any(|e| e.body == "Staging is called kite.")
    });
    wait_proposals(&panel, cx, 1);
    cx.simulate_keystrokes("down");
    assert_eq!(cursor(&panel, cx), Some(MemoryCursor::Proposal(waiting[1])));
    cx.simulate_keystrokes("cmd-backspace");
    wait_proposals(&panel, cx, 0);
    assert!(
        stored(&env, true)
            .iter()
            .all(|e| !e.body.contains("REJECTED")),
        "a rejected proposal stores nothing"
    );
}

// ---- layout ---------------------------------------------------------------------------------

fn rect(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> gpui::Bounds<gpui::Pixels> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} is laid out"))
}

fn px_of(value: gpui::Pixels) -> f32 {
    f32::from(value)
}

#[track_caller]
fn assert_inside(what: &str, inner: gpui::Bounds<gpui::Pixels>, outer: gpui::Bounds<gpui::Pixels>) {
    assert!(
        px_of(inner.left()) >= px_of(outer.left()) - 0.5
            && px_of(inner.right()) <= px_of(outer.right()) + 0.5,
        "{what} {inner:?} leaves {outer:?}"
    );
}

#[track_caller]
fn assert_apart(what: &str, a: gpui::Bounds<gpui::Pixels>, b: gpui::Bounds<gpui::Pixels>) {
    let apart = px_of(a.right()) <= px_of(b.left()) + 0.5
        || px_of(b.right()) <= px_of(a.left()) + 0.5
        || px_of(a.bottom()) <= px_of(b.top()) + 0.5
        || px_of(b.bottom()) <= px_of(a.top()) + 0.5;
    assert!(apart, "{what}: {a:?} overlaps {b:?}");
}

#[gpui::test]
fn the_page_fits_the_sidebar_at_240_and_480_px_without_overlap(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let project = env.project("a-project-with-a-rather-long-name");
    let workspace = env
        .daemon
        .create_workspace("a-workspace-with-a-rather-long-name-indeed", &[&project]);
    let long = "Prefers answers that are short, with the reason first and the command after it, never a table.";
    remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        long,
    );
    remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Fact,
        "Uses jj.",
    );
    remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Fact,
        "Staging is reached through the bastion host named kite-with-a-long-name.internal.example.org",
    );
    remember(
        &env,
        MemoryScope::Workspace,
        Some(workspace.id),
        MemoryCategory::Note,
        "Releases come from trunk, never from a branch, and are tagged by hand.",
    );
    let home = home_panel(&view, cx);
    let mawas = home.read_with(cx, |p, cx| p.session(cx).unwrap());
    propose(
        &env,
        mawas,
        "one",
        "Staging is called kite and lives behind a bastion host that only the platform team may reach.",
    );
    propose(
        &env,
        mawas,
        "two",
        "A-single-unbroken-claim-without-any-spaces-that-is-longer-than-the-sidebar",
    );
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx)
    });
    wait_until(cx, "the workspace to be active", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).active_project().is_some()
        })
    });
    let panel = open_memory(&view, cx);
    wait_proposals(&panel, cx, 2);
    wait_entries(&panel, cx, 4);

    for width in [240.0_f32, 480.0] {
        view.update(cx, |wb, cx| {
            wb.edit_layout(cx, |s| s.docks.left.size = width)
        });
        // Nothing selected: one-line rows.
        click(&view, cx, Region::MemoryItem("entry", 0));
        click(&view, cx, Region::Memory("cancel"));
        draw(cx);
        draw(cx);
        let dock = rect(&view, cx, Region::Dock(DockPosition::Left));
        assert!(
            (px_of(dock.size.width) - width).abs() <= 2.0,
            "the sidebar is {width} wide, not {:?}",
            dock.size.width
        );
        let page = rect(&view, cx, Region::Memory("panel"));
        assert_inside("the page", page, dock);

        // The header and the filter: all in the page, none on another.
        let header = rect(&view, cx, Region::Memory("header"));
        assert_inside("the header", header, page);
        let add = rect(&view, cx, Region::Memory("add"));
        let filters = rect(&view, cx, Region::Memory("filters"));
        assert_inside("Add", add, page);
        assert_inside("the filters", filters, page);
        assert_apart("Add and the filters", add, filters);
        let chips = ["scope-all", "scope-global", "scope-workspace", "archived"]
            .map(|name| (name, rect(&view, cx, Region::Memory(name))));
        for (name, bounds) in &chips {
            assert_inside(name, *bounds, filters);
        }
        for (i, (a_name, a)) in chips.iter().enumerate() {
            for (b_name, b) in &chips[i + 1..] {
                assert_apart(&format!("{a_name} and {b_name}"), *a, *b);
            }
        }

        // The proposals: cards one above the other, their buttons inside and apart.
        let mut above: Option<gpui::Bounds<gpui::Pixels>> = None;
        for index in 0..2 {
            let card = rect(&view, cx, Region::MemoryItem("proposal", index));
            assert_inside("a proposal", card, page);
            if let Some(above) = above {
                assert_apart("two proposals", above, card);
            }
            above = Some(card);
            let buttons = ["reject", "edit-approve", "approve"]
                .map(|name| (name, rect(&view, cx, Region::MemoryItem(name, index))));
            for (name, bounds) in &buttons {
                assert_inside(name, *bounds, card);
            }
            for (i, (a_name, a)) in buttons.iter().enumerate() {
                for (b_name, b) in &buttons[i + 1..] {
                    assert_apart(&format!("{a_name} and {b_name}"), *a, *b);
                }
            }
        }

        // The entries: one line each unless selected, however long the text.
        let mut above: Option<gpui::Bounds<gpui::Pixels>> = None;
        for index in 0..4 {
            let block = rect(&view, cx, Region::MemoryItem("block", index));
            let line = rect(&view, cx, Region::MemoryItem("entry", index));
            assert_inside("an entry", block, page);
            assert_inside("an entry's line", line, block);
            // Entry 0 holds the selection (the click above): its whole text shows.
            assert!(
                index == 0 || px_of(line.size.height) <= 40.0,
                "entry {index} is one line at {width} px: {line:?}"
            );
            if let Some(above) = above {
                assert_apart("two entries", above, block);
            }
            above = Some(block);
        }

        // A selected entry: its whole text and its editor under the row, all inside the page.
        click(&view, cx, Region::MemoryItem("entry", 0));
        draw(cx);
        let block = rect(&view, cx, Region::MemoryItem("block", 0));
        let editor = rect(&view, cx, Region::Memory("editor"));
        let save = rect(&view, cx, Region::Memory("save"));
        let cancel = rect(&view, cx, Region::Memory("cancel"));
        assert_inside("the block", block, page);
        assert_inside("the editor", editor, block);
        assert_inside("Save", save, editor);
        assert_inside("Cancel", cancel, editor);
        assert_apart("Save and Cancel", save, cancel);
        let details = rect(&view, cx, Region::MemoryItem("details", 0));
        assert_inside("the details", details, block);
        assert_apart("the editor and the details", editor, details);
        let next = rect(&view, cx, Region::MemoryItem("block", 1));
        assert_apart("the open entry and the next", block, next);
        for name in ["edit", "archive", "history"] {
            if let Some(bounds) =
                view.read_with(cx, |wb, _| wb.region_bounds(Region::MemoryItem(name, 0)))
            {
                assert_inside(name, bounds, details);
            }
        }

        // Add: its editor, scope and category choices fit as well.
        click(&view, cx, Region::Memory("cancel"));
        click(&view, cx, Region::Memory("add"));
        draw(cx);
        let editor = rect(&view, cx, Region::Memory("editor"));
        assert_inside("the new entry's editor", editor, page);
        let choices = [
            "editor-global",
            "editor-workspace",
            "editor-profile",
            "editor-fact",
            "editor-note",
        ]
        .map(|name| (name, rect(&view, cx, Region::Memory(name))));
        for (name, bounds) in &choices {
            assert_inside(name, *bounds, editor);
        }
        for (i, (a_name, a)) in choices.iter().enumerate() {
            for (b_name, b) in &choices[i + 1..] {
                assert_apart(&format!("{a_name} and {b_name}"), *a, *b);
            }
        }
        click(&view, cx, Region::Memory("cancel"));
    }
}

// ---- settings -------------------------------------------------------------------------------

#[gpui::test]
fn settings_mawas_turns_memory_off_and_sets_the_limit(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    cx.dispatch_action(actions::OpenSettingsFor("assistant".into()));
    draw(cx);
    wait_until(cx, "the daemon's settings", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            let state = wb.daemon_settings();
            state.entry("assistant.memory").is_some()
                && state.entry("assistant.memory_max_entries").is_some()
        })
    });
    // The page offers both; writing them reaches the daemon live.
    view.update(cx, |wb, cx| {
        wb.daemon_setting_set("assistant.memory", Some(serde_json::json!(false)), cx);
        wb.daemon_setting_set(
            "assistant.memory_max_entries",
            Some(serde_json::json!(5)),
            cx,
        );
    });
    wait_until(cx, "the daemon to take them", |cx| {
        let settings: hutan_protocol::daemon_settings::DaemonSettings = env.daemon.call(
            Request::DaemonSettingsGet(hutan_protocol::messages::Empty {}),
        );
        let value = |key: &str| {
            settings
                .entries
                .iter()
                .find(|e| e.key == key)
                .unwrap()
                .value
                .clone()
        };
        let _ = cx;
        value("assistant.memory") == serde_json::json!(false)
            && value("assistant.memory_max_entries") == serde_json::json!(5)
    });
}

// ---- memory files (MEMORY-FILES, protocol 0.46) --------------------------------------------------

fn memory_folder(env: &AssistantEnv) -> std::path::PathBuf {
    env.daemon
        .paths
        .default_home_folder
        .join(hutan_protocol::memory_files::GLOBAL_DIR)
}

fn wait_files(panel: &Entity<MemoryPanel>, cx: &mut VisualTestContext) {
    wait_until(cx, "the page to know where the memory files are", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.files_folder().is_some())
    });
}

fn memory_menu(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<(String, bool)> {
    wait_until(cx, "the Memory ⋯ menu to open", |cx| {
        if view.read_with(cx, |wb, _| wb.context_menu_entries().is_some()) {
            return true;
        }
        draw(cx);
        if let Some(bounds) = view.read_with(cx, |wb, _| wb.region_bounds(Region::Memory("more"))) {
            cx.simulate_click(bounds.center(), Modifiers::none());
        }
        false
    });
    let disabled = view.read_with(cx, |wb, _| wb.context_menu_disabled());
    view.read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the ⋯ menu")
        .into_iter()
        .map(|(label, _)| {
            let label = label.to_string();
            let off = disabled.iter().any(|d| *d == label);
            (label, off)
        })
        .collect()
}

#[gpui::test]
fn the_memory_menu_exports_imports_and_shows_the_files(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Fact,
        "Uses jj.",
    );
    let panel = open_memory(&view, cx);
    wait_entries(&panel, cx, 1);
    wait_files(&panel, cx);

    // The ⋯ menu: where the files are (greyed out), then the three actions.
    let menu = memory_menu(&view, cx);
    let labels: Vec<&str> = menu.iter().map(|(l, _)| l.as_str()).collect();
    assert!(
        labels[0].starts_with("Memory files: ") && labels[0].ends_with(".mawas/memory"),
        "{labels:?}"
    );
    assert_eq!(
        labels[1..],
        ["Export…", "Import…", "Show files"],
        "{labels:?}"
    );
    assert!(menu[0].1, "the folder line is greyed out");
    assert!(menu[1..].iter().all(|(_, off)| !off), "{menu:?}");

    // Show files: the Files panel, on the memory folder.
    let labels_full = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("open");
    let index = labels_full
        .iter()
        .position(|(l, _)| l.as_ref() == "Show files")
        .unwrap();
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(index, window, cx));
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .panel::<hutan_desktop::workbench::panels::FilesPanel>("files")
            .is_some()),
        "the Files panel came up"
    );

    // Export writes what the memory says, as the memory files are written.
    let out = tempfile::tempdir().unwrap();
    let exported = out.path().join("memory.md");
    panel.update(cx, |p, cx| p.export_to(exported.clone(), cx));
    wait_until(cx, "the export to finish", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.notice()
                .is_some_and(|n| n.starts_with("Exported 1 entry"))
        })
    });
    let text = std::fs::read_to_string(&exported).unwrap();
    assert!(
        text.contains("# Facts") && text.contains("Uses jj."),
        "{text}"
    );
    let json = out.path().join("memory.json");
    panel.update(cx, |p, cx| p.export_to(json.clone(), cx));
    wait_until(cx, "the JSON export", |cx| {
        draw(cx);
        json.is_file() && panel.read_with(cx, |p, _| p.notice().is_some())
    });
    assert!(
        std::fs::read_to_string(&json)
            .unwrap()
            .contains("hutan-memory")
    );

    // Import makes proposals and saves nothing.
    let incoming = out.path().join("incoming.md");
    std::fs::write(
        &incoming,
        "# Facts\n\n<!-- mawas:new -->\nThe build is `just check`.\n\n<!-- mawas:new -->\nThe key is sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD\n\n# Notes\n\n<!-- mawas:new -->\nDecision: monorepo.\n",
    )
    .unwrap();
    panel.update(cx, |p, cx| p.import_from(incoming, cx));
    wait_proposals(&panel, cx, 2);
    let notice = panel
        .read_with(cx, |p, _| p.notice().map(str::to_owned))
        .unwrap();
    assert!(
        notice.starts_with("Imported 2 proposals") && notice.contains("1 skipped"),
        "{notice}"
    );
    assert_eq!(
        stored(&env, false).len(),
        1,
        "nothing was saved by the import"
    );
    assert!(
        proposals_of(&env)
            .iter()
            .all(|p| p.provenance.origin.as_deref() == Some("import"))
    );
    // The card says where the proposal came from.
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::MemoryItem("proposal", 0)))
            .is_some()
    );
}

#[gpui::test]
fn an_edit_of_a_memory_file_is_a_proposal_on_the_page_and_a_refused_one_is_a_problem(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    remember(
        &env,
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Prefers short answers.",
    );
    let panel = open_memory(&view, cx);
    wait_entries(&panel, cx, 1);
    wait_files(&panel, cx);

    // The user edits the file; the daemon's watcher makes a proposal, the page lists it.
    let file = memory_folder(&env).join("profile.md");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace("short", "terse")).unwrap();
    wait_proposals(&panel, cx, 1);
    let proposal = panel.read_with(cx, |p, _| p.proposals()[0].clone());
    assert_eq!(proposal.body, "Prefers terse answers.");
    assert_eq!(proposal.provenance.origin.as_deref(), Some("file"));
    assert_eq!(
        stored(&env, false)[0].body,
        "Prefers short answers.",
        "never applied silently"
    );
    draw(cx);

    // Approving it on the page applies it.
    click(&view, cx, Region::MemoryItem("approve", 0));
    wait_until(cx, "the approved edit", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.entries()
                .first()
                .is_some_and(|e| e.body == "Prefers terse answers.")
        })
    });
    assert!(stored(&env, false)[0].body == "Prefers terse answers.");

    // A refused edit is a problem on the page, and the file stays as written.
    let secret = text.replace("short", "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD");
    std::fs::write(&file, &secret).unwrap();
    wait_until(cx, "the problem to show", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.problems().len() == 1)
    });
    let reason = panel.read_with(cx, |p, _| p.problems()[0].reason.clone());
    assert!(reason.contains("not a secret store"), "{reason}");
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::MemoryItem("problem", 0)))
            .is_some(),
        "the Problems card is drawn"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), secret);
    assert!(panel.read_with(cx, |p, _| p.proposals().is_empty()));
}

#[gpui::test]
fn settings_has_keep_memory_in_the_project(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    cx.dispatch_action(actions::OpenSettingsFor("assistant".into()));
    draw(cx);
    wait_until(cx, "the daemon's settings", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.daemon_settings()
                .entry("assistant.memory_files_in_project")
                .is_some()
        })
    });
    view.update(cx, |wb, cx| {
        wb.daemon_setting_set(
            "assistant.memory_files_in_project",
            Some(serde_json::json!(true)),
            cx,
        );
    });
    wait_until(cx, "the daemon to take it", |_| {
        let settings: hutan_protocol::daemon_settings::DaemonSettings = env.daemon.call(
            Request::DaemonSettingsGet(hutan_protocol::messages::Empty {}),
        );
        settings
            .entries
            .iter()
            .find(|e| e.key == "assistant.memory_files_in_project")
            .is_some_and(|e| e.value == serde_json::json!(true))
    });
}
