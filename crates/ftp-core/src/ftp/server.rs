use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use libunftp::auth::{AuthenticationError, Authenticator, Credentials, DefaultUser};
use libunftp::Server;
use tokio::task::AbortHandle;
use unftp_sbe_fs::ServerExt;

use crate::error::{Error, Result};

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

/// Handle to a running FTP server task. Drop/abort to stop it.
pub struct FtpServerHandle {
    abort: AbortHandle,
    pub addr: String,
    pub root: PathBuf,
}

impl FtpServerHandle {
    pub fn stop(&self) {
        self.abort.abort();
    }
}

impl Drop for FtpServerHandle {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

/// Start an FTP server serving `root` on `bind` (e.g. "0.0.0.0:2121").
/// Note: the canonical port 21 is privileged on Linux/macOS.
pub async fn start_server(root: PathBuf, bind: String, auth: FtpAuth) -> Result<FtpServerHandle> {
    if !root.is_dir() {
        return Err(Error::FtpServer(format!(
            "root directory does not exist: {}",
            root.display()
        )));
    }

    let builder = Server::with_fs(root.clone())
        .greeting("Welcome to ftp-toolbox")
        .passive_ports(50000..50100);

    let server = match auth {
        FtpAuth::Anonymous => builder.build().map_err(|e| Error::FtpServer(e.to_string()))?,
        FtpAuth::Users(users) => builder
            .authenticator(Arc::new(StaticAuth { users }))
            .build()
            .map_err(|e| Error::FtpServer(e.to_string()))?,
    };

    let addr_for_log = bind.clone();
    let join = tokio::spawn(async move {
        if let Err(e) = server.listen(&addr_for_log).await {
            tracing::error!("ftp server stopped with error: {e}");
        }
    });

    Ok(FtpServerHandle { abort: join.abort_handle(), addr: bind, root })
}
