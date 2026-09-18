//! Bind-failure attribution.
//!
//! The bug these guard: every `PermissionDenied` bind failure used to be
//! reported as "端口落在系统保留段", which sends the user to
//! `netsh … excludedportrange` even when another program simply holds the port.
//! Diagnosed on a machine where FileZilla Server owned `0.0.0.0:21`
//! *exclusively*, so binding `127.0.0.1:21` was refused with WSAEACCES (10013)
//! rather than WSAEADDRINUSE (10048) — and the old hint sent the user looking
//! in the wrong place entirely.
//!
//! Coverage note: the two causes are tested at different levels on purpose.
//! "Port taken" and "address not local" are reproduced end-to-end here.
//! "Exclusively held wildcard" needs a holder that sets `SO_EXCLUSIVEADDRUSE`
//! (i.e. socket2/winsock FFI or an external program), so its *mapping* is
//! covered by `error::tests::a_failed_wildcard_probe_means_the_port_is_actually_taken`
//! and the probe behaviour itself was established by hand on the machine above.

use std::net::TcpListener;

use ftp_core::error::BindCause;
use ftp_core::ftp::{start_server, FtpAuth, FtpServerHandle};

fn root_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ftp-core-bind-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn start_on(addr: &str, tag: &str) -> Result<FtpServerHandle, ftp_core::Error> {
    start_server(root_dir(tag), addr.to_string(), FtpAuth::Anonymous).await
}

/// A port somebody else already listens on is reported as *taken*: the fix is
/// "查出占用进程并关掉它，或换端口", not "去看保留端口段".
///
/// Goes through the real `start_server` path, not `Error::bind` directly, so it
/// covers the same call the GUI makes.
#[tokio::test]
async fn a_port_held_by_another_listener_is_reported_as_taken() {
    // Hold an ephemeral port for the duration of the test. A plain holder is
    // enough here: two listeners cannot share the exact same wildcard address.
    let holder = TcpListener::bind(("0.0.0.0", 0)).expect("bind an ephemeral port");
    let port = holder.local_addr().unwrap().port();

    let err = match start_on(&format!("0.0.0.0:{port}"), "taken").await {
        Ok(handle) => {
            handle.stop().await;
            panic!("端口 {port} 已被占用，绑定必须失败");
        }
        Err(e) => e,
    };

    assert_eq!(err.bind_cause(), Some(BindCause::Taken), "{err}");
    let msg = err.to_string();
    assert!(msg.contains(&port.to_string()), "文案要带上端口：{msg}");
    assert!(msg.contains("netstat"), "文案要指向占用排查：{msg}");
    drop(holder);
}

/// An address that is not on this machine lands in its own bucket, so the UI
/// can point at the interface picker instead of at the port.
#[test]
fn an_address_that_is_not_local_is_reported_as_unavailable() {
    // 192.0.2.0/24 is TEST-NET-1: reserved for documentation, never assigned.
    let addr = "192.0.2.123:0".parse().expect("literal address");
    let err = match TcpListener::bind(addr) {
        Ok(listener) => {
            drop(listener);
            panic!("192.0.2.123 不应该在本机可绑");
        }
        Err(e) => ftp_core::Error::bind(addr, e),
    };

    assert_eq!(
        err.bind_cause(),
        Some(BindCause::AddressUnavailable),
        "{err}"
    );
    assert!(err.to_string().contains("监听接口"), "{}", err);
}

/// The other half of the diagnosis: a port inside a **plain** OS exclusion is
/// genuinely out of reach, and must be reported as such — including the
/// platform-correct advice, which on Windows is *not* "以管理员运行".
///
/// Uses the real `netsh` table rather than a hard-coded port, so it works on
/// any machine that has a plain exclusion and skips (loudly) where there is
/// none. Managed exclusions (`netsh`'s `*`) are deliberately not used: measured,
/// they let a bind to a specific address through.
#[cfg(windows)]
#[tokio::test]
async fn a_plain_reserved_band_is_reported_as_forbidden() {
    use std::os::windows::process::CommandExt;
    /// Keep the test from flashing a console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let output = std::process::Command::new("netsh")
        .args(["int", "ipv4", "show", "excludedportrange", "protocol=tcp"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("netsh 应当可用");
    let bands = ftp_core::ftp::passive::parse_excluded_ranges(&String::from_utf8_lossy(
        &output.stdout,
    ));

    let Some(plain) = bands.iter().find(|b| !b.managed && b.start >= 1024) else {
        println!("本机没有可用的普通排除段，跳过该用例（managed 段不阻止绑定，无法用于验证）");
        return;
    };

    let addr = format!("127.0.0.1:{}", plain.start);
    let err = match start_on(&addr, "reserved").await {
        Ok(handle) => {
            handle.stop().await;
            panic!("{addr} 落在普通排除段 {} 里，绑定不该成功", plain.label());
        }
        Err(e) => e,
    };

    println!("普通排除段 {} -> {:?}", plain.label(), err.bind_cause());
    println!("渲染给用户的文案: {err}");
    assert_eq!(
        err.bind_cause(),
        Some(BindCause::Forbidden),
        "被系统禁止的端口不能归到别的成因：{err}"
    );

    // Platform-specific advice matters here: the same EACCES on unix means
    // "privileged port", and telling a Windows user to look for one is wrong.
    let msg = err.to_string();
    assert!(msg.contains("excludedportrange"), "{msg}");
    assert!(!msg.contains("需要 root"), "Windows 不涉及 root：{msg}");
}
