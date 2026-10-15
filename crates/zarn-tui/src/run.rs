use crate::render;
use crate::state::{Action, App, PermissionRequest};
use anyhow::Result;
use async_trait::async_trait;
use crossterm::event::{
    Event as TermEvent, EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use futures::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use zarn_core::agent::{Agent, AgentEvent};
use zarn_core::tools::Risk;
use zarn_core::types::{Message, StopReason};
use zarn_tools::permissions::{Answer, PermissionPrompt};

pub struct TuiPrompt {
    tx: mpsc::Sender<PermissionRequest>,
}

#[async_trait]
impl PermissionPrompt for TuiPrompt {
    async fn ask(&self, tool: &str, risk: Risk, args: &Value) -> Answer {
        let (reply, answer) = oneshot::channel();
        let request = PermissionRequest {
            tool: tool.to_string(),
            risk,
            args: args.clone(),
            reply,
        };
        if self.tx.send(request).await.is_err() {
            return Answer::No;
        }
        answer.await.unwrap_or(Answer::No)
    }
}

pub fn permission_channel() -> (Arc<TuiPrompt>, mpsc::Receiver<PermissionRequest>) {
    let (tx, rx) = mpsc::channel(4);
    (Arc::new(TuiPrompt { tx }), rx)
}

enum UiEvent {
    Agent(AgentEvent),
    TurnDone {
        messages: Vec<Message>,
        result: Result<StopReason, String>,
    },
}

pub async fn run(
    agent: Arc<Agent>,
    mut messages: Vec<Message>,
    mut permissions: mpsc::Receiver<PermissionRequest>,
) -> Result<Vec<Message>> {
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );

    let outcome = event_loop(&mut terminal, agent, &mut messages, &mut permissions).await;

    let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    ratatui::restore();

    outcome.map(|_| messages)
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    agent: Arc<Agent>,
    messages: &mut Vec<Message>,
    permissions: &mut mpsc::Receiver<PermissionRequest>,
) -> Result<()> {
    let mut app = App::new(agent.config().model.clone());
    let (ui_tx, mut ui_rx) = mpsc::channel::<UiEvent>(256);
    let mut keys = EventStream::new();
    let mut cancel = CancellationToken::new();

    loop {
        terminal.draw(|frame| render::draw(frame, &app))?;

        tokio::select! {
            key = keys.next() => {
                let Some(Ok(event)) = key else { continue };
                let TermEvent::Key(key) = event else { continue };
                match app.on_key(key) {
                    Some(Action::Quit) => break,
                    Some(Action::Cancel) => cancel.cancel(),
                    Some(Action::Submit(text)) => {
                        if app.streaming {
                            app.notice("still streaming — press esc to cancel first");
                            continue;
                        }
                        messages.push(Message::user(text));
                        cancel = CancellationToken::new();
                        app.streaming = true;
                        spawn_turn(
                            agent.clone(),
                            messages.clone(),
                            ui_tx.clone(),
                            cancel.clone(),
                        );
                    }
                    None => {}
                }
            }
            event = ui_rx.recv() => {
                match event {
                    None => break,
                    Some(UiEvent::Agent(event)) => app.apply(event),
                    Some(UiEvent::TurnDone { messages: updated, result }) => {
                        app.streaming = false;
                        *messages = updated;
                        match result {
                            Ok(StopReason::Cancelled) => app.notice("cancelled"),
                            Ok(_) => {}
                            Err(err) => app.notice(format!("error: {err}")),
                        }
                    }
                }
            }
            request = permissions.recv() => {
                match request {
                    Some(request) => app.pending = Some(request),
                    None => continue,
                }
            }
        }
    }

    cancel.cancel();
    Ok(())
}

fn spawn_turn(
    agent: Arc<Agent>,
    mut messages: Vec<Message>,
    ui: mpsc::Sender<UiEvent>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
        let forward = {
            let ui = ui.clone();
            tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    if ui.send(UiEvent::Agent(event)).await.is_err() {
                        return;
                    }
                }
            })
        };

        let result = agent
            .run(&mut messages, &tx, &cancel)
            .await
            .map_err(|e| e.to_string());

        drop(tx);
        let _ = forward.await;
        let _ = ui.send(UiEvent::TurnDone { messages, result }).await;
    });
}
