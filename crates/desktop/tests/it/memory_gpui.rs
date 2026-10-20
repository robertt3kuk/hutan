#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Mawas's Memory page (MW-2): the page in the center with its entries, editor, history and
//! proposals, its entry points (⌘K "Mawas: Memory", the Today rail), the pending proposals under
//! Needs you, and Settings → Mawas, against a real in-process daemon with the in-memory
//! credential store and a loopback provider. Nothing calls a model.

use crate::common;

use common::assistant::{AssistantEnv, Options, open, open_assistant};
use common::{draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::{ApprovalId, SessionId, WorkspaceId};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::{ItemRef, PanelId};
use hutan_desktop::workbench::panels::{
    AssistantPanel, MemoryEditorTarget, MemoryPanel, MemoryScopeChoice, NeedsKind,
};
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

fn page() -> ItemRef {
    ItemRef::panel("mawas-memory")
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

fn center_pages(view: &Entity<Workbench>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |wb, _| {
        wb.state()
            .all_items()
            .into_iter()
            .filter(|i| *i == page())
            .count()
    })
}

// ---- entry points ---------------------------------------------------------------------------

#[gpui::test]
fn the_palette_and_the_today_rail_open_one_memory_page_in_the_center(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);

    // ⌘K lists it, and its action opens one tab of the center, in no dock.
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
    assert_eq!(center_pages(&view, cx), 1);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(page())
    );
    assert!(
        view.read_with(cx, |wb, _| wb
            .state()
            .docks
            .find(&PanelId::new("mawas-memory")))
            .is_none(),
        "a tab of the center, in no dock"
    );
    assert_eq!(view.read_with(cx, |wb, cx| wb.active_title(cx)), "Memory");
    // Again: still one instance.
    cx.dispatch_action(actions::OpenMawasMemory);
    assert_eq!(center_pages(&view, cx), 1);
    assert!(panel.read_with(cx, |p, _| p.entries().is_empty()));
    draw(cx);
    // An empty memory says what it is for.
    for region in ["add", "scope-all", "scope-global", "archived"] {
        assert!(
            view.read_with(cx, |wb, _| wb.region_bounds(Region::Memory(region)))
                .is_some(),
            "{region} is in the header"
        );
    }

    // Mawas Home links to it from the Today rail.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.close_panel(&PanelId::new("mawas-memory")).unwrap();
        })
    });
    assert_eq!(center_pages(&view, cx), 0);
    let _home = open_assistant(&view, cx);
    click(&view, cx, Region::Assistant("today-memory"));
    wait_until(cx, "the Memory page to open", |cx| {
        draw(cx);
        center_pages(&view, cx) == 1
            && view.read_with(cx, |wb, _| wb.state().active_center_item()) == Some(page())
    });
}

#[gpui::test]
fn turning_mawas_off_takes_the_memory_page_and_its_command_with_it(cx: &mut TestAppContext) {
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
    assert_eq!(center_pages(&view, cx), 1);
    settings
        .write_user("assistant.enabled", toml::Value::Boolean(false))
        .unwrap();
    wait_until(cx, "Mawas to go", |cx| {
        view.read_with(cx, |wb, _| {
            !wb.assistant_enabled() && !wb.panel_open("mawas-memory")
        })
    });
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.commands().iter().map(|c| c.title.to_string()).collect()
    });
    assert!(
        !titles.iter().any(|t| t.contains("Memory")),
        "no command still opens it: {titles:?}"
    );
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
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::MemoryItem(
            "inferred",
            row_of(&panel, cx, &kite.id)
        )))
        .is_some(),
        "the inferred badge is drawn"
    );

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
    click(&view, cx, Region::MemoryItem("edit", uses));
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
    let kite_row = row_of(&panel, cx, &kite.id);
    click(&view, cx, Region::MemoryItem("archive", kite_row));
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
    // The workspace has a layout of its own: the page opens there too (one instance).
    cx.dispatch_action(actions::OpenMawasMemory);
    wait_until(cx, "the page in the workspace's layout", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.state().active_center_item()) == Some(page())
    });
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
fn proposals_wait_on_the_page_and_under_needs_you_and_are_answered_from_either(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let home = home_panel(&view, cx);
    let mawas = home.read_with(cx, |p, cx| p.session(cx).unwrap());
    let first = propose(&env, mawas, "one", "Staging is called kite.");
    let second = propose(&env, mawas, "two", "REJECTED-CLAIM");
    let third = propose(&env, mawas, "three", "draft wording");

    // Under Needs you: memory proposals, each with its options and Edit & approve.
    wait_until(cx, "the proposals under Needs you", |cx| {
        home.read_with(cx, |p, cx| {
            p.today(cx)
                .needs_you
                .iter()
                .filter(|r| matches!(r.kind, NeedsKind::Approval { memory: true, .. }))
                .count()
                == 3
        })
    });
    let today = home.read_with(cx, |p, cx| p.today(cx));
    let NeedsKind::Approval {
        options,
        memory,
        mawas: by_mawas,
        ..
    } = &today.needs_you[0].kind
    else {
        panic!("an approval: {:?}", today.needs_you[0]);
    };
    assert!(*memory && *by_mawas);
    assert_eq!(
        options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["approve", "reject"]
    );
    assert!(
        today.needs_you[0].title.starts_with("Memory proposal:"),
        "{}",
        today.needs_you[0].title
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
fn a_card_under_needs_you_approves_a_proposal_and_edit_and_approve_opens_the_page_on_it(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let home = home_panel(&view, cx);
    let mawas = home.read_with(cx, |p, cx| p.session(cx).unwrap());
    let first = propose(&env, mawas, "one", "Prefers short answers.");
    wait_until(cx, "the card", |cx| {
        home.read_with(cx, |p, cx| p.today(cx).needs_you.len() == 1)
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
        home.read_with(cx, |p, cx| p.today(cx).needs_you.is_empty())
    });
    let applied = proposals_of(&env);
    assert_eq!(
        (applied[0].id, applied[0].state),
        (first, ProposalState::Applied)
    );

    // Edit & approve: opens the page with that proposal's editor, even from a closed page.
    let second = propose(&env, mawas, "two", "Staging is called kite.");
    wait_until(cx, "the second card", |cx| {
        home.read_with(cx, |p, cx| p.today(cx).needs_you.len() == 1)
    });
    draw(cx);
    click(&view, cx, Region::AssistantItem("today-edit-approve", 0));
    wait_until(cx, "the Memory page", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.state().active_center_item()) == Some(page())
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

    // Another tab takes the pane: the page is hidden.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.add_item(pane, ItemRef::placeholder("Elsewhere")).unwrap();
        })
    });
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
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.open_panel_in_center(&PanelId::new("mawas-memory"));
        })
    });
    wait_until(cx, "the page to catch up", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| {
            p.is_visible() && p.render_count() > before && p.entries().len() == 1
        })
    });
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
