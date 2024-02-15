use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use bytes::{Bytes, BytesMut};
use http::{Request, StatusCode};
use rustls::{Certificate, PrivateKey};
use tokio::{fs::File, io::AsyncReadExt};
use tracing::{error, info, trace_span};
use http::{header, Response};
use serde_json::{json, to_string, to_vec};
use h3::{error::ErrorLevel, quic::BidiStream, server::RequestStream};
use h3_quinn::quinn;
use chrono;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::{self, FromRow};
use uuid::Uuid;

static ALPN: &[u8] = b"h3";


#[derive(Serialize, Deserialize, FromRow, Debug)]

pub struct StaffUser {
    pub user_id: Option<Uuid>,
    pub name: Option<String>,
    pub username: String,
    pub mob_phone: Option<String>,
    pub passwd: String,
    pub acc_level: Option<String>,
    pub status: Option<String>,
    pub a_created: Option<NaiveDateTime>,
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .init();

    let cert = Certificate(std::fs::read(PathBuf::from("certs/server.cert"))?);
    let key = PrivateKey(std::fs::read(PathBuf::from("certs/server.key"))?);

    let mut tls_config = rustls::ServerConfig::builder()
        .with_safe_default_cipher_suites()
        .with_safe_default_kx_groups()
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;

    tls_config.max_early_data_size = u32::MAX;
    tls_config.alpn_protocols = vec![ALPN.into()];
    let listen: SocketAddr = "[::1]:4433".parse().unwrap();
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(tls_config));
    let endpoint = quinn::Endpoint::server(server_config, listen)?;

    info!("listening on {}", listen);

    // handle incoming connections and requests

    while let Some(new_conn) = endpoint.accept().await {
        trace_span!("New connection being attempted");
        let root: Arc<Option<PathBuf>> = Arc::new(Some(PathBuf::from("static")));
        tokio::spawn(async move {
            match new_conn.await {
                Ok(conn) => {
                    info!("new connection established");

                    let mut h3_conn = h3::server::Connection::new(h3_quinn::Connection::new(conn))
                        .await
                        .unwrap();

                    loop {
                        match h3_conn.accept().await {
                            Ok(Some((req, stream))) => {
                                info!("new request: {:#?}", req);
                                let root = root.clone();
                                tokio::spawn(async {
                                    if let Err(e) = handle_request(req, stream, root).await {
                                        error!("failed to handle request: {}", e);
                                    }
                                });
                            }
                            // indicating no more streams to be received
                            Ok(None) => {
                                break;
                            }

                            Err(err) => {
                                error!("error on accept {}", err);
                                match err.get_error_level() {
                                    ErrorLevel::ConnectionError => break,
                                    ErrorLevel::StreamError => continue,
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    error!("accepting connection failed: {:?}", err);
                }
            }
        });
    }

    endpoint.wait_idle().await;

    Ok(())
}


async fn handle_request<T>(
    req: http::Request<()>,
    mut stream: h3::server::RequestStream<T, Bytes>,
    _serve_root: Arc<Option<PathBuf>>,
) -> Result<(), Box<dyn std::error::Error>>
where
    T: h3::quic::BidiStream<Bytes>,
{
    // Create a JSON response
    let json_response = json!({ "message": "Hello world" });

    // Serialize JSON data
    let json_bytes = to_vec(&json_response)?;

    // Create an HTTP response with JSON content type
    let resp = Response::builder()
        .status(http::StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(())?; // Empty body

    // Send the response headers
    if let Err(err) = stream.send_response(resp).await {
        error!("unable to send response headers to connection peer: {:?}", err);
        return Ok(());
    }

    // Send the JSON data
    if let Err(err) = stream.send_data(Bytes::from(json_bytes)).await {
        error!("unable to send JSON data to connection peer: {:?}", err);
    }

    Ok(stream.finish().await?)
}