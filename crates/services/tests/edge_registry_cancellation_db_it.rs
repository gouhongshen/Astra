//! Cancellation at real MySQL protocol boundaries, using an isolated live test database.
//! ASTRA_TEST_DB_IT=1 cargo test -p astra-services --test edge_registry_cancellation_db_it -- --ignored

mod common;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use astra_services::multi_agent::{DatabaseEdgeRegistryService, EdgeRegistryService};
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlSslMode};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};

#[derive(Clone, Copy)]
enum Boundary {
    Begin,
    CurrentReadWrite,
}

// Only the first matching exchange is gated. Requests reach the real database
// unchanged; its response is retained until the test cancels the caller. This
// exercises cancellation after server execution, not a sleep before any SQL.
struct ResponseGate {
    armed: AtomicBool,
    pending: AtomicBool,
    received: Notify,
    release: Notify,
}

struct ResponseProxy {
    port: u16,
    gate: Arc<ResponseGate>,
    task: JoinHandle<()>,
}

impl Drop for ResponseProxy {
    fn drop(&mut self) {
        // Dropping the accept task drops its JoinSet and closes every relay,
        // including on assertion failure. No fault-injection socket survives.
        self.task.abort();
    }
}

async fn packet(reader: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Vec<u8>> {
    let mut header = [0; 4];
    reader.read_exact(&mut header).await?;
    let length =
        usize::from(header[0]) | (usize::from(header[1]) << 8) | (usize::from(header[2]) << 16);
    let mut bytes = vec![0; length + 4];
    bytes[..4].copy_from_slice(&header);
    reader.read_exact(&mut bytes[4..]).await?;
    Ok(bytes)
}

impl ResponseProxy {
    async fn start(host: String, port: u16, boundary: Boundary) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = listener.local_addr().unwrap().port();
        let gate = Arc::new(ResponseGate {
            armed: AtomicBool::new(true),
            pending: AtomicBool::new(false),
            received: Notify::new(),
            release: Notify::new(),
        });
        let relay_gate = gate.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (client, _) = accepted.unwrap();
                        let host = host.clone();
                        let gate = relay_gate.clone();
                        connections.spawn(async move {
                            let server = TcpStream::connect((host.as_str(), port)).await.unwrap();
                            let (mut client_read, mut client_write) = client.into_split();
                            let (mut server_read, mut server_write) = server.into_split();
                            let request_gate = gate.clone();
                            let requests = async move {
                                let mut current_read_prepared = false;
                                loop {
                                    let bytes = packet(&mut client_read).await?;
                                    let payload = &bytes[4..];
                                    // COM_STMT_PREPARE for the current-read barrier is
                                    // immediately followed by its COM_STMT_EXECUTE on
                                    // this serial checkout. Gate execution, not prepare.
                                    if payload.first() == Some(&0x16) {
                                        current_read_prepared = payload[1..].starts_with(
                                            b"UPDATE edge_agent_registry SET last_heartbeat_at = last_heartbeat_at"
                                        );
                                    }
                                    let matches = match boundary {
                                        Boundary::Begin => payload == b"\x03BEGIN",
                                        Boundary::CurrentReadWrite => {
                                            payload.first() == Some(&0x17) && current_read_prepared
                                        }
                                    };
                                    if matches && request_gate.armed.swap(false, Ordering::SeqCst) {
                                        request_gate.pending.store(true, Ordering::SeqCst);
                                    }
                                    server_write.write_all(&bytes).await?;
                                }
                                #[allow(unreachable_code)]
                                Ok::<(), std::io::Error>(())
                            };
                            let responses = async move {
                                loop {
                                    let bytes = packet(&mut server_read).await?;
                                    if gate.pending.swap(false, Ordering::SeqCst) {
                                        assert_eq!(bytes[4], 0, "gated SQL must succeed at the server");
                                        gate.received.notify_one();
                                        gate.release.notified().await;
                                    }
                                    client_write.write_all(&bytes).await?;
                                }
                                #[allow(unreachable_code)]
                                Ok::<(), std::io::Error>(())
                            };
                            tokio::select! {
                                _ = requests => {},
                                _ = responses => {},
                            }
                        });
                    }
                    Some(result) = connections.join_next() => { result.unwrap(); }
                }
            }
        });
        Self {
            port: proxy_port,
            gate,
            task,
        }
    }
}

async fn cancelled_publication(boundary: Boundary) {
    let (shared, settings) = common::setup_pool_and_settings().await;
    let observer = shared.get();
    let user = format!("registry-cancel-{}", uuid::Uuid::new_v4());
    let registry = DatabaseEdgeRegistryService::new(observer.clone());
    let lease = registry
        .register_or_update_with_lease(&user, "cancel-edge", "transport", None, None, None, None)
        .await
        .unwrap();
    assert!(registry.finalize_registration(&lease).await.unwrap());

    let proxy = ResponseProxy::start(settings.host, settings.port, boundary).await;
    let pool = MySqlPoolOptions::new()
        .max_connections(1)
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(
            MySqlConnectOptions::new()
                .host("127.0.0.1")
                .port(proxy.port)
                .username(&settings.user)
                .password(&settings.password)
                .database(&settings.database)
                // The loopback fixture must see MySQL framing; production TLS is
                // unchanged. Never use this fixture for a non-test database.
                .ssl_mode(MySqlSslMode::Disabled),
        )
        .await
        .unwrap();
    let original: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let pending = tokio::spawn({
        let registry = DatabaseEdgeRegistryService::new(pool.clone());
        let lease = lease.clone();
        async move { registry.release_registration(&lease).await }
    });
    let reached =
        tokio::time::timeout(Duration::from_secs(30), proxy.gate.received.notified()).await;
    pending.abort();
    let cancelled = pending.await;
    // Deliver the original server response intact. Ordinary pool.begin()
    // wrongly reuses this checkout: SQLx never constructed its Transaction
    // (or incremented transaction_depth) before the BEGIN await was cancelled.
    proxy.gate.release.notify_one();

    let replacement = sqlx::query_scalar::<_, u64>("SELECT CONNECTION_ID()")
        .fetch_one(&pool)
        .await;
    // Retry from an independent pool before closing the tested pool: closing
    // it first would itself release leaked locks and conceal the regression.
    let retry = tokio::time::timeout(Duration::from_secs(15), async {
        let first = registry.release_registration(&lease).await?;
        let replay = registry.release_registration(&lease).await?;
        registry
            .heartbeat(&user, "cancel-edge", "transport", None)
            .await
            .map_err(|error| format!("heartbeat after retry: {error}"))?;
        let healthy = DatabaseEdgeRegistryService::new(pool.clone());
        let replay_on_replacement = healthy.release_registration(&lease).await?;
        let reused: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
            .fetch_one(&pool)
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((first, replay, replay_on_replacement, reused))
    })
    .await;
    pool.close().await;
    drop(proxy);
    sqlx::query("DELETE FROM edge_agent_registry WHERE user_id = ?")
        .bind(&user)
        .execute(observer)
        .await
        .unwrap();

    reached.expect("the real server must execute the gated statement before cancellation");
    assert!(cancelled.is_err_and(|error| error.is_cancelled()));
    let replacement = replacement.unwrap();
    assert_ne!(
        original, replacement,
        "cancelled transaction must discard its physical connection"
    );
    assert_eq!(
        retry
            .expect("cancelled publication must release its row lock and remain retryable")
            .unwrap(),
        (true, true, true, replacement),
        "publication must remain idempotent and completed commits must reuse the healthy connection"
    );
}

#[tokio::test]
#[ignore = "requires isolated live MatrixOne (ASTRA_TEST_DB_IT=1)"]
async fn publication_cancelled_before_begin_ack_discards_connection() {
    cancelled_publication(Boundary::Begin).await;
}

#[tokio::test]
#[ignore = "requires isolated live MatrixOne (ASTRA_TEST_DB_IT=1)"]
async fn publication_cancelled_after_row_write_releases_lock() {
    cancelled_publication(Boundary::CurrentReadWrite).await;
}
