//! Windows OpenSSH agent transport. Only local named pipes are accepted.
//! Overlapped operations are cancelled and joined before their buffers expire.

use std::{
    env, fs, io,
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    time,
};
use windows_sys::Win32::{
    Foundation as foundation,
    Storage::FileSystem as files,
    System::{IO as system_io, Threading as threading},
};

use super::remaining;

pub struct Stream(fs::File);

const DEFAULT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// A local check, with no connection and no blocking: is an agent plausibly
/// there? Used to warn on the connection form before a connection is tried.
///
/// The pipe directory is enumerated rather than opened. Opening a pipe to ask
/// whether it exists connects to an instance, and the agent has a finite number
/// of them. When the directory cannot be read at all, say nothing: a wrong
/// warning is worse than no warning.
pub fn available() -> bool {
    let path = env::var_os("SSH_AUTH_SOCK").unwrap_or_else(|| DEFAULT_PIPE.into());
    let path = std::path::Path::new(&path);
    let (Some(directory), Some(name)) = (path.parent(), path.file_name()) else {
        return true;
    };
    match fs::read_dir(directory) {
        Ok(entries) => entries.flatten().any(|entry| entry.file_name() == name),
        Err(_) => true,
    }
}

impl Stream {
    pub fn connect(deadline: time::Instant) -> io::Result<Self> {
        remaining(deadline).map_err(io::Error::other)?;
        let path = env::var_os("SSH_AUTH_SOCK").unwrap_or_else(|| DEFAULT_PIPE.into());
        if !path.to_string_lossy().starts_with(r"\\.\pipe\") {
            return Err(io::Error::other(
                "Windows SSH_AUTH_SOCK must name a local pipe",
            ));
        }
        // A busy or unavailable agent is an explicit failure, not a fallback to
        // a different identity. File creation never waits for the pipe server.
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(files::FILE_FLAG_OVERLAPPED)
            .open(&path)
            .map_err(|error| {
                io::Error::other(format!(
                    "the SSH agent at {} could not be reached ({error}). Start the \
                     OpenSSH Authentication Agent service and add a key with \
                     `ssh-add`, or choose a private-key file instead.",
                    path.to_string_lossy()
                ))
            })?;
        Ok(Self(file))
    }

    pub fn read_exact(&mut self, mut bytes: &mut [u8], deadline: time::Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            let count =
                self.transfer(bytes.as_mut_ptr(), bytes.len(), Direction::Read, deadline)?;
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            bytes = &mut bytes[count..];
        }
        Ok(())
    }

    pub fn write_all(&mut self, mut bytes: &[u8], deadline: time::Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            // WriteFile only reads this pointer; Direction controls the API.
            let count = self.transfer(
                bytes.as_ptr().cast_mut(),
                bytes.len(),
                Direction::Write,
                deadline,
            )?;
            if count == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = &bytes[count..];
        }
        Ok(())
    }

    fn transfer(
        &self,
        buffer: *mut u8,
        length: usize,
        direction: Direction,
        deadline: time::Instant,
    ) -> io::Result<usize> {
        let timeout = remaining(deadline).map_err(io::Error::other)?;
        let milliseconds = timeout
            .as_millis()
            .saturating_add(1)
            .min(u128::from(u32::MAX - 1)) as u32;
        // SAFETY: the event and OVERLAPPED remain alive through completion (or
        // cancellation followed by completion), as does the caller's buffer.
        // The file is opened for overlapped I/O and not shared across threads.
        unsafe {
            let handle = threading::CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let event = OwnedHandle::from_raw_handle(handle);
            let mut overlapped = system_io::OVERLAPPED {
                hEvent: event.as_raw_handle(),
                ..std::mem::zeroed()
            };
            let mut count = 0;
            let file = self.0.as_raw_handle();
            let started = match direction {
                Direction::Read => {
                    files::ReadFile(file, buffer, length as u32, &mut count, &mut overlapped)
                }
                Direction::Write => files::WriteFile(
                    file,
                    buffer.cast_const(),
                    length as u32,
                    &mut count,
                    &mut overlapped,
                ),
            };
            if started != 0 {
                return Ok(count as usize);
            }
            let error = foundation::GetLastError();
            if error != foundation::ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            let wait = threading::WaitForSingleObject(event.as_raw_handle(), milliseconds);
            if wait != foundation::WAIT_OBJECT_0 {
                system_io::CancelIoEx(file, &overlapped);
                system_io::GetOverlappedResult(file, &overlapped, &mut count, 1);
                return Err(if wait == foundation::WAIT_TIMEOUT {
                    io::ErrorKind::TimedOut.into()
                } else {
                    io::Error::other("agent pipe wait failed")
                });
            }
            if system_io::GetOverlappedResult(file, &overlapped, &mut count, 0) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(count as usize)
        }
    }
}

enum Direction {
    Read,
    Write,
}
