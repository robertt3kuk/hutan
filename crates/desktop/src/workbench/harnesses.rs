//! The harnesses (OMP, Claude Code, Codex) as the desktop knows them (ADR 0015, M8-B): which are
//! installed, which accounts are logged in, and the models each offers. One model serves the
//! new-agent picker, the Accounts page of the settings sheet and the Assistant panel; it asks
//! the daemon (`account.list`, `chat.models`) and holds names and plans, never a token.
//!
//! Logging in or out is the harness's own flow in a terminal session the daemon starts
//! (`account.login` / `account.logout`); the window shows that session, and `account.changed`
//! makes the listing ask again.

use std::collections::HashMap;

use gpui::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::*, px,
};
use hutan_protocol::account::{AccountInfo, AccountStatus};

use super::agents::Models;
use super::chat::{ButtonKind, Look, button};
use super::render::probe;
use super::ui::{self, Mascot, StyledExt as _};
use super::view::{Region, Workbench};
use crate::sessions::{AccountsChanged, SessionsStore};

/// OMP, over `omp acp`.
pub const OMP: &str = "omp";
/// Claude Code, over its ACP adapter.
pub const CLAUDE: &str = "claude";
/// Codex, over `codex app-server`.
pub const CODEX: &str = "codex";

/// A harness the desktop can start chats with, in the order the picker lists them: OMP first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Harness {
    pub id: &'static str,
    pub name: &'static str,
}

pub const HARNESSES: [Harness; 3] = [
    Harness {
        id: OMP,
        name: "OMP",
    },
    Harness {
        id: CLAUDE,
        name: "Claude Code",
    },
    Harness {
        id: CODEX,
        name: "Codex",
    },
];

/// The name of the harness `agent` (the id itself for one that is not known).
pub fn harness_name(agent: &str) -> String {
    HARNESSES
        .iter()
        .find(|h| h.id == agent)
        .map_or_else(|| agent.to_owned(), |h| h.name.to_owned())
}

/// Whether a harness could be found on the daemon's machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// The accounts have not been listed yet.
    Checking,
    /// The daemon found it (it may still be logged out).
    Installed,
    /// The daemon could not find it; the text says what to install or which `daemon.toml` key to
    /// set, in the daemon's words.
    NotFound(String),
    /// The daemon could not be asked.
    Unknown,
}

/// Where the account listing stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// Never asked.
    Idle,
    Loading,
    Ready,
    Failed(String),
}

/// What the daemon says about the harnesses; see the module documentation.
pub struct HarnessDirectory {
    sessions: Entity<SessionsStore>,
    accounts: Vec<AccountInfo>,
    listing: Listing,
    /// Counts the listings asked for; an answer to an older one is dropped.
    listings: u64,
    models: HashMap<String, Models>,
    /// Counts the model lists asked for per harness, like `listings`.
    model_requests: HashMap<String, u64>,
    /// Why the last login or logout could not start (the Accounts page shows it).
    problem: Option<String>,
    _subscription: Subscription,
}

impl std::fmt::Debug for HarnessDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessDirectory")
            .field("listing", &self.listing)
            .field("accounts", &self.accounts.len())
            .finish_non_exhaustive()
    }
}

impl HarnessDirectory {
    pub fn new(sessions: Entity<SessionsStore>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&sessions, |this, _, _: &AccountsChanged, cx| {
            // Somebody looks at the accounts: ask again. Nobody did yet: nothing to refresh.
            if this.listing != Listing::Idle {
                this.refresh(cx);
            }
        });
        Self {
            sessions,
            accounts: Vec::new(),
            listing: Listing::Idle,
            listings: 0,
            models: HashMap::new(),
            model_requests: HashMap::new(),
            problem: None,
            _subscription: subscription,
        }
    }

    // ---- reading ---------------------------------------------------------------------------

    pub fn listing(&self) -> &Listing {
        &self.listing
    }

    /// Every account the daemon listed: per harness the accounts it reports (a harness that
    /// could not be asked is one entry with status `unknown`).
    pub fn accounts(&self) -> &[AccountInfo] {
        &self.accounts
    }

    /// The accounts of `harness`.
    pub fn accounts_of<'a>(&'a self, harness: &'a str) -> impl Iterator<Item = &'a AccountInfo> {
        self.accounts.iter().filter(move |a| a.harness == harness)
    }

    /// The providers `harness` is logged in to, in the order it reported them.
    pub fn logged_in<'a>(&'a self, harness: &'a str) -> Vec<&'a AccountInfo> {
        self.accounts_of(harness)
            .filter(|a| a.status == AccountStatus::LoggedIn)
            .collect()
    }

    /// Whether the daemon found `harness`. A missing ACP adapter (Claude Code's chats run
    /// through `claude-code-acp`) shows when its models were asked for.
    pub fn detection(&self, harness: &str) -> Detection {
        if let Some(Models::Failed(message)) = self.models.get(harness)
            && message.contains("was not found")
        {
            return Detection::NotFound(message.clone());
        }
        let mut entries = self.accounts_of(harness).peekable();
        if entries.peek().is_none() {
            return match self.listing {
                Listing::Failed(_) => Detection::Unknown,
                _ => Detection::Checking,
            };
        }
        for entry in entries {
            if entry.status == AccountStatus::Unknown
                && let Some(detail) = entry.detail.as_deref()
                && detail.contains("was not found")
            {
                return Detection::NotFound(detail.to_owned());
            }
        }
        Detection::Installed
    }

    /// Whether chats with `harness` can be started as far as anyone knows.
    pub fn startable(&self, harness: &str) -> bool {
        !matches!(self.detection(harness), Detection::NotFound(_))
    }

    /// The models last asked of `harness`.
    pub fn models(&self, harness: &str) -> Option<&Models> {
        self.models.get(harness)
    }

    /// Why the last login or logout could not start.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub(crate) fn set_problem(&mut self, problem: Option<String>, cx: &mut Context<Self>) {
        self.problem = problem;
        cx.notify();
    }

    // ---- asking the daemon -----------------------------------------------------------------

    /// Lists the accounts again (`account.list`: it runs each harness's own status command, so
    /// it can take a moment).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let connection = self.sessions.read(cx).connection().clone();
        if !connection.supports_accounts() {
            self.listing = Listing::Failed(
                "Not connected to a hutand that lists harness accounts; restart it (Daemon → Restart)"
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        self.listings += 1;
        let listing = self.listings;
        self.listing = Listing::Loading;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let answer = connection.account_list().await.map_err(|e| e.to_string());
            this.update(cx, |this, cx| {
                if this.listings != listing {
                    return;
                }
                match answer {
                    Ok(accounts) => {
                        this.accounts = accounts;
                        this.listing = Listing::Ready;
                    }
                    Err(error) => this.listing = Listing::Failed(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Asks `harness` for its models (`chat.models`); the answer lands in [`models`](Self::models).
    pub fn load_models(&mut self, harness: &str, cx: &mut Context<Self>) {
        let request = self.model_requests.entry(harness.to_owned()).or_default();
        *request += 1;
        let request = *request;
        self.models.insert(harness.to_owned(), Models::Loading);
        cx.notify();
        let task = self
            .sessions
            .update(cx, |store, cx| store.chat_models(harness, cx));
        let harness = harness.to_owned();
        cx.spawn(async move |this, cx| {
            let answer = task.await;
            this.update(cx, |this, cx| {
                if this.model_requests.get(&harness) != Some(&request) {
                    return;
                }
                this.models.insert(
                    harness,
                    match answer {
                        Ok(models) => Models::Ready(models),
                        Err(error) => Models::Failed(error),
                    },
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// What a harness row of the picker or the Accounts page says about the logins: `anthropic,
/// openai-codex` or why there are none.
pub fn login_summary(directory: &HarnessDirectory, harness: &str) -> String {
    let providers: Vec<String> = directory
        .logged_in(harness)
        .into_iter()
        .map(|a| {
            if harness == OMP {
                a.provider.clone()
            } else {
                a.label.clone()
            }
        })
        .collect();
    if providers.is_empty() {
        "not logged in".to_owned()
    } else {
        format!("logged in: {}", providers.join(", "))
    }
}

impl Workbench {
    /// What the daemon says about the harnesses (tests, inspection).
    pub fn harness_directory(&self) -> &Entity<HarnessDirectory> {
        &self.env.harnesses
    }

    /// Whether the panel `id` is placed in a dock or the center.
    pub fn panel_open(&self, id: &str) -> bool {
        self.state
            .find_panel(&super::model::PanelId::new(id))
            .is_some()
    }

    /// ⌘K "Accounts…": the settings sheet at its Accounts page.
    pub fn open_accounts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(Some("accounts"), window, cx);
    }

    /// The Accounts page asks for the accounts when it is shown.
    pub(crate) fn refresh_accounts(&mut self, cx: &mut Context<Self>) {
        self.env
            .harnesses
            .update(cx, |directory, cx| directory.refresh(cx));
    }

    /// "Log in" / "Log out" of the Accounts page: the harness's own flow runs in a terminal
    /// session of the daemon, which this window shows and focuses so the user can finish it.
    pub fn account_flow(
        &mut self,
        harness: &str,
        provider: Option<String>,
        login: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.env
            .harnesses
            .update(cx, |directory, cx| directory.set_problem(None, cx));
        let task = self.sessions.update(cx, |store, cx| {
            store.account_flow(harness, provider, login, cx)
        });
        let directory = self.env.harnesses.clone();
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(answer) => {
                this.update_in(cx, |this, window, cx| {
                    // The sheet covers the window: close it so the terminal is seen.
                    this.close_settings(window, cx);
                    // The first-run setup steps aside too, until it is opened again.
                    this.suspend_setup(cx);
                    this.open_session(answer.session.id, window, cx);
                    if let Some(instructions) = answer.instructions {
                        this.set_notice(instructions, cx);
                    }
                })
                .ok();
            }
            Err(error) => {
                directory.update(cx, |directory, cx| directory.set_problem(Some(error), cx));
            }
        })
        .detach();
    }

    /// The body of the settings sheet's Accounts page.
    pub(crate) fn render_accounts(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let look = Look::of(cx);
        let (theme, tokens) = (look.theme, look.tokens);
        let directory = self.env.harnesses.read(cx);
        let listing = directory.listing().clone();
        let problem = directory.problem().map(str::to_owned);
        let detections: Vec<(Harness, Detection)> = HARNESSES
            .iter()
            .map(|h| (*h, directory.detection(h.id)))
            .collect();
        let none_installed = listing == Listing::Ready
            && detections
                .iter()
                .all(|(_, d)| matches!(d, Detection::NotFound(_)));
        let intro = div()
            .prose(&tokens)
            .text_color(theme.muted)
            .child("Hutan uses the logins you made in each harness. It never sees, stores or forwards a token: Log in and Log out open the harness's own flow in a terminal.");
        let refresh = button(
            "accounts-refresh",
            if listing == Listing::Loading {
                "Checking…"
            } else {
                "Refresh"
            },
            ButtonKind::Secondary,
            listing != Listing::Loading,
            &look,
        )
        .on_click(cx.listener(|this, _, _, cx| this.refresh_accounts(cx)))
        .child(probe(&self.regions, Region::Account("*", "refresh")));
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing * 3.0))
            .child(intro)
            .child(div().flex().child(refresh));
        if let Listing::Failed(error) = &listing {
            body = body.child(
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(error.clone())),
            );
        }
        if let Some(problem) = problem {
            body = body.child(
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(format!("Could not start: {problem}"))),
            );
        }
        if none_installed && !self.harness_install_available() {
            return body
                .child(ui::empty_state(
                    Mascot::Empty,
                    "No harness is installed. Install OMP, Claude Code or Codex, then check again.",
                    None,
                    &theme,
                    &tokens,
                ))
                .into_any_element();
        }
        if none_installed {
            // The cards carry the Install… buttons.
            body = body.child(
                div()
                    .prose(&tokens)
                    .text_color(theme.muted)
                    .child("No harness is installed yet. Install one below, then check again."),
            );
        }
        for (harness, detection) in detections {
            body = body.child(self.render_account_card(harness, &detection, &look, cx));
        }
        body.into_any_element()
    }

    fn render_account_card(
        &self,
        harness: Harness,
        detection: &Detection,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let directory = self.env.harnesses.read(cx);
        let accounts: Vec<AccountInfo> = directory.accounts_of(harness.id).cloned().collect();
        let found = !matches!(detection, Detection::NotFound(_));
        let (state_text, state_color) = match detection {
            Detection::Installed => ("installed", theme.success),
            Detection::NotFound(_) => ("not found", theme.warning),
            Detection::Checking => ("checking…", theme.muted),
            Detection::Unknown => ("unknown", theme.muted),
        };
        let id = harness.id;
        let login = button(
            format!("account-login-{id}"),
            "Log in",
            ButtonKind::Primary,
            found,
            look,
        )
        .when(found, |b| {
            b.on_click(cx.listener(move |this, _, window, cx| {
                this.account_flow(id, None, true, window, cx);
            }))
        })
        .child(probe(&self.regions, Region::Account(id, "login")));
        let logout = button(
            format!("account-logout-{id}"),
            "Log out",
            ButtonKind::Secondary,
            found,
            look,
        )
        .when(found, |b| {
            b.on_click(cx.listener(move |this, _, window, cx| {
                this.account_flow(id, None, false, window, cx);
            }))
        })
        .child(probe(&self.regions, Region::Account(id, "logout")));
        let rows: Vec<AnyElement> = accounts
            .iter()
            .enumerate()
            .map(|(index, account)| self.render_account_row(id, index, account, look, cx))
            .collect();
        let missing = match detection {
            Detection::NotFound(how) => Some(how.clone()),
            _ => None,
        };
        div()
            .id(SharedString::from(format!("account-card-{id}")))
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .card(&theme, &tokens)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .flex_1()
                            .medium()
                            .child(SharedString::from(harness.name)),
                    )
                    .child(ui::state_label(
                        ui::state_glyph(state_color, &theme),
                        state_text,
                        state_color,
                        false,
                        &theme,
                        &tokens,
                    ))
                    .when(!found && self.harness_install_available(), |s| {
                        s.child(
                            button(
                                format!("account-install-{id}"),
                                "Install…",
                                ButtonKind::Primary,
                                true,
                                look,
                            )
                            .on_click(move |_, window, cx| {
                                window.dispatch_action(
                                    Box::new(super::actions::InstallHarness(id.to_owned())),
                                    cx,
                                );
                            })
                            .child(probe(&self.regions, Region::Account(id, "install"))),
                        )
                    })
                    .child(login)
                    .child(logout),
            )
            .children(missing.map(|how| {
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(how))
            }))
            .children(rows)
            .child(probe(&self.regions, Region::Account(id, "card")))
            .into_any_element()
    }

    fn render_account_row(
        &self,
        harness: &'static str,
        index: usize,
        account: &AccountInfo,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let (status, color) = match account.status {
            AccountStatus::LoggedIn => ("logged in", theme.success),
            AccountStatus::LoggedOut => ("logged out", theme.warning),
            AccountStatus::Unknown => ("unknown", theme.muted),
        };
        // Only OMP has several logins; its logout is per provider (the harness's own flow takes
        // the provider). The others have one login and log out from the card.
        let per_provider = harness == OMP && account.status == AccountStatus::LoggedIn;
        let provider = account.provider.clone();
        let mut line = div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(account.label.clone())),
            );
        if account.label != account.provider {
            line = line.child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(account.provider.clone())),
            );
        }
        if let Some(plan) = &account.plan {
            line = line.child(ui::pill(plan.clone(), &theme, &tokens));
        }
        line = line.child(ui::state_label(
            ui::state_glyph(color, &theme),
            status,
            color,
            false,
            &theme,
            &tokens,
        ));
        if per_provider {
            line = line.child(
                button(
                    format!("account-logout-{harness}-{index}"),
                    "Log out",
                    ButtonKind::Ghost,
                    true,
                    look,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.account_flow(harness, Some(provider.clone()), false, window, cx);
                })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(line)
            .children(account.detail.as_ref().map(|detail| {
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(detail.clone()))
            }))
            .into_any_element()
    }
}
