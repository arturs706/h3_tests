use std::{path::PathBuf, sync::Arc};
use futures::future;
use tokio::io::AsyncWriteExt;
use tracing::{error, info};
use h3_quinn::quinn;
use bytes::Buf;

static ALPN: &[u8] = b"h3";


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .init();

    let uri = "https://localhost:4433".parse::<http::Uri>()?;
    if uri.scheme() != Some(&http::uri::Scheme::HTTPS) {
        Err("uri scheme must be 'https'")?;
    }
    let auth = uri.authority().ok_or("uri must have a host")?.clone();
    let port = auth.port_u16().unwrap_or(443);
    let addr = tokio::net::lookup_host((auth.host(), port))
        .await?
        .next()
        .ok_or("dns found no addresses")?;


    let mut roots = rustls::RootCertStore::empty();
    match rustls_native_certs::load_native_certs() {
        Ok(certs) => {
            for cert in certs {
                if let Err(e) = roots.add(&rustls::Certificate(cert.0)) {
                    error!("failed to parse trust anchor: {}", e);
                }
            }
        }
        Err(e) => {
            error!("couldn't load any default trust roots: {}", e);
        }
    };

    if let Err(e) = roots.add(&rustls::Certificate(std::fs::read(PathBuf::from("certs/ca.cert"))?)) {
        error!("failed to parse trust anchor: {}", e);
    }

    let mut tls_config = rustls::ClientConfig::builder()
        .with_safe_default_cipher_suites()
        .with_safe_default_kx_groups()
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();

    tls_config.enable_early_data = true;
    tls_config.alpn_protocols = vec![ALPN.into()];
    let mut client_endpoint = h3_quinn::quinn::Endpoint::client("[::]:0".parse().unwrap())?;
    let client_config = quinn::ClientConfig::new(Arc::new(tls_config));
    client_endpoint.set_default_client_config(client_config);

    let conn = client_endpoint.connect(addr, auth.host())?.await?;

    let quinn_conn = h3_quinn::Connection::new(conn);
    let (mut driver, mut send_request) = h3::client::new(quinn_conn).await?;

    let drive = async move {
        future::poll_fn(|cx| driver.poll_close(cx)).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let mut response_body = Vec::new();


    let request = async move {
        let req = http::Request::builder().uri(uri).body(())?;
        let mut stream = send_request.send_request(req).await?;
        stream.finish().await?;
        let resp = stream.recv_response().await?;
        while let Some(chunk) = stream.recv_data().await? {
            response_body.extend_from_slice(chunk.chunk());
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    };

    

    let (req_res, drive_res) = tokio::join!(request, drive);
    req_res?;
    drive_res?;
    // wait for the connection to be closed before exiting
    client_endpoint.wait_idle().await;

    Ok(())
}