use std::{env, fs, io, path, process};

use anyhow::Context;
use starcom::{core, replay};

const HELP: &str = "Starcom: Session Terminal And Remote COMmander\n\
\n\
Headless replay harness. Use starcom-inspect for read-only live SSH inspection.\n\
\n\
Usage: starcom --replay FILE [--size COLSxROWS]\n\
       starcom --help\n\
\n\
Use - as FILE to read a synthetic control-mode transcript from stdin.\n\
The default pane size is 80x24. All replay panes use the same fixed size.\n\
Output rows are quoted/escaped so remote text cannot control this terminal.\n";

fn main() -> process::ExitCode {
    match run() {
        Ok(()) => process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("starcom: {}", format!("{error:#}").escape_debug());
            process::ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let mut arguments = env::args_os().skip(1);
    let mut input = None;
    let mut size = core::Size::default();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--help" | "-h") => {
                print!("{HELP}");
                return Ok(());
            }
            Some("--version") => {
                println!("starcom {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            Some("--replay") => {
                anyhow::ensure!(input.is_none(), "--replay was supplied more than once");
                input = Some(path::PathBuf::from(
                    arguments.next().context("--replay needs a file")?,
                ));
            }
            Some("--size") => {
                let argument = arguments.next().context("--size needs COLSxROWS")?;
                let text = argument.to_str().context("size must be UTF-8")?;
                let (columns, rows) = text.split_once('x').context("size must be COLSxROWS")?;
                size = core::Size::new(columns.parse()?, rows.parse()?)?;
            }
            _ => anyhow::bail!("unrecognized argument {argument:?}; use --help"),
        }
    }
    let Some(input) = input else {
        print!("{HELP}");
        return Ok(());
    };
    let mut reader: Box<dyn io::Read> = if input == path::Path::new("-") {
        Box::new(io::stdin())
    } else {
        Box::new(fs::File::open(&input).with_context(|| format!("open {input:?}"))?)
    };
    let mut replay = replay::Replay::new(size);
    let mut buffer = [0; 4096];
    let mut total = 0usize;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count;
        anyhow::ensure!(
            total <= 16 * 1024 * 1024,
            "replay exceeds the 16 MiB input budget"
        );
        replay.feed(&buffer[..count])?;
    }
    replay.finish()?;
    println!(
        "Starcom replay: {} panes, {}x{} cells (no network connection)",
        replay.panes().len(),
        size.columns(),
        size.rows()
    );
    for (pane, terminal) in replay.panes() {
        println!("Pane {pane}:");
        for (row, text) in terminal.screen_lines().iter().enumerate() {
            if !text.is_empty() {
                println!("  {row:>3}: {text:?}");
            }
        }
    }
    Ok(())
}
