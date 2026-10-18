# actor-agent

Assistant conversationnel en Rust qui s'appuie sur un modèle Ollama (via [rig](https://github.com/0xPlaygrounds/rig)) et sur le modèle d'acteurs [Actix]. On l'utilise depuis un terminal (TUI ratatui), depuis une API Web (actix-web), ou les deux à la fois.

## Prérequis

- Rust (édition 2024).
- Un serveur [Ollama](https://ollama.com) démarré, avec le modèle téléchargé :

```sh
ollama pull llama3.2
```

## Lancement

```sh
cargo run --release              # interface terminal (par défaut)
cargo run --release -- --web     # API Web seule
cargo run --release -- --cli --web   # les deux : TUI + API en fond
```

Les options `--cli` et `--web` l'emportent sur le réglage `ui` de la configuration. En mode mixte, quitter le TUI arrête aussi le serveur Web.

## Configuration

Chaque source remplace la précédente, dans cet ordre :

1. les valeurs par défaut ;
2. le fichier `config.toml` du répertoire courant. S'il n'existe pas, il est créé au premier lancement avec les valeurs par défaut ;
3. les variables d'environnement.

| Clé `config.toml`     | Variable d'environnement | Défaut                                            |
|-----------------------|--------------------------|---------------------------------------------------|
| `ollama_url`          | `OLLAMA_API_BASE_URL`    | `http://localhost:11434`                          |
| `model`               | `OLLAMA_MODEL`           | `llama3.2`                                        |
| `preamble`            | `APP_PREAMBLE`           | `Tu es un assistant concis. Réponds en français.` |
| `ui` (`cli`, `web`, `both`) | `APP_UI`           | `cli`                                             |
| `web_addr`            | `WEB_ADDR`               | `127.0.0.1:8080`                                  |
| `answer_timeout_secs` | —                        | `120`                                             |

Exemple :

```toml
model = "qwen3"
ui = "both"
web_addr = "127.0.0.1:3000"
```

Le modèle et le préambule se changent pendant l'exécution, avec `/model` et `/preamble` dans le TUI ou `PATCH /config` dans l'API. Le changement vaut pour les sessions créées ensuite. Une session déjà ouverte garde son modèle jusqu'à ce qu'on l'efface. Ces changements ne sont pas enregistrés dans `config.toml`.

## Interface terminal

| Touche / commande     | Action                                              |
|-----------------------|-----------------------------------------------------|
| `Entrée`              | envoyer le message                                  |
| `↑` `↓` / `PgUp` `PgDn` | faire défiler l'historique                        |
| `Échap` / `Ctrl+C`    | quitter                                             |
| `/clear`              | effacer la conversation (et appliquer la config à jour) |
| `/model <nom>`        | changer le modèle des nouvelles sessions            |
| `/preamble <texte>`   | changer le préambule des nouvelles sessions         |

La barre du bas indique l'état de l'agent : « En attente... » ou « Génération en cours... ».

## API Web

Chaque `session_id` a son propre historique de conversation.

```sh
# Envoyer un message
curl -X POST localhost:8080/chat -H 'content-type: application/json' \
     -d '{"session_id": "alice", "content": "Bonjour !"}'
# → {"session_id":"alice","response":"Bonjour ! Comment puis-je vous aider ?"}

# Effacer une session (204)
curl -X DELETE localhost:8080/sessions/alice

# Lire la configuration
curl localhost:8080/config

# Changer le modèle ou le préambule à chaud
curl -X PATCH localhost:8080/config -H 'content-type: application/json' -d '{"model": "qwen3"}'
curl -X PATCH localhost:8080/config -H 'content-type: application/json' -d '{"preamble": "Sois bref."}'
```

| Code | Signification                                            |
|------|----------------------------------------------------------|
| 200  | réponse de l'agent                                       |
| 204  | session effacée                                          |
| 502  | échec de l'agent (Ollama arrêté, modèle introuvable...)  |
| 504  | l'agent n'a pas répondu dans le délai `answer_timeout_secs` |

## Architecture

```
 TUI (cli_ui) ─┐                      ┌─► AgentActor (session « cli »)
               ├─► ProxyAgent ────────┼─► AgentActor (session « alice »)
 API (web_ui) ─┘        │             └─► ...
                        ▼
                   ConfigActor
```

| Module           | Rôle                                                                 |
|------------------|----------------------------------------------------------------------|
| `config.rs`      | `AppConfig` (chargement) et `ConfigActor` (`GetConfig`, `UpdateConfig`) |
| `proxy.rs`       | `ProxyAgent` : associe chaque session à son agent, créé avec la config à jour |
| `agent_actor.rs` | `AgentActor` : agent rig + historique d'une session                  |
| `web_ui.rs`      | routes HTTP                                                          |
| `cli_ui.rs`      | TUI ratatui, qui n'est jamais bloqué pendant la génération           |
| `main.rs`        | chargement de la config, démarrage des acteurs, choix de l'interface |

## Tests

```sh
cargo test
```

Les tests n'ont pas besoin d'Ollama : un faux serveur Ollama (`src/test_support.rs`) le remplace. Les tests d'intégration (`tests/binary.rs`) lancent le vrai binaire, y compris le TUI dans un pseudo-terminal créé avec `script`. Ils ne tournent donc que sous macOS ou Linux.

Pour mesurer la couverture :

```sh
cargo install cargo-llvm-cov
cargo llvm-cov --summary-only
```

[Actix]: https://actix.rs
