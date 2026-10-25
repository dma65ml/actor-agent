//! Garde-fous vérifiés juste avant chaque appel d'outil (hook rig en mode natif, boucle texte sinon) :
//! - permissions : au-delà du niveau `auto_approve`, l'appel attend l'accord de l'utilisateur ;
//! - boucle bloquée : le même appel répété `STUCK_REPEATS` fois d'affilée arrête l'exécution.

use std::future::Future;

use rig_agent::agent::{DispatchAction, DispatchEvent};
use rig_agent::{AgentHook, HookContext};
use rig_core::effect::EffectKind;
use rig_core::error::{ErrorKind, ErrorReport};
use rig_core::tool::ToolErrorKind;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use crate::tools::{self, Permission};

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

/// Refus d'un appel d'outil.
pub enum Denial {
    /// L'exécution entière s'arrête (boucle détectée).
    Stop(String),
    /// L'appel n'est pas exécuté ; le modèle reçoit la raison et peut continuer.
    Refused(String),
}

/// Mémorise l'appel dans `recent` ; vrai s'il est répété `STUCK_REPEATS` fois d'affilée.
pub fn record(recent: &mut Vec<String>, name: &str, args: &str) -> bool {
    let call = format!("{name} {args}");
    recent.push(call.clone());
    recent.len() >= STUCK_REPEATS && recent.iter().rev().take(STUCK_REPEATS).all(|c| *c == call)
}

/// Garde-fous partagés par les deux modes d'appel d'outils (hook rig ou boucle texte).
#[derive(Clone)]
pub struct Guard {
    approvals: UnboundedSender<Approval>,
    auto_approve: Permission,
}

impl Guard {
    /// Garde-fou qui accepte sans confirmation les appels jusqu'au niveau `auto_approve`.
    pub fn new(approvals: UnboundedSender<Approval>, auto_approve: Permission) -> Self {
        Self { approvals, auto_approve }
    }

    /// Décide d'un appel déjà mémorisé (`stuck` vient de `record`) : arrêt si boucle,
    /// accord de l'utilisateur si la permission dépasse `auto_approve`, exécution sinon.
    pub fn authorize(&self, stuck: bool, name: &str, args: &str) -> impl Future<Output = Result<(), Denial>> + Send + 'static {
        let call = format!("{name} {args}");
        let ask = tools::permission(name, args) > self.auto_approve;
        let approvals = self.approvals.clone();
        async move {
            if stuck {
                return Err(Denial::Stop(format!("boucle détectée : « {call} » appelé {STUCK_REPEATS} fois d'affilée")));
            }
            if !ask {
                return Ok(());
            }
            let (reply, answer) = oneshot::channel();
            let allowed = approvals.send(Approval { request: call, reply }).is_ok() && answer.await.unwrap_or(false);
            if allowed { Ok(()) } else { Err(Denial::Refused("refusé par l'utilisateur".into())) }
        }
    }
}

/// Refus d'un appel d'outil avec le type d'erreur et le message donnés.
fn deny(kind: ErrorKind, message: String) -> DispatchAction {
    DispatchAction::Deny(ErrorReport::new(kind, message))
}

impl AgentHook for Guard {
    /// Applique les garde-fous aux appels d'outil natifs ; les autres effets passent.
    fn on_dispatch(&self, ctx: &HookContext, event: DispatchEvent<'_>) -> impl Future<Output = DispatchAction> + Send {
        let check = match event.kind {
            EffectKind::ToolCall { name, args } => {
                let stuck = ctx.scratchpad().update(|recent: &mut RecentCalls| record(&mut recent.0, name, args));
                Some(self.authorize(stuck, name, args))
            }
            _ => None,
        };
        async move {
            let Some(check) = check else { return DispatchAction::Proceed };
            match check.await {
                Ok(()) => DispatchAction::Proceed,
                // `Cancelled` arrête toute l'exécution et remonte la raison à l'utilisateur.
                Err(Denial::Stop(reason)) => deny(ErrorKind::Cancelled, reason),
                // Refus non fatal : le modèle reçoit ce message comme résultat de l'outil.
                Err(Denial::Refused(reason)) => deny(ErrorKind::Tool(ToolErrorKind::PermissionDenied), reason),
            }
        }
    }
}
