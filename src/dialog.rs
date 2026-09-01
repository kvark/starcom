//! A system dialog before the Starcom window exists.
//!
//! Used when saved tabs cannot be read: name the file and the error, then ask
//! whether to clear the file or exit. Exit is the default so a dismissed dialog
//! never deletes anything.

use std::{io, path};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrokenStore {
    Clear,
    Exit,
}

pub(crate) fn ask_clear_or_exit(file: &path::Path, error: &anyhow::Error) -> BrokenStore {
    if ask(&prompt(file, error)) {
        BrokenStore::Clear
    } else {
        BrokenStore::Exit
    }
}

fn prompt(file: &path::Path, error: &anyhow::Error) -> String {
    format!(
        "Starcom could not read the saved tabs file:\n\n{}\n\n{}\n\n\
         Clear the file and start with a new tab, or exit and leave it unchanged?",
        file.display(),
        error
    )
}

/// `true` means Clear. A missing or cancelled dialog is Exit.
fn ask(text: &str) -> bool {
    match platform::ask(text) {
        Ok(clear) => clear,
        Err(error) => {
            eprintln!("starcom: {text}");
            eprintln!("starcom: no system dialog ({error}); leaving the file unchanged");
            false
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO, MessageBoxW,
    };

    pub fn ask(text: &str) -> io::Result<bool> {
        let text = wide(text);
        let title = wide("Starcom");
        // Yes = Clear, No = Exit. Default is No so Enter does not delete.
        let reply = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
            )
        };
        Ok(reply == IDYES)
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::process;

    pub fn ask(text: &str) -> io::Result<bool> {
        let script = format!(
            "display dialog {} with title \"Starcom\" buttons {{\"Exit\", \"Clear\"}} \
             default button \"Exit\" with icon caution",
            applescript_string(text)
        );
        let output = process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()?;
        if !output.status.success() {
            return Ok(false);
        }
        Ok(String::from_utf8_lossy(&output.stdout).contains("Clear"))
    }

    fn applescript_string(text: &str) -> String {
        let mut out = String::from("\"");
        for ch in text.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                _ => out.push(ch),
            }
        }
        out.push('"');
        out
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::*;
    use std::process;

    pub fn ask(text: &str) -> io::Result<bool> {
        match zenity(text) {
            Ok(clear) => Ok(clear),
            Err(error) if error.kind() == io::ErrorKind::NotFound => kdialog(text),
            Err(error) => Err(error),
        }
    }

    fn zenity(text: &str) -> io::Result<bool> {
        // Cancel is the default so Enter does not delete.
        finish(
            "zenity",
            process::Command::new("zenity")
                .args([
                    "--question",
                    "--title=Starcom",
                    "--ok-label=Clear",
                    "--cancel-label=Exit",
                    "--default-cancel",
                    "--no-markup",
                    "--no-wrap",
                    "--text",
                ])
                .arg(text)
                .status(),
        )
    }

    fn kdialog(text: &str) -> io::Result<bool> {
        // kdialog's default is Yes; label Yes as Exit so the default is safe.
        match finish(
            "kdialog",
            process::Command::new("kdialog")
                .args(["--yes-label", "Exit", "--no-label", "Clear", "--yesno"])
                .arg(text)
                .status(),
        ) {
            Ok(exit_clicked) => Ok(!exit_clicked),
            Err(error) => Err(error),
        }
    }

    fn finish(name: &str, status: io::Result<process::ExitStatus>) -> io::Result<bool> {
        let status = status?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(io::Error::other(format!("{name} exited {status}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_names_the_file_and_the_parse_error() {
        let text = prompt(
            path::Path::new("/home/alice/.config/starcom/workspace.conf"),
            &anyhow::anyhow!("line 21: unknown key auth"),
        );
        assert!(text.contains("/home/alice/.config/starcom/workspace.conf"));
        assert!(text.contains("unknown key auth"));
        assert!(text.contains("Clear"));
        assert!(text.contains("exit"));
    }
}
