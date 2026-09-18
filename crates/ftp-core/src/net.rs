//! Deciding whether a network interface is worth offering as a listen address.
//!
//! Only the *decisions* live here — pure string/IP logic with no OS calls, so
//! they are cheap to unit-test. Reading the OS (`/sys/class/net/…`,
//! `GetAdaptersAddresses`, `netsh`, `ipconfig`) stays in the caller.
//!
//! Why this exists: `get_if_addrs` reports an interface's addresses regardless
//! of link state — its POSIX path only looks at `IFF_BROADCAST` and never at
//! `IFF_UP`/`IFF_RUNNING`, and its Windows path never reads `OperStatus`. So a
//! machine that just had its cable pulled still lists that adapter, and picking
//! it makes the server fail to bind (`WSAEADDRNOTAVAIL` / `EADDRNOTAVAIL`).
//! Enumeration therefore has to ask "is this link actually up?" separately.
//!
//! Cross-platform caveat, because the signals genuinely differ:
//!
//! - **Windows**: unplugging makes NDIS report `Media disconnected` and the
//!   DHCP lease is released, so `OperStatus` flips to `IfOperStatusDown`
//!   quickly — re-enumerating is enough.
//! - **Linux**: an unplugged NIC keeps its address for seconds to minutes
//!   depending on NetworkManager/`dhclient`, and Wi-Fi that is not associated
//!   can still report `IFF_RUNNING`. `operstate` plus `carrier` is the closest
//!   equivalent, which is why both are consulted here.

use std::net::Ipv4Addr;

/// `/sys/class/net/<if>/operstate` values that mean "usable".
///
/// `unknown` is deliberately treated as up: that is what loopback and several
/// virtual drivers report, and hiding those would remove usable interfaces.
pub fn operstate_is_up(operstate: &str) -> bool {
    matches!(operstate.trim(), "up" | "unknown")
}

/// `/sys/class/net/<if>/carrier` → `"1"` means a cable is physically present.
pub fn carrier_is_up(carrier: &str) -> bool {
    carrier.trim() == "1"
}

/// Combine the two sysfs signals into one verdict.
///
/// Either file may be missing — older kernels, virtual and tunnel interfaces
/// often have no `carrier` — in which case the caller passes an empty string.
/// A missing signal never *hides* an interface: dropping a NIC the user can
/// actually bind is worse than showing one that is down, because the failure is
/// silent while the downed one is at least explained by the UI.
pub fn link_is_up(operstate: &str, carrier: &str) -> bool {
    let has_operstate = !operstate.trim().is_empty();
    let has_carrier = !carrier.trim().is_empty();
    if !has_operstate && !has_carrier {
        return true; // no signal at all: keep it
    }
    if has_operstate && !operstate_is_up(operstate) {
        return false;
    }
    if has_carrier && !carrier_is_up(carrier) {
        return false;
    }
    true
}

/// Would we offer this address in the "监听接口" picker?
///
/// Loopback is always offered (handy for self-tests) even though it has no
/// link; `169.254.x.x` never is, because binding it produces an address nothing
/// can reach.
pub fn is_usable_ipv4(ip: Ipv4Addr, link_up: bool) -> bool {
    if ip.is_loopback() {
        return true;
    }
    if ip.is_link_local() {
        return false;
    }
    link_up
}

/// Stable ordering key for a listen-interface entry: IPv4 value, then name.
///
/// Enumeration feeds a dropdown that is re-read every few seconds, and the
/// frontend decides whether anything changed by comparing consecutive
/// snapshots. Without a total order the same set of adapters could come back in
/// a different sequence, and then every poll would look like a change.
///
/// Addresses compare numerically, not as strings, so `10.0.0.9` sorts before
/// `10.0.0.10` instead of after it. A name is the tie-breaker because one
/// adapter can legitimately hold several addresses.
///
/// An unparseable address sorts with `0.0.0.0` rather than panicking — the
/// caller only ever builds these strings from `Ipv4Addr`, but a sort key is the
/// wrong place to find that out.
pub fn interface_sort_key(ip: &str, name: &str) -> (Ipv4Addr, String) {
    (
        ip.parse::<Ipv4Addr>().unwrap_or(Ipv4Addr::UNSPECIFIED),
        name.to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operstate_follows_the_kernel_vocabulary() {
        assert!(operstate_is_up("up"));
        assert!(operstate_is_up("up\n"), "sysfs files end with a newline");
        assert!(operstate_is_up("unknown"), "loopback and virtual drivers");
        assert!(!operstate_is_up("down"));
        assert!(!operstate_is_up("dormant"), "Wi-Fi not associated");
        assert!(!operstate_is_up("lowerlayerdown"));
        assert!(!operstate_is_up(""));
    }

    #[test]
    fn carrier_is_the_physical_cable_signal() {
        assert!(carrier_is_up("1"));
        assert!(carrier_is_up("1\n"));
        assert!(!carrier_is_up("0"));
        assert!(!carrier_is_up(""));
    }

    #[test]
    fn a_missing_signal_never_hides_an_interface() {
        // Both unreadable: keep it rather than guess.
        assert!(link_is_up("", ""));
        // Only carrier readable and up.
        assert!(link_is_up("", "1"));
        // Only operstate readable and up.
        assert!(link_is_up("up", ""));
    }

    #[test]
    fn a_down_link_is_a_down_link() {
        assert!(!link_is_up("down", ""));
        assert!(!link_is_up("up", "0"), "operstate can lag behind the cable");
        assert!(!link_is_up("dormant", "1"));
        assert!(link_is_up("up", "1"));
    }

    #[test]
    fn unusable_addresses_are_excluded_regardless_of_link() {
        let link_local = Ipv4Addr::new(169, 254, 10, 20);
        assert!(!is_usable_ipv4(link_local, true));

        let loopback = Ipv4Addr::new(127, 0, 0, 1);
        assert!(is_usable_ipv4(loopback, false), "回环永远可选");

        let lan = Ipv4Addr::new(192, 168, 1, 5);
        assert!(is_usable_ipv4(lan, true));
        assert!(!is_usable_ipv4(lan, false), "拔了网线的地址不该出现在下拉里");
    }

    #[test]
    fn addresses_sort_numerically_not_as_text() {
        let mut keys = vec![
            interface_sort_key("10.0.0.10", "b"),
            interface_sort_key("10.0.0.9", "a"),
            interface_sort_key("192.168.1.2", "c"),
        ];
        keys.sort();
        // 字符串比较会把 10.0.0.10 排到 10.0.0.9 前面，数值比较不会
        assert_eq!(
            keys,
            vec![
                interface_sort_key("10.0.0.9", "a"),
                interface_sort_key("10.0.0.10", "b"),
                interface_sort_key("192.168.1.2", "c"),
            ]
        );
    }

    #[test]
    fn the_name_breaks_ties_between_addresses_of_one_adapter() {
        assert!(interface_sort_key("192.168.1.5", "a") < interface_sort_key("192.168.1.5", "b"));
        assert_eq!(
            interface_sort_key("192.168.1.5", "eth0"),
            interface_sort_key("192.168.1.5", "eth0"),
            "同一个接口的快照必须可重复比较，否则轮询会一直误报变化"
        );
    }

    #[test]
    fn loopback_sorts_first_and_junk_does_not_panic() {
        let mut keys = vec![
            interface_sort_key("192.168.1.5", "eth0"),
            interface_sort_key("127.0.0.1", "本机回环"),
        ];
        keys.sort();
        assert_eq!(keys[0].0, Ipv4Addr::LOCALHOST);

        // 不该 panic，且与 0.0.0.0 同组
        assert_eq!(
            interface_sort_key("not-an-ip", "x"),
            interface_sort_key("0.0.0.0", "x")
        );
    }
}
