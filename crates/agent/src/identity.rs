//! This machine's self-signed certificate, and the fingerprints used to pin the peer's.

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tracing::info;

const CERT_FILE: &str = "cert.pem";
const KEY_FILE: &str = "key.pem";

/// SHA-256 of a certificate's DER bytes, written `AB:CD:…` (32 bytes).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub fn of(cert: &CertificateDer<'_>) -> Self {
        let digest = ring::digest::digest(&ring::digest::SHA256, cert);
        Self(digest.as_ref().try_into().expect("SHA-256 is 32 bytes"))
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, byte) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(":")?;
            }
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Fingerprint {
    type Err = anyhow::Error;

    /// Accepts 64 hex digits in either case, with or without colons or spaces between them.
    fn from_str(s: &str) -> Result<Self> {
        let digits = s
            .chars()
            .filter(|c| !matches!(c, ':' | ' '))
            .map(|c| {
                c.to_digit(16)
                    .with_context(|| format!("'{c}' isn't a hex digit"))
            })
            .collect::<Result<Vec<u32>>>()?;
        if digits.len() != 64 {
            bail!(
                "a fingerprint has 64 hex digits (32 bytes), this has {}",
                digits.len()
            );
        }
        let mut bytes = [0; 32];
        for (byte, pair) in bytes.iter_mut().zip(digits.chunks(2)) {
            *byte = (pair[0] * 16 + pair[1]) as u8;
        }
        Ok(Self(bytes))
    }
}

/// A certificate and its private key.
pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
    pub fingerprint: Fingerprint,
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Self {
            cert: self.cert.clone(),
            key: self.key.clone_key(),
            fingerprint: self.fingerprint,
        }
    }
}

impl Identity {
    /// Loads `cert.pem` and `key.pem` from `dir`, creating both if neither exists.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        match (cert_path.exists(), key_path.exists()) {
            (true, true) => {}
            (false, false) => {
                let (cert, key) = generate_pem()?;
                fs::create_dir_all(dir)
                    .with_context(|| format!("can't create {}", dir.display()))?;
                write_private(&key_path, &key)?;
                fs::write(&cert_path, cert)
                    .with_context(|| format!("can't write {}", cert_path.display()))?;
                info!(
                    "created a certificate for this machine in {}",
                    dir.display()
                );
            }
            _ => bail!(
                "{} has only one of {CERT_FILE} and {KEY_FILE}. Delete the other one to create \
                 a new certificate; the other machine then has to trust its new fingerprint",
                dir.display()
            ),
        }
        let cert = CertificateDer::from_pem_file(&cert_path)
            .with_context(|| format!("can't read {}", cert_path.display()))?;
        let key = PrivateKeyDer::from_pem_file(&key_path)
            .with_context(|| format!("can't read {}", key_path.display()))?;
        Ok(Self::new(cert, key))
    }

    /// A new certificate that's only kept in memory.
    #[cfg(test)]
    pub fn generate() -> Result<Self> {
        let (cert, key) = generate_pem()?;
        Ok(Self::new(
            CertificateDer::from_pem_slice(cert.as_bytes())?,
            PrivateKeyDer::from_pem_slice(key.as_bytes())?,
        ))
    }

    fn new(cert: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> Self {
        let fingerprint = Fingerprint::of(&cert);
        Self {
            cert,
            key,
            fingerprint,
        }
    }
}

/// A self-signed ECDSA P-256 certificate and its key, as PEM. It never expires: the peer trusts
/// it by fingerprint, not by date or issuer.
fn generate_pem() -> Result<(String, String)> {
    let key = rcgen::KeyPair::generate()?;
    let mut params = rcgen::CertificateParams::new(vec!["crossglide".to_string()])?;
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        format!("crossglide agent on {}", crate::hostname()),
    );
    let cert = params.self_signed(&key)?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// Writes a new file that only the current user can read (on Windows, the user's profile
/// directory already restricts that).
fn write_private(path: &Path, contents: &str) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .with_context(|| format!("can't write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_round_trips_through_text() {
        let fp = Identity::generate().unwrap().fingerprint;
        let text = fp.to_string();
        assert_eq!(text.len(), 32 * 3 - 1);
        assert_eq!(text.parse::<Fingerprint>().unwrap(), fp);
        // Also without colons, in lower case, as some tools print it.
        let bare = text.replace(':', "").to_lowercase();
        assert_eq!(bare.parse::<Fingerprint>().unwrap(), fp);
    }

    #[test]
    fn fingerprint_rejects_bad_text() {
        assert!("".parse::<Fingerprint>().is_err());
        assert!("AB:CD".parse::<Fingerprint>().is_err());
        // `u8::from_str_radix` would accept a leading '+'.
        let plus = format!("+F{}", "0".repeat(62));
        assert!(plus.parse::<Fingerprint>().is_err());
        let not_hex = format!("G{}", "0".repeat(63));
        assert!(not_hex.parse::<Fingerprint>().is_err());
    }

    #[test]
    fn identity_is_created_once_then_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let created = Identity::load_or_create(dir.path()).unwrap();
        let loaded = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(created.fingerprint, loaded.fingerprint);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn half_an_identity_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        Identity::load_or_create(dir.path()).unwrap();
        fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
        assert!(Identity::load_or_create(dir.path()).is_err());
    }
}
