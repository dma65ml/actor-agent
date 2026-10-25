# actor-agent

Assistant conversationnel en Rust qui s'appuie sur un modèle Ollama (via [rig](https://github.com/0xPlaygrounds/rig)) et sur le modèle d'acteurs [Actix]. C'est un harness d'agent minimal, construit selon les principes de [Build a minimal agent harness](https://www.mindstudio.ai/blog/build-minimal-agent-harness-python-step-by-step) : l'agent lit et écrit des fichiers et lance des commandes shell, sous contrôle de l'utilisateur. On l'utilise depuis un terminal (TUI ratatui).

Le modèle doit savoir appeler des outils (`llama3.2`, `qwen3`...).

## Prérequis

- Rust (édition 2024).
- Un serveur [Ollama](https://ollama.com) démarré, avec le modèle téléchargé :

```sh
ollama pull llama3.2
```

## Lancement

```sh
cargo run --release
```

## Configuration

Chaque source remplace la précédente, dans cet ordre :

1. les valeurs par défaut ;
2. le fichier `config.toml` du répertoire courant, s'il existe ;
3. les variables d'environnement.

| Clé `config.toml`     | Variable d'environnement | Défaut                                            |
|-----------------------|--------------------------|---------------------------------------------------|
| `ollama_url`          | `OLLAMA_API_BASE_URL`    | `http://localhost:11434`                          |
| `model`               | `OLLAMA_MODEL`           | `llama3.2`                                        |
| `preamble`            | `APP_PREAMBLE`           | consigne d'agent de développement (base du prompt système) |
| `answer_timeout_secs` | —                        | `600` (attente d'accord comprise)                 |
| `max_turns`           | —                        | `50` appels au modèle par message                 |
| `context_tokens`      | —                        | `8192` (compaction à 80 %)                        |
| `session_file`        | —                        | `.actor-agent-session.jsonl`                      |
| `auto_approve` (`read`, `workspace`, `full`) | — | `read`                                          |

Exemple :

```toml
model = "qwen3"
context_tokens = 32768
auto_approve = "workspace"
```

Une configuration invalide affiche `Configuration : <erreur>` et termine avec le code 1 ; un journal de session illisible affiche `Session (<fichier>) : <erreur>`, code 1 également. `config.toml` n'est jamais créé ni modifié par le programme.

Le modèle et le préambule se changent pendant l'exécution, avec `/model` et `/preamble`. Le changement s'applique après `/clear`. Ces changements ne sont pas enregistrés dans `config.toml`.

## Interface terminal

| Touche / commande     | Action                                              |
|-----------------------|-----------------------------------------------------|
| `Entrée`              | envoyer le message                                  |
| molette / `PgUp` `PgDn` | faire défiler l'historique                        |
| `↑` `↓`               | rappeler les messages envoyés                       |
| `Échap` / `Ctrl+C`    | quitter                                             |
| `/clear`              | effacer la conversation (et appliquer la config à jour) |
| `/model <nom>`        | changer le modèle (actif après `/clear`)            |
| `/preamble <texte>`   | changer le préambule (actif après `/clear`)         |

La barre du bas indique l'état de l'agent : « En attente... », « Génération en cours... » ou une demande d'autorisation (`o` pour autoriser, toute autre touche pour refuser, quitter refuse). Demandes et réponses sont notées dans l'historique. Pendant une génération, l'interface reste réactive mais un nouvel envoi est ignoré ; une saisie vide aussi.

## Harness

| Principe | Mise en œuvre |
|----------|---------------|
| Boucle bornée | boucle d'outils de rig, plafonnée à `max_turns` ; le même appel répété 3 fois d'affilée arrête l'exécution |
| Outils | `read_file`, `write_file`, `run_bash` |
| Permissions | chaque appel a un niveau (`read` < `workspace` < `full`) ; au-delà de `auto_approve`, il attend votre accord. Écrire hors du répertoire courant, `rm`, `sudo`... ou toute commande composée (`;`, `\|`, `$`, redirections) relève de `full` |
| Prompt système | `preamble`, puis les fichiers `AGENTS.md` / `CLAUDE.md` trouvés de la racine jusqu'au répertoire courant (le plus proche l'emporte) |
| Session | journal JSON Lines en ajout seul (`session_file`), rejoué au démarrage : la conversation reprend où elle s'était arrêtée. `/clear` y inscrit une remise à zéro |
| Compaction | au-delà de 80 % de `context_tokens` (estimation : 4 caractères par jeton), les anciens messages sont résumés et les 6 derniers gardés tels quels |

La boucle d'agent est celle de rig (`agent.chat` avec `default_max_turns`) : le harness l'encadre au lieu de la réécrire. Les tours sont traités l'un après l'autre, et un tour en échec ne modifie pas l'historique.

### Outils et permissions

| Outil | Effet | Niveau requis |
|-------|-------|---------------|
| `read_file(path)` | lit un fichier texte | `read` |
| `write_file(path, content)` | crée ou remplace un fichier (répertoires parents créés) | `workspace` dans le répertoire courant, `full` ailleurs (chemin absolu ou `..`) |
| `run_bash(command)` | `sh -c` dans le répertoire courant, renvoie le code de sortie et les sorties | `read` pour `ls cat grep echo pwd head tail` ; `full` pour `rm sudo shutdown reboot mkfs dd` ou toute commande contenant `; \| & $ \` < > ( ) { }` ou un saut de ligne ; `workspace` sinon |

Des arguments illisibles ou un outil inconnu demandent le niveau `full`. Une sortie de plus de 20 000 caractères est tronquée. Un appel refusé n'est pas exécuté : le modèle reçoit « refusé par l'utilisateur » et peut continuer.

Par défaut (`auto_approve = "read"`), seules les lectures s'exécutent sans demander. `auto_approve = "workspace"` reproduit le choix de l'article : toute commande non destructive s'exécute alors directement. Il n'y a pas de bac à sable : une commande autorisée tourne avec vos droits.

### Session et compaction

- Chaque événement (message, compaction, remise à zéro) est une ligne JSON horodatée (`ts`) ajoutée au journal ; le fichier n'est jamais réécrit. Une ligne illisible (écriture interrompue) est ignorée au rejeu.
- La compaction a lieu avant d'envoyer un message. La coupure tombe toujours sur un message utilisateur textuel, jamais entre un appel d'outil et son résultat. Si le résumé échoue, l'historique reste intact et la compaction sera retentée au tour suivant.

### Erreurs affichées

| Erreur | Cause |
|--------|-------|
| `échec de l'agent (Ollama est-il démarré ?) : …` | appel au modèle en échec (serveur arrêté, modèle introuvable…) |
| `limite de N appels au modèle atteinte ; …` | plafond `max_turns` atteint |
| `exécution arrêtée : boucle détectée …` | même appel d'outil répété 3 fois |
| `délai de réponse dépassé` | pas de réponse dans `answer_timeout_secs` ; la génération et la commande en cours sont abandonnées |
| `journal de session : …` | écriture du journal impossible |
| `acteur agent indisponible` | acteur arrêté avant de répondre |

## Architecture

```
 main.rs ──► cli_ui::run ──Ask / Reset──► AgentActor ──► boucle rig ──► Ollama
                 ▲                            │              │
                 └──── demandes d'accord ─────┼── Guard ◄────┘ (avant chaque outil)
                                              └─► SessionStore (journal JSONL)
```

Le cœur est une bibliothèque (`src/lib.rs`) ; le binaire ne fait que l'assembler.

| Module           | Rôle                                                                 |
|------------------|----------------------------------------------------------------------|
| `config.rs`      | `AppConfig` : chargement depuis les défauts, `config.toml` et l'environnement |
| `agent_actor.rs` | `AgentActor` : assemble le harness, historique, délai ; messages `Ask` et `Reset` |
| `harness/`       | `prompt.rs` (prompt système), `session.rs` (journal), `tools.rs` (outils, permissions), `guard.rs` (garde-fous), `compact.rs` (compaction) |
| `cli_ui/`        | TUI ratatui, jamais bloqué pendant la génération : `mod.rs` (boucle d'événements), `app.rs` (état, saisie), `draw.rs` (rendu), `tests.rs` |
| `lib.rs`         | expose les modules ci-dessus                                         |
| `main.rs`        | chargement de la config, démarrage de l'agent et du TUI              |

## Limites connues

- Hors périmètre : sessions multiples, API réseau, sous-agents, hooks configurables, streaming, fournisseurs autres qu'Ollama, persistance des changements faits avec `/model` et `/preamble`.
- Le journal de session grossit sans limite ; son rejeu au démarrage est linéaire.
- Le curseur est mal placé avec des caractères larges (CJK, emoji).
- La capture de la souris empêche la sélection directe de texte : maintenir `Shift` (ou `Option` dans iTerm2).

## Tests

```sh
cargo test
```

Les tests n'ont pas besoin d'Ollama : un faux serveur Ollama (`src/test_support.rs`) le remplace. Il réagit à des messages spéciaux (`ERREUR`, `LENT`, `MODELE`, `PREAMBULE`) et sait demander des appels d'outils (`OUTIL <commande>`, `BOUCLE`), ce qui permet de tester la boucle, les permissions et la détection de boucle. Le test d'intégration (`tests/binary.rs`) lance le vrai binaire avec une configuration invalide.

Pour mesurer la couverture :

```sh
cargo install cargo-llvm-cov
cargo llvm-cov --summary-only
```

[Actix]: https://actix.rs
