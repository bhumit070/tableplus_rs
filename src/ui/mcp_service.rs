//! Runs the MCP server (`mcp.rs`) while Settings › MCP has it on, and shows
//! its safe-mode confirmations in the window, as the UI's own writes do.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Global, Window};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;
use tokio::sync::mpsc;

use super::confirm::ConfirmWrite;
use crate::mcp::{self, ConfirmRequest, Running};
use crate::runtime;
use crate::settings;
use crate::store;

/// The server while it runs, and what Settings shows about it.
#[derive(Default)]
pub struct McpState {
    running: Option<Running>,
    pub port: Option<u16>,
    pub token: Option<String>,
    pub error: Option<String>,
    pub starting: bool,
    /// The token is only in memory (Save passwords is off, or the keychain
    /// refused it): tools need the new one after a restart.
    pub token_unsaved: bool,
    /// Bumped on every start / stop, so a late start result is dropped.
    generation: u64,
}

impl Global for McpState {}

pub fn state(cx: &App) -> Option<&McpState> {
    cx.try_global::<McpState>()
}

/// Starts or stops the server to match the setting.
pub fn apply(window: &mut Window, cx: &mut App) {
    let enabled = settings::get(cx).mcp_enabled;
    let state = cx.default_global::<McpState>();
    if !enabled {
        state.generation += 1;
        state.running = None;
        state.port = None;
        state.error = None;
        state.starting = false;
        cx.refresh_windows();
        return;
    }
    if state.running.is_some() || state.starting {
        return;
    }
    start(window, cx);
}

/// The saved token, or a new one (saved to the keychain when allowed).
fn token(cx: &mut App) -> (String, bool) {
    if let Some(token) = state(cx).and_then(|state| state.token.clone()) {
        let unsaved = state(cx).is_some_and(|state| state.token_unsaved);
        return (token, unsaved);
    }
    let save = settings::get(cx).save_passwords;
    if save && let Ok(Some(token)) = store::mcp_token() {
        return (token, false);
    }
    let token = mcp::new_token();
    let saved = save && store::set_mcp_token(&token).is_ok();
    (token, !saved)
}

fn start(window: &mut Window, cx: &mut App) {
    let (token, unsaved) = token(cx);
    let state = cx.default_global::<McpState>();
    state.generation += 1;
    let generation = state.generation;
    state.starting = true;
    state.error = None;
    state.token = Some(token.clone());
    state.token_unsaved = unsaved;

    // Confirmations asked for by the server, shown in this window.
    let (confirm, mut asks) = mpsc::unbounded_channel::<ConfirmRequest>();
    let window_handle = window.window_handle();
    cx.spawn(async move |cx| {
        while let Some(ask) = asks.recv().await {
            let shown =
                window_handle.update(cx, |_, window, cx| show_confirmation(ask, window, cx));
            if shown.is_err() {
                break;
            }
        }
    })
    .detach();

    let preferred = settings::get(cx).mcp_port;
    let task = runtime::spawn(mcp::start(
        preferred,
        token,
        confirm,
        Arc::new(|| store::load().0),
    ));
    cx.spawn(async move |cx| {
        let started = task.await;
        cx.update(|cx| {
            let state = cx.default_global::<McpState>();
            if state.generation != generation {
                return;
            }
            state.starting = false;
            match started {
                Ok(running) => {
                    let port = running.port;
                    state.port = Some(port);
                    state.running = Some(running);
                    if port != preferred {
                        settings::update(cx, |settings| settings.mcp_port = port);
                    }
                }
                Err(error) => state.error = Some(format!("{error:#}")),
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// A new token: tools using the old one stop working at once.
pub fn regenerate_token(window: &mut Window, cx: &mut App) {
    let token = mcp::new_token();
    let saved = settings::get(cx).save_passwords && store::set_mcp_token(&token).is_ok();
    let state = cx.default_global::<McpState>();
    state.token = Some(token.clone());
    state.token_unsaved = !saved;
    if let Some(running) = &state.running {
        running.set_token(token);
    }
    window.push_notification("New MCP token: update it in your AI tools", cx);
    cx.refresh_windows();
}

/// The safe-mode confirmation, saying it came from an AI tool. Closing it
/// without confirming drops the reply, which the server reads as "no".
pub(super) fn show_confirmation(ask: ConfirmRequest, window: &mut Window, cx: &mut App) {
    window.push_notification(
        Notification::warning(format!(
            "An AI tool wants to {} on {} (via MCP)",
            ask.what.to_lowercase(),
            ask.cfg.display_name()
        )),
        cx,
    );
    let reply = Rc::new(RefCell::new(Some(ask.reply)));
    ConfirmWrite::open(
        &ask.cfg,
        &ask.what,
        ask.statements,
        window,
        cx,
        Rc::new(move |_, _| {
            if let Some(reply) = reply.borrow_mut().take() {
                let _ = reply.send(true);
            }
        }),
    );
}
