//! Whose certificates Kumi trusts. First the certificate authorities it carries (webpki-roots), as before; when those
//! don't vouch for a server, this computer's own trust store, asked as the system asks it (rustls-platform-verifier),
//! with SSL_CERT_FILE's certificates besides. A school or office network that inspects secure connections works once
//! its certificate is installed on the computer, as the browser and the installer need. The system is asked only when
//! the carried authorities refuse, so a connection that worked before is checked as before.

use std::path::Path;
use std::sync::{Arc, LazyLock, OnceLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};

/// A client's TLS: Kumi's trust, TLS 1.2 and 1.3, and HTTP/2 offered unless `http1`, as reqwest's own had it. Every
/// client shares one trust, so the system's is loaded once, when it's first needed.
pub(super) fn config(http1: bool) -> rustls::ClientConfig {
    static CONFIG: LazyLock<rustls::ClientConfig> = LazyLock::new(|| configured(Arc::new(Trust::new(provider()))));
    let mut config = CONFIG.clone();
    if http1 {
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
    }
    config
}

fn configured(trust: Arc<Trust>) -> rustls::ClientConfig {
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::clone(&trust.provider))
        .with_protocol_versions(rustls::ALL_VERSIONS)
        .expect("TLS 1.2 and 1.3")
        .dangerous()
        .with_custom_certificate_verifier(trust)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config
}

/// The cryptography reqwest's TLS used: one the program installed, else ring.
fn provider() -> Arc<CryptoProvider> {
    CryptoProvider::get_default().cloned().unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()))
}

/// The carried authorities, then the system's trust store.
#[derive(Debug)]
struct Trust {
    carried: Arc<WebPkiServerVerifier>,
    /// None when the system has no trust store to ask (a Linux without a certificate bundle).
    system: OnceLock<Option<Arc<dyn ServerCertVerifier>>>,
    provider: Arc<CryptoProvider>,
}

impl Trust {
    fn new(provider: Arc<CryptoProvider>) -> Self {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self::with(roots, OnceLock::new(), provider)
    }

    /// With other carried authorities and a system already decided (for tests).
    fn with(carried: RootCertStore, system: OnceLock<Option<Arc<dyn ServerCertVerifier>>>, provider: Arc<CryptoProvider>) -> Self {
        let carried =
            WebPkiServerVerifier::builder_with_provider(Arc::new(carried), Arc::clone(&provider)).build().expect("the carried authorities");
        Self { carried, system, provider }
    }

    fn system(&self) -> Option<&Arc<dyn ServerCertVerifier>> {
        self.system.get_or_init(|| system_verifier(&self.provider)).as_ref()
    }
}

/// The system's trust store, with SSL_CERT_FILE's certificates besides when it names a file of them.
fn system_verifier(provider: &Arc<CryptoProvider>) -> Option<Arc<dyn ServerCertVerifier>> {
    use rustls_platform_verifier::Verifier;
    let extra = std::env::var_os("SSL_CERT_FILE").map(|path| certificates(Path::new(&path))).unwrap_or_default();
    // Certificates the system can't take are left out rather than losing its trust store with them.
    let verifier = (!extra.is_empty())
        .then(|| Verifier::new_with_extra_roots(extra, Arc::clone(provider)).ok())
        .flatten()
        .or_else(|| Verifier::new(Arc::clone(provider)).ok())?;
    Some(Arc::new(verifier))
}

/// The certificates in a file: PEM, or one in DER, as Windows exports it.
fn certificates(path: &Path) -> Vec<CertificateDer<'static>> {
    use rustls::pki_types::pem::PemObject;
    let pem: Vec<_> = CertificateDer::pem_file_iter(path).map(|found| found.filter_map(Result::ok).collect()).unwrap_or_default();
    if !pem.is_empty() {
        return pem;
    }
    std::fs::read(path).ok().filter(|der| der.first() == Some(&0x30)).map(|der| vec![CertificateDer::from(der)]).unwrap_or_default()
}

/// A refusal for something other than who issued the certificate.
fn telling(refused: &Error) -> bool {
    use CertificateError::*;
    matches!(
        refused,
        Error::InvalidCertificate(
            NotValidForName
                | NotValidForNameContext { .. }
                | Expired
                | ExpiredContext { .. }
                | NotValidYet
                | NotValidYetContext { .. }
                | Revoked
                | InvalidPurpose
                | InvalidPurposeContext { .. }
        )
    )
}

impl ServerCertVerifier for Trust {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let carried = self.carried.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now);
        // A certificate no carried authority vouches for (an office's own, say): the system's word. When it refuses
        // too, the refusal reads as the carried one, the same on every system, unless the system knew the authority
        // and found something else wrong (the name, the dates, a revocation), which says more.
        if let (Err(Error::InvalidCertificate(why)), Some(system)) = (&carried, self.system()) {
            match system.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now) {
                Ok(verified) => return Ok(verified),
                Err(refused) if *why == CertificateError::UnknownIssuer && telling(&refused) => return Err(refused),
                Err(_) => {}
            }
        }
        carried
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.carried.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.carried.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.carried.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose};

    /// A certificate authority of its own, as a school's or an office's network has.
    fn authority(name: &str) -> (CertificateDer<'static>, Issuer<'static, KeyPair>) {
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, name);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let key = KeyPair::generate().unwrap();
        let der = params.self_signed(&key).unwrap().der().clone();
        (der, Issuer::new(params, key))
    }

    fn server(host: &str, by: &Issuer<'_, KeyPair>) -> CertificateDer<'static> {
        let mut params = CertificateParams::new(vec![host.to_string()]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.signed_by(&KeyPair::generate().unwrap(), by).unwrap().der().clone()
    }

    fn roots(authorities: &[&CertificateDer<'static>]) -> RootCertStore {
        let mut store = RootCertStore::empty();
        authorities.iter().for_each(|authority| store.add((*authority).clone()).unwrap());
        store
    }

    /// The system, as a verifier trusting `authorities` alone.
    fn system(authorities: &[&CertificateDer<'static>]) -> OnceLock<Option<Arc<dyn ServerCertVerifier>>> {
        let verifier = WebPkiServerVerifier::builder_with_provider(Arc::new(roots(authorities)), provider()).build().unwrap();
        OnceLock::from(Some(verifier as Arc<dyn ServerCertVerifier>))
    }

    fn check(trust: &Trust, certificate: &CertificateDer<'_>, host: &str) -> Result<ServerCertVerified, Error> {
        trust.verify_server_cert(certificate, &[], &ServerName::try_from(host.to_string()).unwrap(), &[], UnixTime::now())
    }

    #[test]
    fn the_system_vouches_for_a_certificate_the_carried_authorities_dont() {
        let (public, public_issuer) = authority("A public authority");
        let (office, office_issuer) = authority("An office's inspecting proxy");
        let both = Trust::with(roots(&[&public]), system(&[&office]), provider());
        assert!(check(&both, &server("api.openai.com", &public_issuer), "api.openai.com").is_ok(), "the carried ones, as before");
        assert!(check(&both, &server("api.openai.com", &office_issuer), "api.openai.com").is_ok(), "the system's");
        let before = Trust::with(roots(&[&public]), OnceLock::from(None), provider());
        assert!(matches!(
            check(&before, &server("api.openai.com", &office_issuer), "api.openai.com"),
            Err(Error::InvalidCertificate(CertificateError::UnknownIssuer))
        ));
    }

    #[test]
    fn a_refusal_reads_as_the_carried_authorities_have_it() {
        let (public, public_issuer) = authority("A public authority");
        let (office, office_issuer) = authority("An office's inspecting proxy");
        let (_, stranger) = authority("Nobody's");
        let trust = Trust::with(roots(&[&public]), system(&[&office]), provider());
        assert!(matches!(
            check(&trust, &server("api.openai.com", &stranger), "api.openai.com"),
            Err(Error::InvalidCertificate(CertificateError::UnknownIssuer))
        ));
        // Another server's certificate: the carried authorities' refusal, or the system's when only it knows the issuer.
        for issuer in [&public_issuer, &office_issuer] {
            let refused = check(&trust, &server("github.com", issuer), "api.openai.com");
            assert!(
                matches!(
                    refused,
                    Err(Error::InvalidCertificate(CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }))
                ),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn ssl_cert_file_reads_pem_and_der_and_nothing_else() {
        let folder = tempfile::tempdir().unwrap();
        let (one, _) = authority("One");
        let (two, _) = authority("Two");
        let pem = |der: &CertificateDer<'_>| {
            use base64::Engine;
            let body = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
            format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n")
        };
        let bundle = folder.path().join("bundle.pem");
        std::fs::write(&bundle, format!("# an office's\n{}{}", pem(&one), pem(&two))).unwrap();
        assert_eq!(certificates(&bundle), [one.clone(), two]);
        let der = folder.path().join("office.cer");
        std::fs::write(&der, one.as_ref()).unwrap();
        assert_eq!(certificates(&der), [one]);
        let text = folder.path().join("notes.txt");
        std::fs::write(&text, "not a certificate").unwrap();
        assert!(certificates(&text).is_empty() && certificates(&folder.path().join("missing.pem")).is_empty());
    }
}
