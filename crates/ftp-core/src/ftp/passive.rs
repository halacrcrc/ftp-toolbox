//! Passive-mode (PASV/EPSV) data port helpers.
//!
//! libunftp picks a random port out of a configured range and retries a few
//! times, so a band that *partially* overlaps an OS-reserved range fails
//! roughly proportionally to the overlap. Nothing about that failure is visible
//! from the client beyond "列表偶尔失败", so the range is configurable and the
//! OS's reserved bands are checked before starting.
//!
//! Not all reserved bands are equal, which is the part that used to be wrong
//! here: see [`ReservedBand::managed`].
//!
//! Everything in this module is pure string/number logic (no OS calls) so it is
//! cheap to test; the actual `netsh` invocation lives in the GUI shell.

use std::ops::Range;

use crate::error::{Error, Result};

/// Passive-mode port range used unless the caller asks for something else.
pub const DEFAULT_PASSIVE_PORTS: Range<u16> = 50000..50100;

/// Lowest port that is not in the privileged range on every platform.
pub const MIN_PASSIVE_PORT: u16 = 1024;

/// Range as humans write it: `50000..50100` -> `"50000-50099"`.
pub fn label(ports: &Range<u16>) -> String {
    let end = ports.end.saturating_sub(1);
    format!("{}-{}", ports.start, end)
}

/// Parse the spec a UI or CLI would accept, returning libunftp's half-open form.
///
/// Accepts `"50000-50099"` (inclusive, how the range is displayed), `"50000..50100"`
/// (half-open) and a single port. An empty string yields [`DEFAULT_PASSIVE_PORTS`].
pub fn parse(spec: &str) -> Result<Range<u16>> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(DEFAULT_PASSIVE_PORTS);
    }

    if let Some((a, b)) = spec.split_once("..") {
        return finish(port(a, spec)?, port(b, spec)?, spec);
    }
    if let Some((a, b)) = spec.split_once('-') {
        let start = port(a, spec)?;
        let end = port(b, spec)?;
        // `50000-50099` is inclusive as written, so the exclusive upper bound
        // is end + 1 — which is why 65535 cannot be expressed this way.
        let exclusive = end.checked_add(1).ok_or_else(|| {
            Error::Config(format!("被动端口段「{spec}」的结束端口不能是 65535"))
        })?;
        return finish(start, exclusive, spec);
    }
    let single = port(spec, spec)?;
    finish(single, single.saturating_add(1), spec)
}

fn port(text: &str, whole: &str) -> Result<u16> {
    text.trim()
        .parse::<u16>()
        .map_err(|_| Error::Config(format!("被动端口段「{whole}」里有非法端口：{}", text.trim())))
}

fn finish(start: u16, end: u16, spec: &str) -> Result<Range<u16>> {
    if start >= end {
        return Err(Error::Config(format!(
            "被动端口段「{spec}」无效：结束端口必须大于起始端口"
        )));
    }
    Ok(start..end)
}

/// Validate a range before handing it to libunftp.
///
/// libunftp accepts a bad range happily and then fails on every PASV command,
/// which is a miserable thing to debug from the client side.
pub fn validate(ports: &Range<u16>) -> Result<()> {
    if ports.start >= ports.end {
        return Err(Error::Config(format!(
            "被动端口范围无效：{}（结束端口必须大于起始端口）",
            label(ports)
        )));
    }
    if ports.start < MIN_PASSIVE_PORT {
        return Err(Error::Config(format!(
            "被动端口范围不能低于 {MIN_PASSIVE_PORT}：{}（低端口是系统保留的）",
            label(ports)
        )));
    }
    Ok(())
}

/// A band the OS keeps to itself, as listed by
/// `netsh int ipv4 show excludedportrange protocol=tcp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedBand {
    pub start: u16,
    pub end: u16,
    /// netsh marks these rows with `*` — the "managed port exclusion" bands
    /// Hyper-V / WSL2 / `winnat` ask for.
    ///
    /// They are *not* interchangeable with a plain exclusion. Measured on
    /// Windows 11: a plain exclusion refuses our `bind` (WSAEACCES, 10013),
    /// while a managed one lets a bind to a **specific address** through. That
    /// distinction decides whether the warning is real: libunftp's PASV
    /// listener binds the control connection's own local address
    /// (`pasv.rs` → `bind(args.local_addr.ip(), …)`), not the wildcard, so a
    /// managed overlap usually costs nothing — while the old check treated it
    /// as a hard conflict and offered a fix for a problem that was not there.
    ///
    /// The behaviour is version- and `winnat`-state-dependent, so a managed
    /// overlap is reported as "probably fine", never as "safe".
    pub managed: bool,
}

impl ReservedBand {
    /// Display label, e.g. `"50000-50059"`.
    pub fn label(&self) -> String {
        format!("{}-{}", self.start, self.end)
    }
}

/// Pull the reserved bands out of netsh's table.
///
/// Headers are localized ("开始端口 结束端口" / "Start Port End Port"), so the
/// only thing relied upon is that a data row begins with two numbers. The
/// trailing `*` marker — and the legend line explaining it, which has no
/// numbers and is therefore skipped — decides `managed`.
pub fn parse_excluded_ranges(text: &str) -> Vec<ReservedBand> {
    let mut bands = Vec::new();
    for line in text.lines() {
        let mut numbers = line.split_whitespace().filter_map(|t| t.parse::<u16>().ok());
        if let (Some(start), Some(end)) = (numbers.next(), numbers.next()) {
            if start <= end {
                bands.push(ReservedBand {
                    start,
                    end,
                    managed: line.contains('*'),
                });
            }
        }
    }
    bands.sort_unstable_by_key(|b| (b.start, b.end));
    bands
}

/// Reserved bands overlapping the inclusive interval `[lo, hi]`.
pub fn overlapping(bands: &[ReservedBand], lo: u16, hi: u16) -> Vec<ReservedBand> {
    bands
        .iter()
        .copied()
        .filter(|b| lo <= b.end && b.start <= hi)
        .collect()
}

/// Plain exclusions inside a half-open passive range: these genuinely break PASV.
pub fn conflicts(ports: &Range<u16>, bands: &[ReservedBand]) -> Vec<String> {
    if ports.start >= ports.end {
        return Vec::new();
    }
    overlapping(bands, ports.start, ports.end - 1)
        .iter()
        .filter(|b| !b.managed)
        .map(ReservedBand::label)
        .collect()
}

/// Managed exclusions inside a half-open passive range: worth a note, not an alarm.
pub fn managed_overlaps(ports: &Range<u16>, bands: &[ReservedBand]) -> Vec<String> {
    if ports.start >= ports.end {
        return Vec::new();
    }
    overlapping(bands, ports.start, ports.end - 1)
        .iter()
        .filter(|b| b.managed)
        .map(ReservedBand::label)
        .collect()
}

/// Find a band of `size` free ports as close as possible to `preferred`,
/// walking downwards first so the suggestion usually sits just below the
/// reserved block the user ran into.
///
/// Avoids *every* band including the managed ones: this is only consulted after
/// a genuine conflict, and recommending a replacement that is itself reserved
/// would be worse than recommending nothing.
pub fn suggest(size: u16, bands: &[ReservedBand], preferred: u16) -> Option<(u16, u16)> {
    let size = size.max(1) as usize;
    let overlaps = |lo: u16, hi: u16| bands.iter().any(|b| lo <= b.end && b.start <= hi);
    let candidate = |lo: usize| -> Option<(u16, u16)> {
        let hi = lo + size - 1;
        if lo < MIN_PASSIVE_PORT as usize || hi > u16::MAX as usize {
            return None;
        }
        let (lo, hi) = (lo as u16, hi as u16);
        (!overlaps(lo, hi)).then_some((lo, hi))
    };

    let preferred = preferred as usize;
    let downwards = (MIN_PASSIVE_PORT as usize..=preferred).rev().step_by(size);
    let upwards = ((preferred + size)..=(u16::MAX as usize + 1 - size)).step_by(size);
    downwards.chain(upwards).find_map(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim shape of `netsh ... excludedportrange protocol=tcp` on a
    /// Chinese Windows, including the `*` marker column and the legend line.
    const NETSH_TABLE: &str = "协议 tcp 端口排除范围\n\n开始端口    结束端口      \n\
        ----------    --------      \n\
        \x20    28385       28385      \n\x20    28390       28390      \n\
        \x20    50000       50059     *\n\x20    50131       50131      \n\n\
        * - 管理的端口排除。\n";

    fn plain(start: u16, end: u16) -> ReservedBand {
        ReservedBand { start, end, managed: false }
    }

    fn managed(start: u16, end: u16) -> ReservedBand {
        ReservedBand { start, end, managed: true }
    }

    #[test]
    fn parses_the_way_the_ui_shows_it() {
        // inclusive as displayed -> half-open for libunftp
        assert_eq!(parse("50000-50099").unwrap(), 50000..50100);
        assert_eq!(parse(" 50000 - 50099 ").unwrap(), 50000..50100);
        // half-open spelled out
        assert_eq!(parse("50000..50100").unwrap(), 50000..50100);
        // single port
        assert_eq!(parse("2121").unwrap(), 2121..2122);
        // empty -> default
        assert_eq!(parse("").unwrap(), DEFAULT_PASSIVE_PORTS);
    }

    #[test]
    fn rejects_broken_specs() {
        assert!(parse("50099-50000").is_err());
        assert!(parse("abc").is_err());
        assert!(parse("50000-").is_err());
        assert!(parse("50000-70000").is_err());
        assert!(parse("50000-65535").is_err(), "闭区间写法无法表示 65535");
    }

    #[test]
    fn label_round_trips_through_parse() {
        let ports = 40000..40010;
        assert_eq!(label(&ports), "40000-40009");
        assert_eq!(parse(&label(&ports)).unwrap(), ports);
    }

    #[test]
    fn validate_catches_what_libunftp_would_swallow() {
        assert!(validate(&(50000..50100)).is_ok());
        assert!(validate(&(50000..50000)).is_err());
        assert!(validate(&(50000..49999)).is_err());
        assert!(validate(&(80..90)).is_err(), "低端口应当被拒绝");
    }

    #[test]
    fn parses_netsh_rows_and_keeps_the_managed_marker() {
        let bands = parse_excluded_ranges(NETSH_TABLE);
        assert_eq!(
            bands,
            vec![
                plain(28385, 28385),
                plain(28390, 28390),
                // the row carries `*`, so it is a managed exclusion
                managed(50000, 50059),
                plain(50131, 50131),
            ]
        );
        assert_eq!(bands.len(), 4, "图例行不含数字，不能被当成数据行");
    }

    #[test]
    fn the_default_band_only_overlaps_a_managed_one_on_this_machine() {
        let bands = parse_excluded_ranges(NETSH_TABLE);

        // The old code called this a conflict and offered 49900-49999. It is a
        // managed exclusion, and libunftp binds a specific address for PASV, so
        // it does not actually break anything — no alarm, no suggestion.
        assert!(conflicts(&DEFAULT_PASSIVE_PORTS, &bands).is_empty());
        assert_eq!(managed_overlaps(&DEFAULT_PASSIVE_PORTS, &bands), vec!["50000-50059"]);
    }

    #[test]
    fn a_plain_exclusion_is_still_a_real_conflict() {
        let bands = parse_excluded_ranges(NETSH_TABLE);
        let ports = 28300..28400; // covers both plain bands (28385 and 28390)

        assert_eq!(
            conflicts(&ports, &bands),
            vec!["28385-28385", "28390-28390"]
        );
        assert!(managed_overlaps(&ports, &bands).is_empty());
        // and the suggestion must dodge both
        let (lo, hi) = suggest(100, &bands, 28300).expect("高段还有空位");
        for reserved in [28385u16, 28390] {
            assert!(
                !(lo <= reserved && reserved <= hi),
                "建议段 {lo}-{hi} 不能压在 {reserved} 上"
            );
        }
    }

    #[test]
    fn a_band_clear_of_everything_is_clean() {
        let bands = parse_excluded_ranges(NETSH_TABLE);
        assert!(conflicts(&(40000..40010), &bands).is_empty());
        assert!(managed_overlaps(&(40000..40010), &bands).is_empty());
    }

    #[test]
    fn suggests_a_band_just_below_the_reserved_one() {
        let bands = vec![plain(50000, 50059)];
        assert_eq!(suggest(100, &bands, 50000), Some((49900, 49999)));
    }

    #[test]
    fn suggestion_never_leaves_the_usable_band() {
        // A very crowded reservation list: the walk must still terminate and
        // stay above the privileged ports.
        let crowded: Vec<ReservedBand> = (1000..=50000)
            .step_by(1000)
            .map(|a| plain(a, a + 999))
            .collect();
        let picked = suggest(100, &crowded, 20000).expect("高段还有空位");
        assert!(picked.0 >= MIN_PASSIVE_PORT);
        assert!(picked.1 < u16::MAX);

        // Nothing free at all -> no suggestion rather than a bogus one.
        assert_eq!(suggest(100, &[plain(0, u16::MAX)], 50000), None);
    }
}
