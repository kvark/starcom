use std::{path, process};

use starcom::{core, replay};

const TRANSCRIPT: &[u8] = include_bytes!("data/two-panes.tmux");

#[test]
fn independent_panes_survive_every_chunk_boundary() {
    for split in 0..=TRANSCRIPT.len() {
        let mut replay = replay::Replay::new(core::Size::default());
        replay.feed(&TRANSCRIPT[..split]).unwrap();
        replay.feed(&TRANSCRIPT[split..]).unwrap();
        replay.finish().unwrap();
        assert_eq!(replay.panes().len(), 2);
        let left = replay.panes()[&tmuxctl::PaneId(1)].screen_lines();
        let right = replay.panes()[&tmuxctl::PaneId(2)].screen_lines();
        assert_eq!(&left[..3], ["Starcom shell", "$ cargo test", "done"]);
        assert_eq!(&right[..2], ["ready", "café 界"]);
    }
}

#[test]
fn replay_cli_uses_real_terminal_models_and_safe_diagnostics() {
    let fixture = path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/two-panes.tmux");
    let output = process::Command::new(env!("CARGO_BIN_EXE_starcom"))
        .arg("--replay").arg(fixture).output().unwrap();
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    assert!(!output.stdout.contains(&0x1b));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("2 panes"));
    assert!(text.contains("Pane %1"));
    assert!(text.contains("done"));
    assert!(text.contains("café 界"));
}

#[test]
fn too_many_panes_is_an_explicit_error() {
    let mut replay = replay::Replay::new(core::Size::default());
    for pane in 0..16 {
        replay.feed(format!("%output %{pane} x\n").as_bytes()).unwrap();
    }
    assert!(replay.feed(b"%output %16 x\n").is_err());
}
