use std::sync::{Arc, OnceLock};

use rustls::{ClientConfig, RootCertStore};
use tokio_tungstenite::Connector;

fn client_config(roots: RootCertStore) -> ClientConfig {
    ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

pub(super) fn connector() -> Connector {
    // Keep the same Mozilla roots and TLS defaults as tungstenite. Sharing the
    // config retains rustls's bounded, server-name-scoped session ticket cache.
    // Early data remains disabled: Responses requests must never use TLS 0-RTT.
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    Connector::Rustls(Arc::clone(CONFIG.get_or_init(|| {
        Arc::new(client_config(RootCertStore::from_iter(
            webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
        )))
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
    use rustls::{ClientConnection, HandshakeKind, ServerConfig, ServerConnection};
    use std::io::Cursor;

    // Self-signed localhost-only test certificate and public test key; never
    // added to the production trust store. Valid September 2026–August 2126.
    const CERT: &str = "MIIBpDCCAUmgAwIBAgIUVQqq9VxO4hn7AzyNqJcKP6Ny6hAwCgYIKoZIzj0EAwIwFDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MDkxMDE0NTgwNFoYDzIxMjYwODE3MTQ1ODA0WjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAShEstzQNRN8AJd2Iio8k0jZJSjqktxZ0g/JVT8HrBMfZUpJP/LCFAeM/XjCVJErWy9E1/YrFxjaImGpupFw+F5o3cwdTAdBgNVHQ4EFgQUnSFwYYP41524UKIHKQe9rp7MGoIwHwYDVR0jBBgwFoAUnSFwYYP41524UKIHKQe9rp7MGoIwJQYDVR0RBB4wHIIJbG9jYWxob3N0gg9vdGhlci5sb2NhbGhvc3QwDAYDVR0TAQH/BAIwADAKBggqhkjOPQQDAgNJADBGAiEA7oRxgaw31J17HmwIZ/jyJzIBjFH5oSg51tkS6O07MTICIQDXSjrHGTyWxX6SpUOasHp5PwhJD/NN5TElZCp5w0+VXg==";
    const KEY: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgCgwHnIe6m6meBhbcPL0HjLvwVrWCh0J06f3ZaaG31S2hRANCAAShEstzQNRN8AJd2Iio8k0jZJSjqktxZ0g/JVT8HrBMfZUpJP/LCFAeM/XjCVJErWy9E1/YrFxjaImGpupFw+F5";

    fn certificate() -> CertificateDer<'static> {
        base64::engine::general_purpose::STANDARD
            .decode(CERT)
            .unwrap()
            .into()
    }

    fn server_config() -> Arc<ServerConfig> {
        let key = base64::engine::general_purpose::STANDARD
            .decode(KEY)
            .unwrap();
        Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![certificate()], PrivatePkcs8KeyDer::from(key).into())
                .unwrap(),
        )
    }

    fn trusted_client() -> Arc<ClientConfig> {
        let mut roots = RootCertStore::empty();
        roots.add(certificate()).unwrap();
        Arc::new(client_config(roots))
    }

    fn handshake(
        client: Arc<ClientConfig>,
        server: Arc<ServerConfig>,
        name: &'static str,
    ) -> anyhow::Result<(HandshakeKind, usize)> {
        let mut client = ClientConnection::new(client, name.try_into()?)?;
        let mut server = ServerConnection::new(server)?;
        let mut bytes = 0;
        for _ in 0..8 {
            let mut wire = Vec::new();
            client.write_tls(&mut wire)?;
            bytes += wire.len();
            server.read_tls(&mut Cursor::new(wire))?;
            server.process_new_packets()?;
            let mut wire = Vec::new();
            server.write_tls(&mut wire)?;
            bytes += wire.len();
            client.read_tls(&mut Cursor::new(wire))?;
            client.process_new_packets()?;
            if !client.is_handshaking()
                && !server.is_handshaking()
                && !client.wants_write()
                && !server.wants_write()
            {
                return Ok((client.handshake_kind().unwrap(), bytes));
            }
        }
        anyhow::bail!("TLS handshake did not finish")
    }

    #[test]
    fn shared_tls_config_resumes_without_early_data_and_preserves_verification() {
        let (Connector::Rustls(first), Connector::Rustls(second)) = (connector(), connector())
        else {
            panic!("expected rustls connectors");
        };
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!first.enable_early_data);
        let server = server_config();
        assert!(handshake(first, server.clone(), "localhost").is_err());
        let client = trusted_client();
        assert!(!client.enable_early_data);
        let full = handshake(client.clone(), server.clone(), "localhost").unwrap();
        let resumed = handshake(client.clone(), server.clone(), "localhost").unwrap();
        assert_eq!(full.0, HandshakeKind::Full);
        assert_eq!(resumed.0, HandshakeKind::Resumed);
        assert!(resumed.1 < full.1, "full={full:?}, resumed={resumed:?}");
        println!(
            "TLS handshake bytes: full={}, resumed={}",
            full.1, resumed.1
        );
        // A different hostname gets a full handshake even with the same cert.
        assert_eq!(
            handshake(client.clone(), server.clone(), "other.localhost")
                .unwrap()
                .0,
            HandshakeKind::Full
        );
        assert!(handshake(client.clone(), server, "wrong.localhost").is_err());
        // A restarted server can reject the ticket and complete a full handshake.
        assert_eq!(
            handshake(client, server_config(), "localhost").unwrap().0,
            HandshakeKind::Full
        );
    }
}
