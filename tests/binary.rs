//! Tests d'intégration : lancent le vrai binaire pour couvrir `main`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Commande du binaire exécutée dans un répertoire temporaire vierge (pour `config.toml`).
fn binary(name: &str) -> (Command, PathBuf) {
    let dir = std::env::temp_dir().join(format!("actor-agent-bin-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_actor-agent"));
    cmd.current_dir(&dir).env_remove("OLLAMA_MODEL").env_remove("WEB_ADDR");
    (cmd, dir)
}

/// Une configuration invalide arrête le programme avec un message lisible.
#[test]
fn invalid_config_exits_with_message() {
    let (mut cmd, _) = binary("badui");
    let out = cmd.env("APP_UI", "tui").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("interface inconnue « tui »"));
}

/// Mode Web : crée `config.toml`, sert `GET /config`, puis s'arrête proprement sur SIGTERM.
#[test]
fn web_mode_serves_config_and_stops_on_sigterm() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let (mut cmd, dir) = binary("web");
    let mut child = cmd.args(["--web"]).env("WEB_ADDR", &addr).stdout(Stdio::null()).spawn().unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match TcpStream::connect(&addr) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("serveur injoignable : {e}"),
        }
    };
    write!(stream, "GET /config HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"model\":\"llama3.2\""));
    assert!(dir.join("config.toml").exists());

    // SIGTERM (et non SIGKILL) : actix s'arrête proprement et `main` rend la main.
    Command::new("kill").arg(child.id().to_string()).status().unwrap();
    assert!(child.wait().unwrap().success());
}

/// Lance le binaire dans un pseudo-terminal (`script`), attend l'affichage du TUI puis envoie Échap.
/// Renvoie le code de sortie et ce que le TUI a écrit à l'écran.
fn run_tui(name: &str, args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
    let (inner, dir) = binary(name);
    let exe = inner.get_program().to_owned();
    // Le pseudo-terminal naît en 0×0 : on lui donne une taille avant de lancer le TUI.
    let line = format!("stty rows 24 cols 80; '{}' {}", exe.to_string_lossy(), args.join(" "));
    let mut cmd = Command::new("script");
    if cfg!(target_os = "macos") {
        cmd.args(["-q", "/dev/null", "sh", "-c", &line]);
    } else {
        cmd.args(["-qec", &line, "/dev/null"]);
    }
    let mut child = cmd
        .current_dir(dir)
        .envs(envs.iter().copied())
        .env_remove("OLLAMA_MODEL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    });
    std::thread::sleep(Duration::from_secs(2));
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"\x1b").unwrap();
    stdin.flush().unwrap();
    let status = child.wait().unwrap();
    drop(stdin);
    (status.success(), reader.join().unwrap())
}

/// Mode CLI dans un vrai terminal : le TUI s'affiche et Échap le ferme proprement.
#[test]
fn cli_mode_runs_in_terminal() {
    let (ok, screen) = run_tui("cli", &["--cli"], &[]);
    assert!(ok, "sortie en échec : {screen}");
    assert!(screen.contains("Historique") && screen.contains("En attente"));
}

/// Mode mixte : le TUI tourne pendant que l'API Web écoute en fond.
#[test]
fn both_mode_runs_tui_with_web_server() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let (ok, screen) = run_tui("both", &["--cli", "--web"], &[("WEB_ADDR", &addr)]);
    assert!(ok, "sortie en échec : {screen}");
    assert!(screen.contains("Historique"));
}
