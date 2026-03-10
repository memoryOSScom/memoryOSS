// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use std::sync::Arc;

use rcgen::{CertificateParams, KeyPair};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use rustls::server::WebPkiClientVerifier;
use tokio_rustls::TlsAcceptor;

use crate::config::TlsConfig;

pub fn build_tls_acceptor(tls_config: &TlsConfig) -> anyhow::Result<TlsAcceptor> {
    let (cert_pem, key_pem) =
        if let (Some(cert_path), Some(key_path)) = (&tls_config.cert_path, &tls_config.key_path) {
            tracing::info!("Loading TLS cert from {}", cert_path.display());
            let cert = std::fs::read(cert_path)?;
            let key = std::fs::read(key_path)?;
            (cert, key)
        } else if tls_config.auto_generate {
            tracing::info!("Auto-generating self-signed TLS certificate");
            generate_self_signed()?
        } else {
            anyhow::bail!("TLS: no cert/key provided and auto_generate is false");
        };

    let certs: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(&cert_pem).collect::<Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_slice(&key_pem)?;

    let mut server_config = if let Some(ref ca_path) = tls_config.client_ca_path {
        // mTLS: require client certificate signed by the given CA
        tracing::info!("mTLS enabled: loading client CA from {}", ca_path.display());
        let ca_pem = std::fs::read(ca_path)?;
        let ca_certs: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(&ca_pem).collect::<Result<Vec<_>, _>>()?;

        let mut root_store = rustls::RootCertStore::empty();
        for ca_cert in ca_certs {
            root_store.add(ca_cert)?;
        }

        // B4 FIX: enforce client certificate when mTLS CA is configured
        let client_verifier = WebPkiClientVerifier::builder(Arc::new(root_store))
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build client verifier: {e}"))?;

        ServerConfig::builder()
            .with_client_cert_verifier(client_verifier)
            .with_single_cert(certs, key)?
    } else {
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)?
    };

    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

fn generate_self_signed() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let mut params =
        CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])?;
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::Utf8String("memoryoss self-signed".to_string()),
    );

    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    let cert_pem = cert.pem().into_bytes();
    let key_pem = key_pair.serialize_pem().into_bytes();

    Ok((cert_pem, key_pem))
}
