//! Interface terminal (ratatui) : historique défilant, zone de saisie et barre de statut.
//! Les touches arrivent d'un thread dédié et les réponses de tâches actix : la boucle de
//! rendu ne bloque jamais pendant la génération.

mod app;
mod draw;
mod history;
mod input;
#[cfg(test)]
mod tests;

use actix::Addr;
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::agent_actor::AgentActor;
use crate::config::AppConfig;
use crate::harness::guard::Approval;
use app::App;

/// Lance le TUI jusqu'à Échap / Ctrl+C ; le terminal est restauré même en cas d'erreur.
/// La souris est capturée pour faire défiler l'historique à la molette ; le collage
/// (bracketed paste) arrive d'un bloc, sans que ses sauts de ligne n'envoient le message.
pub async fn run(
    agent: Addr<AgentActor>,
    config: AppConfig,
    approvals: UnboundedReceiver<Approval>,
) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let res = event_loop(&mut terminal, spawn_input_thread(), approvals, agent, config).await;
    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    res
}

/// Boucle principale : dessine, puis attend une touche, la souris, un collage, une demande d'autorisation
/// ou une réponse de l'agent. Le terminal et les sources d'événements sont injectés pour les tests.
async fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    mut keys: UnboundedReceiver<Event>,
    mut approvals: UnboundedReceiver<Approval>,
    agent: Addr<AgentActor>,
    config: AppConfig,
) -> Result<(), B::Error> {
    let (replies, mut reply_rx) = unbounded_channel();
    let mut app = App::new(agent, config, replies);
    app.show_tool_mode();
    loop {
        terminal.draw(|f| app.draw(f))?;
        // Ordre fixe (`biased`) : une demande d'autorisation arrivée est affichée avant de
        // traiter la touche suivante, qui sert alors de réponse.
        tokio::select! {
            biased;
            Some(reply) = reply_rx.recv() => app.on_reply(reply),
            Some(approval) = approvals.recv() => app.on_approval(approval),
            ev = keys.recv() => match ev {
                Some(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if !app.on_key(key) { return Ok(()) }
                }
                Some(Event::Mouse(mouse)) => app.on_mouse(mouse.kind),
                Some(Event::Paste(text)) => app.on_paste(&text),
                Some(_) => {}
                None => return Ok(()),
            },
        }
    }
}

/// Lit les événements crossterm (bloquants) dans un thread et les transmet à la boucle async.
fn spawn_input_thread() -> UnboundedReceiver<Event> {
    let (tx, rx) = unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
    rx
}
