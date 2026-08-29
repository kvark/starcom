//! Small live-transport diagnostic, not the planned desktop interface.
use std::{collections, env, ffi, path, process, time};

use anyhow::Context;
use starcom::{core, inspect, ssh};

const HELP: &str = "Starcom live SSH/tmux inspector (read-only; no GUI or restored terminal)\n\
\n\
Usage: starcom-inspect --host HOST --user USER --session NAME --known-hosts FILE\n\
                      [--identity FILE | --agent] [--port PORT] [--socket PATH]\n\
                      [--history LINES] [--timeout SECONDS]\n\
\n\
HOST is a DNS name or unbracketed IP, not an SSH config alias. This first backend\n\
does not read ~/.ssh/config. Supply connection settings explicitly.\n\
Host keys must already be trusted in the supplied OpenSSH known_hosts file.\n\
Unknown/changed keys are never accepted automatically. Marker/pattern policies\n\
not supported by this backend are rejected, not ignored.\n\
Use --agent for an encrypted key already loaded in an SSH agent; --identity\n\
currently accepts an unencrypted private-key file. Exactly one is required.\n\
Defaults: port 22, history 200 lines (max 1000), timeout 10 seconds per operation.\n\
OS DNS resolution and local agent IPC are not covered by the network timeout.\n\
Captures are escaped textual observations, NOT an atomic/interactive session.\n";

fn main() -> process::ExitCode {
    match run() {
        Ok(()) => process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("starcom-inspect: {}", format!("{error:#}").escape_debug());
            process::ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let mut args = env::args_os().skip(1);
    let mut values = collections::BTreeMap::<String, ffi::OsString>::new();
    let mut agent = false;
    while let Some(arg) = args.next() {
        let arg = arg.to_str().context("option must be UTF-8")?;
        match arg {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            "--agent" => {
                anyhow::ensure!(!agent, "--agent supplied twice");
                agent = true;
            }
            "--host" | "--user" | "--session" | "--known-hosts" | "--identity" | "--port"
            | "--socket" | "--history" | "--timeout" => {
                let value = args
                    .next()
                    .with_context(|| format!("{arg} needs a value"))?;
                anyhow::ensure!(
                    values.insert(arg.to_owned(), value).is_none(),
                    "duplicate option {arg}"
                );
            }
            _ => anyhow::bail!("unknown option {arg:?}; use --help"),
        }
    }
    if values.is_empty() && !agent {
        print!("{HELP}");
        return Ok(());
    }
    let host = text(&mut values, "--host")?;
    let user = text(&mut values, "--user")?;
    let session = core::SessionName::new(text(&mut values, "--session")?)?;
    let known_hosts = path::PathBuf::from(
        values
            .remove("--known-hosts")
            .context("--known-hosts is required")?,
    );
    let identity = values.remove("--identity").map(path::PathBuf::from);
    anyhow::ensure!(
        agent != identity.is_some(),
        "choose exactly one of --identity and --agent"
    );
    let authentication = match identity {
        Some(path) => ssh::Authentication::Identity(path),
        None => ssh::Authentication::Agent,
    };
    let port = optional_number(&mut values, "--port", 22)?;
    let history = optional_number(&mut values, "--history", 200)?;
    let seconds: u64 = optional_number(&mut values, "--timeout", 10)?;
    let socket = values
        .remove("--socket")
        .map(|value| {
            value
                .into_string()
                .map_err(|_| anyhow::anyhow!("remote socket path must be UTF-8"))
        })
        .transpose()?;
    anyhow::ensure!(
        history <= inspect::MAX_HISTORY_LINES,
        "--history exceeds {}",
        inspect::MAX_HISTORY_LINES
    );
    let options = ssh::Options {
        host,
        port,
        user,
        known_hosts,
        authentication,
        timeout: time::Duration::from_secs(seconds),
    };
    let mut inspector = inspect::Inspector::attach(&options, &session, socket.as_deref())?;
    let observation = inspector.observe(history)?;
    println!(
        "Starcom inspection: tmux {:?}, session {}, {} panes, verified {}",
        observation.tmux_version,
        observation.session,
        observation.captures.len(),
        observation.fingerprint
    );
    println!("Read-only observations; not an atomic snapshot or an interactive/restored terminal.");
    if observation.metadata_changed_during_capture {
        println!("Warning: pane metadata changed during capture.");
    }
    for capture in observation.captures {
        println!(
            "Pane {} window {}: {}x{} at {},{}; cursor {},{}; alternate={}; retained history={}/{}",
            capture.pane.id,
            capture.pane.window,
            capture.pane.size.columns(),
            capture.pane.size.rows(),
            capture.pane.left,
            capture.pane.top,
            capture.pane.cursor_x,
            capture.pane.cursor_y,
            capture.pane.alternate_screen,
            capture.pane.history_size,
            capture.pane.history_limit
        );
        for (row, text) in capture.escaped_rows.iter().enumerate() {
            if !text.is_empty() {
                println!("  {row:>4}: {text:?}");
            }
        }
    }
    Ok(())
}

fn text(
    values: &mut collections::BTreeMap<String, ffi::OsString>,
    key: &str,
) -> anyhow::Result<String> {
    values
        .remove(key)
        .with_context(|| format!("{key} is required"))?
        .into_string()
        .map_err(|_| anyhow::anyhow!("{key} must be UTF-8"))
}

fn optional_number<T: std::str::FromStr>(
    values: &mut collections::BTreeMap<String, ffi::OsString>,
    key: &str,
    default: T,
) -> anyhow::Result<T> {
    match values.remove(key) {
        Some(value) => value
            .to_str()
            .with_context(|| format!("{key} must be UTF-8"))?
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid number for {key}")),
        None => Ok(default),
    }
}
