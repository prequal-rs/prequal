//! A gRPC channel to the EPP, optionally over TLS that trusts any certificate: prequal-epp serves a fresh
//! self-signed one, as Envoy's ext_proc cluster (no validation context) accepts in the llm-d chart.

use std::sync::Arc;

use hyper_util::rt::TokioIo;
use rustls::{
    ClientConfig, DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tonic::transport::{Channel, Endpoint, Uri};

#[derive(Debug)]
struct AcceptAny(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

pub async fn channel(port: u16, tls: bool) -> Result<Channel, Box<dyn std::error::Error>> {
    let endpoint = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))?;
    if !tls {
        return Ok(endpoint.connect().await?);
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let connector = TlsConnector::from(Arc::new(config));
    let connect = tower::service_fn(move |uri: Uri| {
        let connector = connector.clone();
        async move {
            let tcp = TcpStream::connect((uri.host().unwrap_or("127.0.0.1"), uri.port_u16().unwrap_or(port))).await?;
            tcp.set_nodelay(true)?;
            let name = ServerName::try_from("prequal-epp").expect("valid name");
            Ok::<_, std::io::Error>(TokioIo::new(connector.connect(name, tcp).await?))
        }
    });
    Ok(endpoint.connect_with_connector(connect).await?)
}
