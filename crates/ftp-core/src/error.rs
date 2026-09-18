use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use thiserror::Error;

/// Unified error type for the whole engine.
///
/// Variants are grouped by origin so callers can distinguish "the peer
/// refused" (protocol/remote errors, usually worth surfacing to the user)
/// from "the local machine failed" (I/O errors like missing files).
#[derive(Debug, Error)]
pub enum Error {
    /// Local I/O failure: file open/read/write, socket bind, etc.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// FTP client error from suppaftp (includes server reply codes).
    #[error("ftp error: {0}")]
    Ftp(#[from] suppaftp::FtpError),

    /// FTP server failed to build or start (bad root dir, invalid config).
    #[error("ftp server error: {0}")]
    FtpServer(String),

    /// A user-supplied setting is unusable (e.g. an invalid passive port
    /// range). The message is written for the person who typed it, so it is
    /// rendered without any wrapping prefix.
    #[error("{0}")]
    Config(String),

    /// The listen address could not be bound: the port is taken, sits inside
    /// a system-reserved range, or the IP is no longer assigned to a NIC.
    ///
    /// The OS error is embedded in `Display` on purpose — `thiserror` does not
    /// walk the `source()` chain, so without this a bind failure would only
    /// ever read as "io error".
    #[error("无法绑定监听地址 {addr}：{source}{hint}")]
    Bind {
        addr: String,
        /// What we believe went wrong, after looking past `io::ErrorKind`.
        cause: BindCause,
        /// Extra operator-facing guidance, already wrapped in parentheses.
        hint: String,
        #[source]
        source: std::io::Error,
    },

    /// The TFTP peer violated the protocol: bad opcode, unexpected packet
    /// type, or a rejected path-traversal attempt.
    #[error("tftp protocol error: {0}")]
    TftpProtocol(String),

    /// The TFTP peer sent a well-formed ERROR packet (codes per RFC 1350 §5,
    /// e.g. 1 = file not found, 2 = access violation).
    #[error("tftp remote error {code}: {msg}")]
    TftpRemote { code: u16, msg: String },

    /// A TFTP transfer exhausted its retransmission budget
    /// (`MAX_RETRIES` attempts at `TIMEOUT` each).
    #[error("timeout waiting for peer")]
    Timeout,
}

/// Why a `bind()` failed, after looking past `io::ErrorKind`.
///
/// `io::ErrorKind` alone is not enough on Windows: a single
/// `PermissionDenied` covers three unrelated situations with three different
/// fixes — a port inside an OS-reserved band, a port another process holds
/// *exclusively* on the wildcard address, and a firewall/WFP policy. Reporting
/// all of them as "端口落在系统保留段" sends the user to `excludedportrange`
/// when the real answer is in `netstat`, so [`Error::bind`] spends one extra
/// `bind()` to tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindCause {
    /// Another process already holds the port.
    Taken,
    /// The OS refuses to give *this* process the port, and nobody else holds
    /// it: a reserved band, a firewall/WFP rule, or (on unix) a privileged
    /// port below 1024.
    Forbidden,
    /// The address is not assigned to any local interface, or its NIC is down.
    AddressUnavailable,
    /// Could not narrow it down; the raw OS error is all we have.
    Unknown,
}

impl BindCause {
    /// Operator-facing guidance, already wrapped in parentheses.
    ///
    /// Deliberately platform-aware: the *same* failure reads as `EACCES` /
    /// `10013` on Windows and on unix, but the correct fix has nothing in
    /// common. Windows has no privileged-port rule below 1024 — measured, an
    /// unprivileged user can bind 80 — while unix does, and there the fix is
    /// capabilities rather than a reserved band.
    pub fn hint(self, addr: &SocketAddr) -> String {
        let port = addr.port();
        match self {
            BindCause::Taken => format!(
                "（端口 {port} 已被其他程序占用：用 netstat -ano | findstr :{port} 查到占用进程后结束它，\
                 或者换一个端口。注意：占用者以独占方式持有 0.0.0.0:{port} 时，Windows 会把「绑定具体地址」\
                 报成访问被拒而不是「已被占用」，所以这里未必出现「地址已在使用」字样）"
            ),
            BindCause::Forbidden if cfg!(windows) => format!(
                "（系统不允许绑定端口 {port}，且它并没有被别的程序占用：常见原因是落在 Windows 保留端口段，\
                 或被杀毒/防火墙的过滤驱动拦截。用 netsh int ipv4 show excludedportrange protocol=tcp 核对；\
                 若确实落在保留段内，换用段外端口，或临时执行 net stop winnat 再 net start winnat 释放）"
            ),
            BindCause::Forbidden if port < 1024 => format!(
                "（端口 {port} 在 1024 以下，属于特权端口：Linux 需要以 root 运行，或给可执行文件执行一次 \
                 setcap 'cap_net_bind_service=+ep'（每次升级程序后都要重设），也可临时 \
                 sysctl -w net.ipv4.ip_unprivileged_port_start={port}；macOS 需要 sudo 运行，\
                 或安装一个 launchd 特权 helper。也可以直接改用 1024 以上的端口）"
            ),
            BindCause::Forbidden => format!(
                "（系统策略拒绝了绑定端口 {port}：Linux 上多为 SELinux/AppArmor 规则，\
                 或该端口被 socket activation 之类的机制代管；请改用其它端口）"
            ),
            BindCause::AddressUnavailable => {
                "（该 IP 不属于本机、或网卡已断开，请在「监听接口」里重新选择）".to_string()
            }
            BindCause::Unknown => String::new(),
        }
    }
}

/// Which cause a bind failure maps to, given an optional wildcard probe.
///
/// Split out from [`Error::bind`] so the mapping is a pure function over two
/// errors and can be tested without a live socket.
pub fn classify_bind(
    source: &std::io::Error,
    wildcard_probe: Option<&std::io::Error>,
) -> BindCause {
    use std::io::ErrorKind;
    match source.kind() {
        ErrorKind::AddrNotAvailable => BindCause::AddressUnavailable,
        ErrorKind::AddrInUse => BindCause::Taken,
        ErrorKind::PermissionDenied => match wildcard_probe.map(|p| p.kind()) {
            // Someone else holds the wildcard address, so the port really is
            // taken — Windows just reports the clash as WSAEACCES (10013)
            // rather than WSAEADDRINUSE (10048) when the holder asked for
            // exclusive access.
            Some(ErrorKind::AddrInUse) => BindCause::Taken,
            // The probe succeeded or failed for the same "forbidden" reason:
            // nobody owns the port, the OS simply will not hand it over.
            _ => BindCause::Forbidden,
        },
        _ => BindCause::Unknown,
    }
}

/// Can *we* bind the same port on the wildcard address of the same family?
///
/// Returns the error, or `None` when the bind succeeded. Only ever called when
/// the original failure was `PermissionDenied` on Windows, where the two causes
/// genuinely collide; on unix `EACCES` below 1024 is unambiguous already.
///
/// The listener is dropped immediately, so nothing is held. Rust's
/// `TcpListener::bind` does not set `SO_REUSEADDR` on Windows, which is what
/// makes this probe meaningful there.
fn probe_wildcard(addr: SocketAddr) -> Option<std::io::Error> {
    let port = addr.port();
    let wildcard = match addr {
        SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
        SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
    };
    match std::net::TcpListener::bind(wildcard) {
        Ok(listener) => {
            drop(listener);
            None
        }
        Err(e) => Some(e),
    }
}

impl Error {
    /// Build a [`Error::Bind`], translating the common OS errors into hints
    /// the user can act on.
    pub fn bind(addr: SocketAddr, source: std::io::Error) -> Self {
        let probe = if cfg!(windows) && source.kind() == std::io::ErrorKind::PermissionDenied {
            probe_wildcard(addr)
        } else {
            None
        };
        let cause = classify_bind(&source, probe.as_ref());
        Error::Bind {
            addr: addr.to_string(),
            cause,
            hint: cause.hint(&addr),
            source,
        }
    }

    /// The classified cause, when this is a bind failure.
    pub fn bind_cause(&self) -> Option<BindCause> {
        match self {
            Error::Bind { cause, .. } => Some(*cause),
            _ => None,
        }
    }
}

/// Flatten an error and its `source()` chain into a single line.
///
/// `thiserror`'s derived `Display` prints only the outermost message, which is
/// how libunftp's `ServerError` ends up hiding its cause: it renders as
/// "server error: io error" and swallows the underlying `Os { code: 10048,
/// kind: AddrInUse, ... }`.
pub fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        out.push_str(" → ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn maps_the_windows_bind_error_codes() {
        // 10048 WSAEADDRINUSE / 10049 WSAEADDRNOTAVAIL are stable across
        // platforms' decoders only on Windows, so assert the kinds we rely on
        // where the mapping is defined.
        #[cfg(windows)]
        {
            use std::io::ErrorKind;
            assert_eq!(
                std::io::Error::from_raw_os_error(10013).kind(),
                ErrorKind::PermissionDenied,
                "WSAEACCES must decode as PermissionDenied"
            );
            assert_eq!(
                std::io::Error::from_raw_os_error(10048).kind(),
                ErrorKind::AddrInUse
            );
            assert_eq!(
                std::io::Error::from_raw_os_error(10049).kind(),
                ErrorKind::AddrNotAvailable
            );
        }

        assert_eq!(
            classify_bind(&std::io::Error::from_raw_os_error(10048), None),
            BindCause::Taken
        );
        assert_eq!(
            classify_bind(&std::io::Error::from_raw_os_error(10049), None),
            BindCause::AddressUnavailable
        );
    }

    #[test]
    fn a_failed_wildcard_probe_means_the_port_is_actually_taken() {
        // This is the case that used to be misreported as "端口落在系统保留段":
        // FileZilla-style exclusive holder on 0.0.0.0:21, our bind to
        // 127.0.0.1:21 gets EACCES, and probing the wildcard gets EADDRINUSE.
        let original = std::io::Error::from_raw_os_error(10013);
        let probe = std::io::Error::from_raw_os_error(10048);
        assert_eq!(classify_bind(&original, Some(&probe)), BindCause::Taken);
    }

    #[test]
    fn a_forbidden_port_stays_forbidden_whatever_the_probe_says() {
        let original = std::io::Error::from_raw_os_error(10013);

        // probe refused for the same reason -> it is the OS saying no
        let forbidden = std::io::Error::from_raw_os_error(10013);
        assert_eq!(
            classify_bind(&original, Some(&forbidden)),
            BindCause::Forbidden
        );

        // probe succeeded -> the port is free, so "forbidden" is still the
        // honest answer (WFP/ACL scoped to this address)
        assert_eq!(classify_bind(&original, None), BindCause::Forbidden);
    }

    #[test]
    fn hints_name_the_port_and_are_platform_appropriate() {
        let socket = addr(21);
        let taken = BindCause::Taken.hint(&socket);
        assert!(taken.contains("21"), "{taken}");
        assert!(taken.contains("netstat"), "{taken}");

        let forbidden = BindCause::Forbidden.hint(&socket);
        assert!(forbidden.contains("21"), "{forbidden}");
        if cfg!(windows) {
            // Windows has no privileged-port rule; the hint must not tell the
            // user to look for one.
            assert!(
                forbidden.contains("excludedportrange") || forbidden.contains("保留端口段"),
                "{forbidden}"
            );
            assert!(!forbidden.contains("root"), "Windows 不涉及 root：{forbidden}");
        } else {
            // 21 is privileged off-Windows, and the fix there is capabilities.
            assert!(forbidden.contains("root"), "{forbidden}");
            assert!(forbidden.contains("setcap"), "{forbidden}");
        }

        // A high port on unix is not a privilege problem, so it must not
        // mention root at all.
        if !cfg!(windows) {
            let high = BindCause::Forbidden.hint(&addr(50000));
            assert!(!high.contains("root"), "{high}");
        }

        // Unknown carries no guidance rather than a wrong one.
        assert!(BindCause::Unknown.hint(&socket).is_empty());
    }

    #[test]
    fn bind_errors_expose_their_cause() {
        let err = Error::bind(addr(2121), std::io::Error::from_raw_os_error(10048));
        assert_eq!(err.bind_cause(), Some(BindCause::Taken));
        // The raw OS error must survive into Display, not just the hint.
        let rendered = err.to_string();
        assert!(rendered.contains("2121"), "{rendered}");
        assert!(rendered.contains("os error"), "{rendered}");
    }
}
