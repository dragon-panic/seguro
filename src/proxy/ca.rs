use color_eyre::eyre::{Result, WrapErr};
use rcgen::{BasicConstraints, Certificate, CertificateParams, DistinguishedName, IsCa, KeyPair};

/// A self-signed CA used for TLS inspection (--tls-inspect).
///
/// Kept in memory for the session lifetime; used to sign per-domain leaf certs
/// on demand inside `handle_connect_mitm`.
pub struct Ca {
    /// rcgen certificate object, kept alive for `signed_by` calls.
    cert: Certificate,
    /// rcgen key pair, kept alive for `signed_by` calls.
    key: KeyPair,
    /// PEM-encoded CA cert for injection into the guest's trust store.
    cert_pem: String,
}

impl Ca {
    /// Generate a new ephemeral CA for TLS inspection.
    pub fn generate() -> Result<Self> {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let mut dn = DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, "Seguro Inspection CA");
        dn.push(rcgen::DnType::OrganizationName, "Seguro");
        params.distinguished_name = dn;

        let key = KeyPair::generate().wrap_err("generating CA key pair")?;
        let cert = params.self_signed(&key).wrap_err("generating CA cert")?;
        let cert_pem = cert.pem();

        Ok(Self { cert, key, cert_pem })
    }

    /// PEM-encoded CA certificate, suitable for writing into the guest trust store.
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// Sign a TLS leaf certificate for `host`.
    ///
    /// Returns `(cert_der, key_der)` — both in DER format, ready for `rustls`.
    pub fn sign_for_host(&self, host: &str) -> Result<(Vec<u8>, Vec<u8>)> {
        let params = CertificateParams::new(vec![host.to_string()])
            .wrap_err("building leaf cert params")?;
        let leaf_key = KeyPair::generate().wrap_err("generating leaf key")?;
        let leaf_cert = params
            .signed_by(&leaf_key, &self.cert, &self.key)
            .wrap_err("signing leaf cert")?;

        Ok((leaf_cert.der().to_vec(), leaf_key.serialize_der()))
    }
}
