//! GUI events to user input. Plain Ctrl-C/X/V remain terminal control keys;
//! Ctrl-Shift-C/V (Cmd-C/V on macOS) operate the local clipboard.

use crate::{command, input};

pub enum Event {
    Input(Vec<input::Action>),
    Copy,
    Paste(input::Paste),
    RequestPaste,
}

fn bytes(bytes: &[u8]) -> Event {
    Event::Input(
        bytes
            .chunks(command::MAX_INPUT_BYTES)
            .map(|chunk| input::Action::Bytes(chunk.to_vec()))
            .collect(),
    )
}

pub fn translate(
    event: &egui::Event,
    modifiers: egui::Modifiers,
) -> Result<Option<Event>, input::Error> {
    let out = match *event {
        egui::Event::Copy if modifiers.ctrl && !modifiers.shift && !modifiers.mac_cmd => {
            bytes(&[3])
        }
        egui::Event::Cut if modifiers.ctrl && !modifiers.shift && !modifiers.mac_cmd => {
            bytes(&[24])
        }
        egui::Event::Copy | egui::Event::Cut => Event::Copy,
        egui::Event::Paste(ref text) => Event::Paste(input::Paste::new(text)?),
        egui::Event::Text(ref text) | egui::Event::Ime(egui::ImeEvent::Commit(ref text)) => {
            if text.is_empty() {
                return Ok(None);
            }
            if text.len() > input::MAX_PASTE_BYTES || text.chars().any(char::is_control) {
                return Err(input::Error::InputSize);
            }
            if modifiers.mac_cmd {
                return Ok(None);
            }
            if modifiers.alt && !modifiers.ctrl && !cfg!(target_os = "macos") {
                let mut data = vec![0x1b];
                data.extend_from_slice(text.as_bytes());
                bytes(&data)
            } else {
                bytes(text.as_bytes())
            }
        }
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            if modifiers.mac_cmd {
                return Ok(None);
            }
            if key == egui::Key::Insert && modifiers.shift && !modifiers.ctrl && !modifiers.alt {
                return Ok(Some(Event::RequestPaste));
            }
            // Control letters do not produce egui Text events. These mappings
            // deliberately do not interpret a physical key as printable text.
            if modifiers.ctrl
                && let Some(byte) = control_byte(key)
            {
                let mut data = Vec::new();
                if modifiers.alt {
                    data.push(0x1b);
                }
                data.push(byte);
                return Ok(Some(bytes(&data)));
            }
            let key = match key {
                egui::Key::ArrowUp => input::Key::Up,
                egui::Key::ArrowDown => input::Key::Down,
                egui::Key::ArrowLeft => input::Key::Left,
                egui::Key::ArrowRight => input::Key::Right,
                egui::Key::Enter => input::Key::Enter,
                egui::Key::Tab => input::Key::Tab,
                egui::Key::Backspace => input::Key::Backspace,
                egui::Key::Escape => input::Key::Escape,
                egui::Key::Home => input::Key::Home,
                egui::Key::End => input::Key::End,
                egui::Key::Insert => input::Key::Insert,
                egui::Key::Delete => input::Key::Delete,
                egui::Key::PageUp => input::Key::PageUp,
                egui::Key::PageDown => input::Key::PageDown,
                key => {
                    if let Some(number) = key
                        .name()
                        .strip_prefix('F')
                        .and_then(|n| n.parse::<u8>().ok())
                    {
                        input::Key::Function(number)
                    } else {
                        return Ok(None);
                    }
                }
            };
            let modifiers = input::Modifiers {
                control: modifiers.ctrl,
                alt: modifiers.alt,
                shift: modifiers.shift,
            };
            key.name(modifiers)?;
            Event::Input(vec![input::Action::Key(key, modifiers)])
        }
        _ => return Ok(None),
    };
    Ok(Some(out))
}

fn control_byte(key: egui::Key) -> Option<u8> {
    let name = key.name();
    if name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() {
        return Some(name.as_bytes()[0].to_ascii_uppercase() & 0x1f);
    }
    match key {
        egui::Key::Space | egui::Key::Num2 => Some(0),
        egui::Key::OpenBracket | egui::Key::Num3 => Some(27),
        egui::Key::Backslash | egui::Key::Pipe | egui::Key::Num4 => Some(28),
        egui::Key::CloseBracket | egui::Key::Num5 => Some(29),
        egui::Key::Num6 => Some(30),
        egui::Key::Minus | egui::Key::Slash | egui::Key::Num7 => Some(31),
        egui::Key::Num8 | egui::Key::Questionmark => Some(127),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn raw(event: Option<Event>) -> Vec<u8> {
        let Some(Event::Input(actions)) = event else {
            panic!("expected terminal bytes")
        };
        actions
            .into_iter()
            .flat_map(|action| match action {
                input::Action::Bytes(bytes) => bytes,
                _ => panic!("not bytes"),
            })
            .collect()
    }
    #[test]
    fn control_c_is_interrupt_but_shift_copy_and_mac_copy_are_local() {
        assert_eq!(
            raw(translate(&egui::Event::Copy, egui::Modifiers::CTRL).unwrap()),
            [3]
        );
        assert!(matches!(
            translate(
                &egui::Event::Copy,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT
            )
            .unwrap(),
            Some(Event::Copy)
        ));
        assert!(matches!(
            translate(&egui::Event::Copy, egui::Modifiers::MAC_CMD).unwrap(),
            Some(Event::Copy)
        ));
    }
    #[test]
    fn text_and_committed_ime_are_utf8_not_duplicated_key_names() {
        let event = egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(translate(&event, egui::Modifiers::NONE).unwrap().is_none());
        assert_eq!(
            raw(translate(&egui::Event::Text("café界".into()), egui::Modifiers::NONE).unwrap()),
            "café界".as_bytes()
        );
        assert!(
            translate(
                &egui::Event::Ime(egui::ImeEvent::Preedit("候補".into())),
                egui::Modifiers::NONE
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            raw(translate(
                &egui::Event::Ime(egui::ImeEvent::Commit("漢字".into())),
                egui::Modifiers::NONE
            )
            .unwrap()),
            "漢字".as_bytes()
        );
    }
    #[test]
    fn key_release_does_not_repeat_input_and_special_keys_keep_modifiers() {
        let event = egui::Event::Key {
            key: egui::Key::ArrowLeft,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        };
        let Some(Event::Input(actions)) = translate(&event, egui::Modifiers::NONE).unwrap() else {
            panic!()
        };
        assert!(matches!(
            actions[0],
            input::Action::Key(input::Key::Left, input::Modifiers { control: true, .. })
        ));
        let event = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(translate(&event, egui::Modifiers::NONE).unwrap().is_none());
    }
    #[test]
    fn alt_text_and_control_letters_are_single_byte_sequences() {
        assert_eq!(
            raw(translate(&egui::Event::Text("b".into()), egui::Modifiers::ALT).unwrap()),
            if cfg!(target_os = "macos") {
                b"b".as_slice()
            } else {
                b"\x1bb".as_slice()
            }
        );
        let event = egui::Event::Key {
            key: egui::Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        };
        assert_eq!(raw(translate(&event, egui::Modifiers::CTRL).unwrap()), [26]);
    }
}
