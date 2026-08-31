//! Keys are loaded only after the server's host key was accepted.
//! RustCrypto signs identity-file requests; the local agent signs agent requests.

use std::{collections, fs, io, time};

use super::{Authentication, Error, Kind, agent};
use signature::Signer as _;

pub(super) struct Credentials {
    keys: collections::VecDeque<sunset::SignKey>,
    signer: Signer,
    offered: usize,
}

enum Signer {
    Identity(Box<ssh_key::PrivateKey>),
    Agent(agent::Agent),
}

impl Credentials {
    pub fn load(authentication: &Authentication, deadline: time::Instant) -> Result<Self, Error> {
        match *authentication {
            Authentication::Identity(ref path) => {
                let file = fs::File::open(path).map_err(error)?;
                let mut bytes = zeroize::Zeroizing::new(Vec::new());
                io::Read::read_to_end(&mut io::Read::take(file, 1024 * 1024 + 1), &mut bytes)
                    .map_err(error)?;
                if bytes.len() > 1024 * 1024 {
                    return Err(error("identity file exceeds 1 MiB"));
                }
                let private = ssh_key::PrivateKey::from_openssh(&*bytes)
                    .map_err(|_| error("unsupported OpenSSH private key"))?;
                if private.is_encrypted() {
                    return Err(error("load encrypted keys into your SSH agent first"));
                }
                let wire = private.public_key().to_bytes().map_err(error)?;
                let (public, used) =
                    sunset::sshwire::read_ssh::<sunset::PubKey<'_>>(&wire, None).map_err(error)?;
                if used != wire.len() {
                    return Err(error("trailing public-key data"));
                }
                let key = sunset::SignKey::from_agent_pubkey(&public).map_err(error)?;
                Ok(Self {
                    keys: [key].into_iter().collect(),
                    signer: Signer::Identity(Box::new(private)),
                    offered: 0,
                })
            }
            Authentication::Agent => {
                let mut agent = agent::Agent::connect(deadline)?;
                let keys = agent.identities(deadline)?;
                Ok(Self {
                    keys,
                    signer: Signer::Agent(agent),
                    offered: 0,
                })
            }
        }
    }

    pub fn next_key(&mut self) -> Result<sunset::SignKey, Error> {
        let key = self.keys.pop_front().ok_or_else(|| {
            if self.offered == 0 {
                error("no supported identities were available to offer")
            } else {
                error("public-key authentication was rejected")
            }
        })?;
        self.offered += 1;
        Ok(key)
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
        let signed = match self.signer {
            Signer::Identity(ref private) => {
                if private.public_key().to_bytes().map_err(error)? != key {
                    return Err(error("identity mismatch"));
                }
                let signature: ssh_key::Signature = if let Some(rsa) = private.key_data().rsa() {
                    (rsa, Some(ssh_key::HashAlg::Sha256))
                        .try_sign(&message)
                        .map_err(error)?
                } else {
                    private.try_sign(&message).map_err(error)?
                };
                let mut wire = Vec::new();
                agent::put_string(&mut wire, signature.algorithm().as_str().as_bytes())?;
                agent::put_string(&mut wire, signature.as_bytes())?;
                agent::parse_signature(&wire, algorithm)?
            }
            Signer::Agent(ref mut agent) => agent.sign(&key, &message, algorithm, deadline)?,
        };
        request.signed(&signed).map_err(error)
    }
}

fn error(detail: impl std::fmt::Display) -> Error {
    Error::new(Kind::Authentication, detail)
}
