//! Bounded local OpenSSH-agent messages. No agent forwarding or key export.

use std::{collections, time};

use super::{Error, Kind, remaining};

const MAX_MESSAGE: usize = 256 * 1024;
const MAX_IDENTITIES: u32 = 64;

pub(super) struct Agent {
    stream: platform::Stream,
}

impl Agent {
    pub fn connect(deadline: time::Instant) -> Result<Self, Error> {
        Ok(Self {
            stream: platform::Stream::connect(deadline).map_err(authentication)?,
        })
    }

    pub fn identities(
        &mut self,
        deadline: time::Instant,
    ) -> Result<collections::VecDeque<sunset::SignKey>, Error> {
        let reply = self.request(&[11], deadline)?;
        let mut wire = Wire::new(&reply);
        if wire.byte()? != 12 {
            return Err(authentication("agent refused to list identities"));
        }
        let count = wire.number()?;
        if count > MAX_IDENTITIES {
            return Err(authentication("agent returned more than 64 identities"));
        }
        let mut keys = collections::VecDeque::new();
        for _ in 0..count {
            let key = wire.string()?;
            let _comment = wire.string()?;
            if let Ok((public, used)) = sunset::sshwire::read_ssh::<sunset::PubKey<'_>>(key, None)
                && used == key.len()
                && let Ok(key) = sunset::SignKey::from_agent_pubkey(&public)
            {
                keys.push_back(key);
            }
        }
        wire.finish()?;
        Ok(keys)
    }

    pub fn sign(
        &mut self,
        key: &[u8],
        message: &[u8],
        algorithm: &str,
        deadline: time::Instant,
    ) -> Result<sunset::OwnedSig, Error> {
        let flags = if algorithm == "rsa-sha2-256" { 2u32 } else { 0 };
        let mut request = vec![13];
        put_string(&mut request, key)?;
        put_string(&mut request, message)?;
        request.extend_from_slice(&flags.to_be_bytes());
        let reply = self.request(&request, deadline)?;
        let mut wire = Wire::new(&reply);
        if wire.byte()? != 14 {
            return Err(authentication("agent refused to sign"));
        }
        let signature = parse_signature(wire.string()?, algorithm)?;
        wire.finish()?;
        Ok(signature)
    }

    fn request(&mut self, bytes: &[u8], deadline: time::Instant) -> Result<Vec<u8>, Error> {
        if bytes.is_empty() || bytes.len() > MAX_MESSAGE {
            return Err(authentication("agent request exceeds budget"));
        }
        self.stream
            .write_all(&(bytes.len() as u32).to_be_bytes(), deadline)
            .map_err(authentication)?;
        self.stream
            .write_all(bytes, deadline)
            .map_err(authentication)?;
        let mut length = [0; 4];
        self.stream
            .read_exact(&mut length, deadline)
            .map_err(authentication)?;
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > MAX_MESSAGE {
            return Err(authentication("agent reply exceeds budget"));
        }
        let mut result = vec![0; length];
        self.stream
            .read_exact(&mut result, deadline)
            .map_err(authentication)?;
        Ok(result)
    }
}

pub(super) fn parse_signature(bytes: &[u8], expected: &str) -> Result<sunset::OwnedSig, Error> {
    let (signature, used) = sunset::sshwire::read_ssh::<sunset::Signature<'_>>(bytes, None)
        .map_err(|_| authentication("invalid SSH signature encoding"))?;
    if used != bytes.len() || signature.algorithm_name().ok() != Some(expected) {
        return Err(authentication(
            "signature algorithm does not match the requested key",
        ));
    }
    sunset::OwnedSig::try_from(signature).map_err(authentication)
}

pub(super) fn put_string(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    if out.len().saturating_add(4).saturating_add(bytes.len()) > MAX_MESSAGE {
        return Err(authentication("agent field exceeds budget"));
    }
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

struct Wire<'a>(&'a [u8]);

impl<'a> Wire<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        if count > self.0.len() {
            return Err(authentication("truncated agent reply"));
        }
        let (value, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn number(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("length checked"),
        ))
    }
    fn string(&mut self) -> Result<&'a [u8], Error> {
        let length = self.number()? as usize;
        self.take(length)
    }
    fn finish(self) -> Result<(), Error> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(authentication("trailing agent reply data"))
        }
    }
}

fn authentication(detail: impl std::fmt::Display) -> Error {
    Error::new(Kind::Authentication, detail)
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::{env, io, os::unix::net};

    pub struct Stream(net::UnixStream);

    impl Stream {
        pub fn connect(deadline: time::Instant) -> io::Result<Self> {
            remaining(deadline).map_err(io::Error::other)?;
            let path = env::var_os("SSH_AUTH_SOCK")
                .filter(|path| !path.is_empty())
                .ok_or_else(|| io::Error::other("SSH_AUTH_SOCK is not set"))?;
            Ok(Self(net::UnixStream::connect(path)?))
        }
        pub fn read_exact(
            &mut self,
            mut bytes: &mut [u8],
            deadline: time::Instant,
        ) -> io::Result<()> {
            while !bytes.is_empty() {
                self.0
                    .set_read_timeout(Some(remaining(deadline).map_err(io::Error::other)?))?;
                match io::Read::read(&mut self.0, bytes) {
                    Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                    Ok(count) => bytes = &mut bytes[count..],
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        }
        pub fn write_all(&mut self, mut bytes: &[u8], deadline: time::Instant) -> io::Result<()> {
            while !bytes.is_empty() {
                self.0
                    .set_write_timeout(Some(remaining(deadline).map_err(io::Error::other)?))?;
                match io::Write::write(&mut self.0, bytes) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(count) => bytes = &bytes[count..],
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        }
    }
}

#[cfg(windows)]
#[path = "agent_windows.rs"]
mod platform;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a disposable OpenSSH agent; scripts/test-ssh.sh or scripts/test-windows-agent.py"]
    fn signs_with_isolated_openssh_agent() {
        let path = std::env::var_os("STARCOM_AGENT_TEST_PUBKEY").expect("use the agent fixture");
        let key = ssh_key::PublicKey::read_openssh_file(path).unwrap();
        let wire = key.to_bytes().unwrap();
        let deadline = time::Instant::now() + time::Duration::from_secs(5);
        let mut agent = Agent::connect(deadline).unwrap();
        let keys = agent.identities(deadline).unwrap();
        assert!(keys.iter().any(|key| {
            let mut candidate = Vec::new();
            sunset::sshwire::ssh_push_vec(&mut candidate, &key.pubkey()).unwrap();
            candidate == wire
        }));
        let message = b"Starcom local agent integration test, not a remote authentication request";
        let signature = agent.sign(&wire, message, "ssh-ed25519", deadline).unwrap();
        let sunset::OwnedSig::Ed25519(signature) = signature else {
            panic!("wrong signing algorithm");
        };
        let signature =
            ssh_key::Signature::new(ssh_key::Algorithm::Ed25519, signature.to_vec()).unwrap();
        signature::Verifier::verify(&key, message, &signature).unwrap();
    }

    #[test]
    fn agent_lengths_and_trailing_data_are_rejected() {
        assert!(Wire::new(&[0xff; 4]).string().is_err());
        assert!(Wire::new(&[0, 0, 0]).number().is_err());
        assert!(Wire::new(&[1]).finish().is_err());
        assert!(put_string(&mut Vec::new(), &vec![0; MAX_MESSAGE]).is_err());
    }

    #[test]
    fn signatures_cannot_change_algorithm_or_smuggle_trailing_bytes() {
        let mut wire = Vec::new();
        put_string(&mut wire, b"ssh-ed25519").unwrap();
        put_string(&mut wire, &[0; 64]).unwrap();
        assert!(parse_signature(&wire, "ssh-ed25519").is_ok());
        assert!(parse_signature(&wire, "rsa-sha2-256").is_err());
        wire.push(0);
        assert!(parse_signature(&wire, "ssh-ed25519").is_err());
    }
}
