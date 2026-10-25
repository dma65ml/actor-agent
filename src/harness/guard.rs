//! Garde-fous de la boucle d'agent, vérifiés juste avant chaque appel d'outil (hook rig) :
//! - permissions : au-delà du niveau `auto_approve`, l'appel attend l'accord de l'utilisateur ;
//! - boucle bloquée : le même appel répété `STUCK_REPEATS` fois d'affilée arrête l'exécution.

use std::future::Future;

use rig_agent::agent::{DispatchAction, DispatchEvent};
use rig_agent::{AgentHook, HookContext};
use rig_core::effect::EffectKind;
use rig_core::error::{ErrorKind, ErrorReport};
use rig_core::tool::ToolErrorKind;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use super::tools::{self, Permission};

/// Nombre d'appels identiques consécutifs considéré comme une boucle bloquée.
const STUCK_REPEATS: usize = 3;

/// Demande d'autorisation transmise à l'interface ; elle répond par `reply`.
/// Si l'interface abandonne la demande (fermeture), l'appel est refusé.
pub struct Approval {
    /// Appel d'outil proposé, lisible par l'utilisateur.
    pub request: String,
    /// Réponse : `true` pour autoriser.
    pub reply: oneshot::Sender<bool>,
}

/// Appels d'outil de l'exécution en cours, gardés dans le scratchpad de rig (propre à chaque exécution).
#[derive(Clone, Default)]
struct RecentCalls(Vec<String>);

/// Décision prise avant d'exécuter un appel.
enum Check {
    Proceed,
    Stuck(String),
    AskUser(String),
}

/// Hook appliquant les garde-fous.
pub struct Guard {
    approvals: UnboundedSender<Approval>,
    auto_approve: Permission,
}

impl Guard {
    /// Garde-fou qui accepte sans confirmation les appels jusqu'au niveau `auto_approve`.
    pub fn new(approvals: UnboundedSender<Approval>, auto_approve: Permission) -> Self {
        Self { approvals, auto_approve }
    }

    /// Mémorise l'appel puis décide : boucle bloquée, accord nécessaire ou exécution directe.
    fn check(&self, ctx: &HookContext, name: &str, args: &str) -> Check {
        let call = format!("{name} {args}");
        let stuck = ctx.scratchpad().update(|recent: &mut RecentCalls| {
            recent.0.push(call.clone());
            recent.0.len() >= STUCK_REPEATS && recent.0.iter().rev().take(STUCK_REPEATS).all(|c| *c == call)
        });
        if stuck {
            Check::Stuck(call)
        } else if tools::permission(name, args) > self.auto_approve {
            Check::AskUser(call)
        } else {
            Check::Proceed
        }
    }
}

/// Refus d'un appel d'outil avec le type d'erreur et le message donnés.
fn deny(kind: ErrorKind, message: String) -> DispatchAction {
    DispatchAction::Deny(ErrorReport::new(kind, message))
}

impl AgentHook for Guard {
    /// Applique les garde-fous aux appels d'outil ; les autres effets passent.
    fn on_dispatch(&self, ctx: &HookContext, event: DispatchEvent<'_>) -> impl Future<Output = DispatchAction> + Send {
        let check = match event.kind {
            EffectKind::ToolCall { name, args } => self.check(ctx, name, args),
            _ => Check::Proceed,
        };
        let approvals = self.approvals.clone();
        async move {
            match check {
                Check::Proceed => DispatchAction::Proceed,
                // `Cancelled` arrête toute l'exécution et remonte la raison à l'utilisateur.
                Check::Stuck(call) => deny(
                    ErrorKind::Cancelled,
                    format!("boucle détectée : « {call} » appelé {STUCK_REPEATS} fois d'affilée"),
                ),
                Check::AskUser(request) => {
                    let (reply, answer) = oneshot::channel();
                    let allowed = approvals.send(Approval { request, reply }).is_ok() && answer.await.unwrap_or(false);
                    if allowed {
                        DispatchAction::Proceed
                    } else {
                        // Refus non fatal : le modèle reçoit ce message comme résultat de l'outil.
                        deny(ErrorKind::Tool(ToolErrorKind::PermissionDenied), "refusé par l'utilisateur".into())
                    }
                }
            }
        }
    }
}
