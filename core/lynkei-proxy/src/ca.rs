//! Local certificate authority: creates/loads the Lynkei root CA and mints per-host leaf certificates.

use std::fmt;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use lru::LruCache;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SerialNumber,
};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use time::{Duration, OffsetDateTime};

pub const CA_CERT_FILE: &str = "lynkei-ca.pem";
const CA_KEY_FILE: &str = "lynkei-ca-key.pem";
const LEAF_CACHE_SIZE: usize = 1024;

pub struct CertAuthority {
    issuer: Issuer<'static, KeyPair>,
    ca_pem: String,
    ca_der: CertificateDer<'static>,
    ca_path: PathBuf,
    provider: Arc<CryptoProvider>,
    cache: Mutex<LruCache<String, Arc<CertifiedKey>>>,
}

impl fmt::Debug for CertAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertAuthority")
            .field("ca_path", &self.ca_path)
            .finish_non_exhaustive()
    }
}

fn random_serial() -> SerialNumber {
    let mut bytes: [u8; 16] = rand::random();
    bytes[0] &= 0x7f; // keep it a positive ASN.1 INTEGER
    SerialNumber::from_slice(&bytes)
}

impl CertAuthority {
    /// Loads the CA from `dir`, creating a new one on first use.
    pub fn load_or_create(dir: &Path, provider: Arc<CryptoProvider>) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);

        let (ca_pem, key) = if cert_path.exists() && key_path.exists() {
            let key =
                KeyPair::from_pem(&fs::read_to_string(&key_path)?).context("reading CA key")?;
            (fs::read_to_string(&cert_path)?, key)
        } else {
            let key = KeyPair::generate().context("generating CA key")?;
            let mut params = CertificateParams::default();
            let host = hostname();
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, format!("Lynkei Local CA ({host})"));
            dn.push(DnType::OrganizationName, "Lynkei");
            dn.push(
                DnType::OrganizationalUnitName,
                "Generated locally - never share the key",
            );
            params.distinguished_name = dn;
            params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
            params.key_usages = vec![
                KeyUsagePurpose::KeyCertSign,
                KeyUsagePurpose::CrlSign,
                KeyUsagePurpose::DigitalSignature,
            ];
            let now = OffsetDateTime::now_utc();
            params.not_before = now - Duration::days(1);
            params.not_after = now + Duration::days(3650);
            params.serial_number = Some(random_serial());
            let cert = params.self_signed(&key).context("self-signing CA")?;
            let pem = cert.pem();
            fs::write(&cert_path, &pem)?;
            write_private(&key_path, &key.serialize_pem())?;
            (pem, key)
        };

        let ca_der = CertificateDer::from_pem_slice(ca_pem.as_bytes()).context("parsing CA PEM")?;
        let issuer = Issuer::from_ca_cert_pem(&ca_pem, key).context("loading CA as issuer")?;
        Ok(Self {
            issuer,
            ca_pem,
            ca_der,
            ca_path: cert_path,
            provider,
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(LEAF_CACHE_SIZE).unwrap())),
        })
    }

    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }

    pub fn ca_der(&self) -> &CertificateDer<'static> {
        &self.ca_der
    }

    pub fn ca_path(&self) -> &Path {
        &self.ca_path
    }

    /// Returns a (cached) leaf certificate for `host`, signed by the Lynkei CA.
    pub fn leaf_for(&self, host: &str) -> Result<Arc<CertifiedKey>> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if let Some(hit) = self.cache.lock().unwrap().get(&host) {
            return Ok(hit.clone());
        }
        let mut params = CertificateParams::new(vec![host.clone()]).context("leaf SAN")?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host.clone());
        params.distinguished_name = dn;
        let now = OffsetDateTime::now_utc();
        // Apple requires leaf validity <= 398 days for TLS server certs.
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(365);
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.use_authority_key_identifier_extension = true;
        params.serial_number = Some(random_serial());

        let key = KeyPair::generate().context("generating leaf key")?;
        let cert = params
            .signed_by(&key, &self.issuer)
            .context("signing leaf")?;
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let signing_key = self
            .provider
            .key_provider
            .load_private_key(key_der)
            .context("loading leaf key")?;
        let certified = Arc::new(CertifiedKey::new(
            vec![cert.der().clone(), self.ca_der.clone()],
            signing_key,
        ));
        self.cache.lock().unwrap().put(host, certified.clone());
        Ok(certified)
    }
}

/// Picks the leaf certificate from the TLS SNI, falling back to the CONNECT host.
#[derive(Debug)]
pub struct SniResolver {
    pub ca: Arc<CertAuthority>,
    pub fallback_host: String,
}

impl ResolvesServerCert for SniResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let host = hello
            .server_name()
            .unwrap_or(&self.fallback_host)
            .to_owned();
        match self.ca.leaf_for(&host) {
            Ok(k) => Some(k),
            Err(e) => {
                tracing::warn!(%host, error = %e, "could not mint leaf certificate");
                None
            }
        }
    }
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
        })
        .map(|s| s.trim().trim_end_matches(".local").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this device".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_reloads_same_ca() {
        let dir = std::env::temp_dir().join(format!("lynkei-ca-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let a = CertAuthority::load_or_create(&dir, provider.clone()).unwrap();
        let b = CertAuthority::load_or_create(&dir, provider).unwrap();
        assert_eq!(a.ca_pem(), b.ca_pem());
        assert_eq!(a.ca_der(), b.ca_der());
        let leaf = a.leaf_for("API.AcmeShop.com.").unwrap();
        assert_eq!(leaf.cert.len(), 2, "leaf + CA chain");
        assert!(
            Arc::ptr_eq(&leaf, &a.leaf_for("api.acmeshop.com").unwrap()),
            "cached by normalized host"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
