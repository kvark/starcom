//! Keys are loaded only after the server's host key was accepted.
//! Identity files are offered first; the local agent is appended unless
//! `IdentitiesOnly` closed that path. RustCrypto signs files; the agent signs
//! its own keys, including `sk-ssh-ed25519` (the authenticator holds the
//! private half). `sk-ecdsa-*` is still skipped.

use std::{collections, fs, io, path, time};

use super::{Authentication, Error, Kind, agent};
use signature::Signer as _;

pub(super) struct Credentials {
    keys: collections::VecDeque<PreparedKey>,
    current: Option<PreparedKey>,
    agent: Option<agent::Agent>,
    skipped: Vec<String>,
    offered: usize,
}

struct PreparedKey {
    offered: sunset::SignKey,
    source: KeySource,
}

enum KeySource {
    File(Box<ssh_key::PrivateKey>),
    Agent,
}

impl Credentials {
    pub fn load(authentication: &Authentication, deadline: time::Instant) -> Result<Self, Error> {
        let mut keys = collections::VecDeque::new();
        let mut skipped = Vec::new();
        let mut agent = None;
        for path in &authentication.files {
            match load_file(path) {
                Ok(prepared) => keys.push_back(prepared),
                Err(reason) => skipped.push(reason),
            }
        }
        if authentication.agent {
            match agent::Agent::connect(deadline) {
                Ok(mut connected) => match connected.identities(deadline) {
                    Ok(listed) => {
                        skipped.extend(listed.skipped);
                        for key in listed.keys {
                            keys.push_back(PreparedKey {
                                offered: key,
                                source: KeySource::Agent,
                            });
                        }
                        agent = Some(connected);
                    }
                    // Files already loaded: an agent that holds only SK keys
                    // must not fail the whole offer. The skip reason is kept
                    // for the later "not offered" message.
                    Err(error) if keys.is_empty() => return Err(error),
                    Err(error) => skipped.push(format!("SSH agent ({})", error.detail())),
                },
                Err(error) if keys.is_empty() => return Err(error),
                Err(error) => skipped.push(format!("SSH agent ({})", error.detail())),
            }
        }
        if keys.is_empty() {
            return Err(error(empty_offer(
                authentication.files.len(),
                authentication.agent,
                &skipped,
            )));
        }
        Ok(Self {
            keys,
            current: None,
            agent,
            skipped,
            offered: 0,
        })
    }

    pub fn next_key(&mut self) -> Result<sunset::SignKey, Error> {
        let prepared = self.keys.pop_front().ok_or_else(|| {
            if self.offered == 0 {
                error("no supported identities were available to offer")
            } else if self.skipped.is_empty() {
                error("public-key authentication was rejected")
            } else {
                error(format!(
                    "public-key authentication was rejected; not offered: {}",
                    self.skipped.join(", ")
                ))
            }
        })?;
        let offered = prepared.offered.clone();
        self.current = Some(prepared);
        self.offered += 1;
        Ok(offered)
    }

    pub fn offered(&self) -> usize {
        self.offered
    }

    pub fn sign(
        &mut self,
        request: sunset::event::RequestSign<'_, '_>,
        deadline: time::Instant,
    ) -> Result<(), Error> {
        let public = request.key().map_err(error)?.pubkey();
        let algorithm = match public {
            sunset::PubKey::Ed25519(_) => "ssh-ed25519",
            sunset::PubKey::RSA(_) => "rsa-sha2-256",
            sunset::PubKey::ECDSA256(_) => "ecdsa-sha2-nistp256",
            sunset::PubKey::SkEd25519(_) => "sk-ssh-ed25519@openssh.com",
            _ => return Err(error("unsupported signing key")),
        };
        let mut key = Vec::new();
        sunset::sshwire::ssh_push_vec(&mut key, &public).map_err(error)?;
        let mut message = Vec::new();
        sunset::sshwire::ssh_push_vec(&mut message, &request.message().map_err(error)?)
            .map_err(error)?;
        if key.len() > 16 * 1024 || message.len() > 32 * 1024 {
            return Err(error("signing request exceeds budget"));
        }
        let signed = match self.current.as_ref().map(|prepared| &prepared.source) {
            Some(KeySource::File(private)) => sign_file(private, &key, &message, algorithm)?,
            Some(KeySource::Agent) => self
                .agent
                .as_mut()
                .ok_or_else(|| error("agent signing request without an agent"))?
                .sign(&key, &message, algorithm, deadline)?,
            None => return Err(error("signing request without an offered key")),
        };
        request.signed(&signed).map_err(error)
    }
}

fn sign_file(
    private: &ssh_key::PrivateKey,
    key: &[u8],
    message: &[u8],
    algorithm: &str,
) -> Result<sunset::OwnedSig, Error> {
    if private.public_key().to_bytes().map_err(error)? != key {
        return Err(error("identity mismatch"));
    }
    let signature: ssh_key::Signature = if let Some(rsa) = private.key_data().rsa() {
        (rsa, Some(ssh_key::HashAlg::Sha256))
            .try_sign(message)
            .map_err(error)?
    } else {
        private.try_sign(message).map_err(error)?
    };
    let mut wire = Vec::new();
    agent::put_string(&mut wire, signature.algorithm().as_str().as_bytes())?;
    agent::put_string(&mut wire, signature.as_bytes())?;
    agent::parse_signature(&wire, algorithm)
}

fn load_file(path: &path::Path) -> Result<PreparedKey, String> {
    let display = path.display();
    let file = fs::File::open(path).map_err(|error| format!("{display}: {error}"))?;
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    io::Read::read_to_end(&mut io::Read::take(file, 1024 * 1024 + 1), &mut bytes)
        .map_err(|error| format!("{display}: {error}"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(format!("{display}: identity file exceeds 1 MiB"));
    }
    let private = ssh_key::PrivateKey::from_openssh(&*bytes)
        .map_err(|_| format!("{display}: unsupported OpenSSH private key"))?;
    if private.is_encrypted() {
        return Err(format!(
            "{display}: encrypted; add it with ssh-add so the agent can sign"
        ));
    }
    let wire = private
        .public_key()
        .to_bytes()
        .map_err(|error| format!("{display}: {error}"))?;
    let (public, used) = sunset::sshwire::read_ssh::<sunset::PubKey<'_>>(&wire, None)
        .map_err(|_| format!("{display}: unsupported public key"))?;
    if used != wire.len() {
        return Err(format!("{display}: trailing public-key data"));
    }
    let offered = sunset::SignKey::from_agent_pubkey(&public)
        .map_err(|_| format!("{display}: not ed25519, ECDSA P-256, or RSA"))?;
    Ok(PreparedKey {
        offered,
        source: KeySource::File(Box::new(private)),
    })
}

fn empty_offer(files: usize, wanted_agent: bool, skipped: &[String]) -> String {
    let mut parts = Vec::new();
    if files > 0 {
        parts.push(format!(
            "none of {files} IdentityFile {} could be offered",
            if files == 1 { "entry" } else { "entries" }
        ));
    }
    if wanted_agent && files == 0 {
        parts.push("no supported identities were available".into());
    }
    if !skipped.is_empty() {
        parts.push(format!("skipped: {}", skipped.join("; ")));
    }
    if parts.is_empty() {
        "no identity files and no SSH agent were configured".into()
    } else {
        parts.join("; ")
    }
}

fn error(detail: impl std::fmt::Display) -> Error {
    Error::new(Kind::Authentication, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_offer_names_skipped_files_and_the_agent() {
        let text = empty_offer(
            2,
            true,
            &[
                "/home/a/.ssh/id_ed25519: encrypted; add it with ssh-add so the agent can sign"
                    .into(),
                "SSH agent (the SSH agent holds 3 keys, 1 unsupported (sk-ecdsa-sha2-nistp256@openssh.com))"
                    .into(),
            ],
        );
        assert!(text.contains("none of 2 IdentityFile entries"));
        assert!(text.contains("skipped:"));
        assert!(text.contains("sk-ecdsa-sha2-nistp256@openssh.com"));
    }

    #[test]
    fn empty_offer_without_files_does_not_invent_a_count() {
        let text = empty_offer(0, false, &[]);
        assert_eq!(text, "no identity files and no SSH agent were configured");
    }
}
