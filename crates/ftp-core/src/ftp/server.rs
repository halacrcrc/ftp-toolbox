use std::collections::HashMap;
use std::net::SocketAddr;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use libunftp::auth::{AuthenticationError, Authenticator, Credentials, DefaultUser};
use libunftp::Server;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use unftp_sbe_fs::{Filesystem, ServerExt};

use crate::error::{error_chain, Error, Result};
use crate::ftp::passive::{self, DEFAULT_PASSIVE_PORTS};
use crate::lifecycle::{ServerShared, ServerState, SessionGuard};

const GREETING: &str = "Welcome to ftp-toolbox";

/// Server knobs that the GUI exposes. `Default` reproduces the historical
/// behaviour, so callers that do not care keep working unchanged.
#[derive(Debug, Clone)]
pub struct FtpServerOptions {
    /// Port range used for PASV/EPSV data connections (half-open, as
    /// libunftp takes it: `50000..50100` means 50000-50099).
    ///
    /// See [`crate::ftp::passive`] for why this is configurable: on Windows the
    /// default band can overlap a Hyper-V/WSL2 reserved block, which makes PASV
    /// fail intermittently with nothing useful in the log.
    pub passive_ports: Range<u16>,
}

impl Default for FtpServerOptions {
    fn default() -> Self {
        Self { passive_ports: DEFAULT_PASSIVE_PORTS }
    }
}

impl FtpServerOptions {
    /// Reject ranges that cannot work, in words the UI can show as-is.
    pub fn validate(&self) -> Result<()> {
        passive::validate(&self.passive_ports)
    }

    /// Range as humans write it: `50000..50100` -> "50000-50099".
    pub fn passive_ports_label(&self) -> String {
        passive::label(&self.passive_ports)
    }
}

/// Authentication mode for the FTP server.
pub enum FtpAuth {
    /// Anyone can log in (any username/password, including none).
    Anonymous,
    /// Static username -> password map.
    Users(HashMap<String, String>),
}

impl FtpAuth {
    /// Convenience constructor for a single account.
    pub fn single_user(user: &str, pass: &str) -> Self {
        FtpAuth::Users(HashMap::from([(user.to_string(), pass.to_string())]))
    }
}

/// Simple in-memory authenticator for libunftp.
#[derive(Debug)]
struct StaticAuth {
    users: HashMap<String, String>,
}

#[async_trait::async_trait]
impl Authenticator<DefaultUser> for StaticAuth {
    async fn authenticate(
        &self,
        username: &str,
        creds: &Credentials,
    ) -> std::result::Result<DefaultUser, AuthenticationError> {
        match self.users.get(username) {
            None => Err(AuthenticationError::BadUser),
            Some(expected) => match creds.password.as_deref() {
                Some(p) if p == expected => Ok(DefaultUser),
                _ => Err(AuthenticationError::BadPassword),
            },
        }
    }
}

/// Everything needed to build a libunftp [`Server`] for one accepted
/// connection. `Server::service` consumes the instance, so a fresh one is
/// built per connection — all fields are cheap Arc/PathBuf clones.
struct ServerConfig {
    root: PathBuf,
    /// `None` means anonymous access.
    auth: Option<Arc<StaticAuth>>,
    passive_ports: Range<u16>,
}

impl ServerConfig {
    fn build(&self) -> std::result::Result<Server<Filesystem, DefaultUser>, Error> {
        let builder = Server::with_fs(self.root.clone())
            .greeting(GREETING)
            .passive_ports(self.passive_ports.clone());
        let builder = match &self.auth {
            Some(auth) => builder
                .authenticator(Arc::clone(auth) as Arc<dyn Authenticator<DefaultUser> + Send + Sync>),
            None => builder,
        };
        builder.build().map_err(|e| Error::FtpServer(error_chain(&e)))
    }
}

/// Handle to a running FTP server.
///
/// Dropping the handle — or calling [`FtpServerHandle::stop`] — asks the
/// accept loop *and* every active session to exit. Unlike the previous
/// `AbortHandle`-based version, stopping is observable: [`Self::is_running`]
/// reports the real state and `stop()` waits for the loop to finish.
#[derive(Debug)]
pub struct FtpServerHandle {
    shared: Arc<ServerShared>,
    /// Shutdown signal. Sending on it (or dropping it) makes the accept loop
    /// and all session tasks — which hold `broadcast::Receiver`s — exit.
    stop_tx: Option<broadcast::Sender<()>>,
    join: JoinHandle<()>,
    /// Listen address as configured by the caller (e.g. "0.0.0.0:21").
    pub addr: String,
    /// Address actually bound, e.g. "0.0.0.0:21" or "127.0.0.1:52814".
    pub local_addr: String,
    pub root: PathBuf,
    /// Passive port range in effect (half-open).
    pub passive_ports: Range<u16>,
}

impl FtpServerHandle {
    /// True while the accept loop is alive.
    pub fn is_running(&self) -> bool {
        self.shared.is_running()
    }

    /// Live control connections right now.
    pub fn sessions(&self) -> usize {
        self.shared.sessions()
    }

    /// Running flag + session count in one snapshot.
    pub fn state(&self) -> ServerState {
        self.shared.state()
    }

    /// Subscribe to run-state changes (seeded with the current value).
    pub fn subscribe(&self) -> watch::Receiver<ServerState> {
        self.shared.subscribe()
    }

    /// Ask the server to stop without waiting for it.
    pub fn signal_stop(&mut self) {
        self.shared.request_stop();
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
            // `tx` is dropped here as well, so even receivers that subscribed
            // after the send observe a closed channel and exit.
        }
    }

    /// Stop the server and wait until the accept loop has finished.
    pub async fn stop(mut self) {
        self.signal_stop();
        let _ = (&mut self.join).await;
    }
}

impl Drop for FtpServerHandle {
    fn drop(&mut self) {
        self.signal_stop();
    }
}

/// Start an FTP server serving `root` on `bind` (e.g. "0.0.0.0:2121") with the
/// default [`FtpServerOptions`].
///
/// The socket is bound *before* the accept loop is spawned, so failures such
/// as "port already in use" come back as a readable [`Error::Bind`] instead of
/// being buried in a background task. Note: the canonical port 21 is
/// privileged on Linux/macOS.
pub async fn start_server(root: PathBuf, bind: String, auth: FtpAuth) -> Result<FtpServerHandle> {
    start_server_with(root, bind, auth, FtpServerOptions::default()).await
}

/// [`start_server`] with explicit options (passive port range).
pub async fn start_server_with(
    root: PathBuf,
    bind: String,
    auth: FtpAuth,
    options: FtpServerOptions,
) -> Result<FtpServerHandle> {
    options.validate()?;

    if !root.is_dir() {
        return Err(Error::FtpServer(format!(
            "共享目录不存在或不是目录: {}",
            root.display()
        )));
    }

    let addr: SocketAddr = bind
        .parse()
        .map_err(|e| Error::FtpServer(format!("监听地址无法解析「{bind}」: {e}")))?;

    let listener = TcpListener::bind(addr).await.map_err(|e| Error::bind(addr, e))?;
    let local_addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| bind.clone());

    let cfg = Arc::new(ServerConfig {
        root: root.clone(),
        auth: match auth {
            FtpAuth::Anonymous => None,
            FtpAuth::Users(users) => Some(Arc::new(StaticAuth { users })),
        },
        passive_ports: options.passive_ports.clone(),
    });

    let shared = ServerShared::new("ftp");
    let (stop_tx, _) = broadcast::channel::<()>(1);
    let passive_label = options.passive_ports_label();

    let join = tokio::spawn({
        let shared = Arc::clone(&shared);
        let cfg = Arc::clone(&cfg);
        // Kept alive for the whole loop so every accepted connection can
        // subscribe its own receiver.
        let session_tx = stop_tx.clone();
        let mut stop_rx = stop_tx.subscribe();
        let listen_for_log = local_addr.clone();
        async move {
            // Clears `running` and notifies subscribers on *any* exit path,
            // including a panic in the body below.
            let _loop_guard = shared.loop_guard();
            info!(listen = %listen_for_log, passive_ports = %passive_label, "ftp server listening");
            loop {
                tokio::select! {
                    _ = stop_rx.recv() => {
                        info!("ftp server stopped");
                        break;
                    }
                    accepted = listener.accept() => match accepted {
                        Ok((stream, peer)) => {
                            info!(%peer, "incoming ftp control connection");
                            spawn_session(
                                Arc::clone(&cfg),
                                shared.session(),
                                session_tx.subscribe(),
                                stream,
                                peer,
                            );
                        }
                        Err(e) => {
                            // A failed accept (e.g. out of file descriptors)
                            // must not kill the server.
                            error!("ftp accept failed: {}", error_chain(&e));
                        }
                    },
                }
            }
        }
    });

    Ok(FtpServerHandle {
        shared,
        stop_tx: Some(stop_tx),
        join,
        addr: bind,
        local_addr,
        root,
        passive_ports: options.passive_ports,
    })
}

/// Serve one accepted control connection with libunftp, closing it early if
/// the server is asked to shut down.
///
/// `session` is the live-session guard: it is moved into the task and dropped
/// when the connection ends, which is what keeps the UI's connection count
/// honest.
fn spawn_session(
    cfg: Arc<ServerConfig>,
    session: SessionGuard,
    mut stop_rx: broadcast::Receiver<()>,
    stream: TcpStream,
    peer: SocketAddr,
) {
    tokio::spawn(async move {
        let _session = session;
        let server = match cfg.build() {
            Ok(server) => server,
            Err(e) => {
                error!(%peer, "could not build ftp session handler: {}", error_chain(&e));
                return;
            }
        };
        tokio::select! {
            result = server.service(stream) => match result {
                Ok(()) => debug!(%peer, "ftp session closed"),
                Err(e) => warn!(%peer, "ftp session ended with error: {}", error_chain(&e)),
            },
            _ = stop_rx.recv() => {
                info!(%peer, "closing ftp session because the server is stopping");
            }
        }
    });
}
