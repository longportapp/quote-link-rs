use anyhow::Result;
use boring::asn1::Asn1Time;
use boring::bn::{BigNum, MsbOption};
use boring::error::ErrorStack;
use boring::hash::MessageDigest;
use boring::pkey::{PKey, Private};
use boring::rsa::Rsa;
use boring::ssl::{SslContextBuilder, SslMethod, SslVerifyMode, SslVersion};
use boring::x509::extension::{BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectKeyIdentifier};
use boring::x509::{X509NameBuilder, X509};

/// Make a CA certificate and private key
pub(crate) fn mk_ca_cert() -> Result<(X509, PKey<Private>), ErrorStack> {
    let rsa = Rsa::generate(2048)?;
    let privkey = PKey::from_rsa(rsa)?;

    let mut x509_name = X509NameBuilder::new()?;
    x509_name.append_entry_by_text("C", "CN")?;
    x509_name.append_entry_by_text("ST", "TX")?;
    x509_name.append_entry_by_text("O", "Longbridge organization")?;
    x509_name.append_entry_by_text("CN", "quote-link-server.rs")?;
    let x509_name = x509_name.build();

    let mut cert_builder = X509::builder()?;
    cert_builder.set_version(2)?;
    let serial_number = {
        let mut serial = BigNum::new()?;
        serial.rand(159, MsbOption::MAYBE_ZERO, false)?;
        serial.to_asn1_integer()?
    };
    cert_builder.set_serial_number(&serial_number)?;
    cert_builder.set_subject_name(&x509_name)?;
    cert_builder.set_issuer_name(&x509_name)?;
    cert_builder.set_pubkey(&privkey)?;
    let not_before = Asn1Time::days_from_now(0)?;
    cert_builder.set_not_before(&not_before)?;
    let not_after = Asn1Time::days_from_now(365)?;
    cert_builder.set_not_after(&not_after)?;

    // Generate a leaf certificate suitable for TLS 1.3 server authentication
    // - Not a CA cert
    cert_builder.append_extension(BasicConstraints::new().critical().build()?)?;
    // - Key usage must include digitalSignature; keyEncipherment is harmless and commonly used
    cert_builder.append_extension(
        KeyUsage::new()
            .critical()
            .digital_signature()
            .key_encipherment()
            .build()?,
    )?;
    // - Extended Key Usage: serverAuth for TLS server
    cert_builder.append_extension(ExtendedKeyUsage::new().server_auth().build()?)?;

    let subject_key_identifier =
        SubjectKeyIdentifier::new().build(&cert_builder.x509v3_context(None, None))?;
    cert_builder.append_extension(subject_key_identifier)?;
    cert_builder.sign(&privkey, MessageDigest::sha256())?;

    let cert = cert_builder.build();
    Ok((cert, privkey))
}

/// 创建带有自生成证书的BoringSSL上下文构建器
pub(crate) fn new_tls_context_builder() -> Result<SslContextBuilder> {
    let mut ctx_builder = SslContextBuilder::new(SslMethod::tls())?;

    // 强制使用TLS 1.3（QUIC要求）
    ctx_builder.set_min_proto_version(Some(SslVersion::TLS1_3))?;
    ctx_builder.set_max_proto_version(Some(SslVersion::TLS1_3))?;

    let (cert, key) = mk_ca_cert()?;
    ctx_builder.set_certificate(&cert)?;
    ctx_builder.set_private_key(&key)?;
    ctx_builder.check_private_key()?;
    ctx_builder.set_verify(SslVerifyMode::NONE);

    // 设置ALPN协议
    ctx_builder.set_alpn_protos(b"\x11quic-echo-example")?;

    Ok(ctx_builder)
}
