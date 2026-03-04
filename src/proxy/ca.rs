use color_eyre::eyre::{Result, WrapErr};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};

/// A self-signed CA certificate used for TLS inspection (--tls-inspect).
pub struct Ca {
    pub cert_pem: String,
    pub key_pem: String,
}

impl Ca {
    /// Generate a new ephemeral CA for TLS inspection.
    pub fn generate() -> Result<Self> {
        let mut params = CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let mut dn = DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, "Seguro Inspection CA");
        dn.push(rcgen::DnType::OrganizationName, "Seguro");
        params.distinguished_name = dn;

        let key = KeyPair::generate().wrap_err("generating CA key pair")?;
        let cert = params.self_signed(&key).wrap_err("generating CA cert")?;
        let cert_pem = cert.pem();
        let key_pem = key.serialize_pem();

        Ok(Self { cert_pem, key_pem })
    }
}
