use std::sync::LazyLock;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use maki_config::ClockFormat;

pub fn hm(format: ClockFormat) -> &'static str {
    if is_12h(format) { "%-I:%M %p" } else { "%H:%M" }
}

pub fn hms(format: ClockFormat) -> &'static str {
    if is_12h(format) {
        "%-I:%M:%S %p"
    } else {
        "%H:%M:%S"
    }
}

pub fn format_message_timestamp(epoch_ms: u64, format: ClockFormat) -> String {
    let tz = TimeZone::system();
    let now = Timestamp::now().to_zoned(tz.clone());
    format_message_timestamp_with_now(epoch_ms, format, &now, &tz)
}

pub(crate) fn format_message_timestamp_with_now(
    epoch_ms: u64,
    format: ClockFormat,
    now: &jiff::Zoned,
    tz: &TimeZone,
) -> String {
    let Ok(ts) = Timestamp::from_millisecond(epoch_ms as i64) else {
        return String::new();
    };
    let zoned = ts.to_zoned(tz.clone());
    let time_fmt = hm(format);
    if zoned.date() == now.date() {
        zoned.strftime(time_fmt).to_string()
    } else {
        zoned.strftime(&format!("%Y-%m-%d {time_fmt}")).to_string()
    }
}

fn is_12h(format: ClockFormat) -> bool {
    static SYSTEM_12H: LazyLock<bool> = LazyLock::new(system_uses_12h);
    match format {
        ClockFormat::Hour12 => true,
        ClockFormat::Hour24 => false,
        ClockFormat::System => *SYSTEM_12H,
    }
}

/// A POSIX `T_FMT` pattern is 12-hour when it renders a 12-hour field:
/// hour (`%I`, `%l`), AM/PM marker (`%p`), or the whole time (`%r`).
#[cfg(any(all(unix, not(target_os = "android")), test))]
fn posix_fmt_is_12h(fmt: &[u8]) -> bool {
    fmt.windows(2)
        .any(|w| matches!(w, b"%I" | b"%l" | b"%p" | b"%r"))
}

// bionic ignores the locale in `nl_langinfo_l`, so it cannot tell whether the
// system uses a 12-hour clock.
#[cfg(all(unix, target_os = "android"))]
fn system_uses_12h() -> bool {
    false
}

#[cfg(all(unix, not(target_os = "android")))]
fn system_uses_12h() -> bool {
    use std::ffi::CStr;

    // The libc crate does not bind `nl_langinfo_l` on Apple, NetBSD, OpenBSD
    // or Haiku. On Haiku it also misses `T_FMT`, `newlocale`, `freelocale` and
    // `LC_TIME_MASK`. The C library provides all of them, so declare what is
    // missing here instead of turning the detection off.
    #[cfg(target_os = "haiku")]
    type LocaleT = *mut libc::c_void;
    #[cfg(not(target_os = "haiku"))]
    type LocaleT = libc::locale_t;

    #[cfg(target_os = "haiku")]
    unsafe extern "C" {
        fn newlocale(mask: libc::c_int, locale: *const libc::c_char, base: LocaleT) -> LocaleT;
        fn freelocale(loc: LocaleT);
    }
    #[cfg(not(target_os = "haiku"))]
    use libc::{freelocale, newlocale};
    unsafe extern "C" {
        fn nl_langinfo_l(item: libc::nl_item, locale: LocaleT) -> *mut libc::c_char;
    }

    #[cfg(target_os = "haiku")]
    const T_FMT: libc::nl_item = 3; // Haiku posix/langinfo.h
    #[cfg(not(target_os = "haiku"))]
    const T_FMT: libc::nl_item = libc::T_FMT;

    #[cfg(target_os = "haiku")]
    const LC_TIME_MASK: libc::c_int = 1 << (5 - 1); // Haiku: LC_TIME = 5
    #[cfg(not(target_os = "haiku"))]
    const LC_TIME_MASK: libc::c_int = libc::LC_TIME_MASK;

    unsafe {
        let loc = newlocale(LC_TIME_MASK, c"".as_ptr(), std::ptr::null_mut());
        if loc.is_null() {
            return false;
        }
        let fmt = nl_langinfo_l(T_FMT, loc);
        let uses_12h = !fmt.is_null() && posix_fmt_is_12h(CStr::from_ptr(fmt).to_bytes());
        freelocale(loc);
        uses_12h
    }
}

#[cfg(windows)]
fn system_uses_12h() -> bool {
    use windows_sys::Win32::Globalization::{GetLocaleInfoEx, LOCALE_STIMEFORMAT};
    let mut buf = [0u16; 128];
    let len = unsafe {
        GetLocaleInfoEx(
            std::ptr::null(),
            LOCALE_STIMEFORMAT,
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    // `len` counts the terminating NUL. Windows patterns spell 24-hour
    // hours as `H` and 12-hour ones as `h`, so no `H` means 12-hour.
    len > 1 && !buf[..(len - 1) as usize].contains(&u16::from(b'H'))
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::posix_fmt_is_12h;

    #[test_case(b"%r", true ; "twelve_hour_r")]
    #[test_case(b"%I:%M:%S %p", true ; "twelve_hour_i_and_p")]
    #[test_case(b"%l:%M %p", true ; "twelve_hour_l")]
    #[test_case(b"%T", false ; "twenty_four_hour_t")]
    #[test_case(b"%H:%M:%S", false ; "twenty_four_hour_h")]
    #[test_case(b"", false ; "empty_defaults_to_24h")]
    fn posix_fmt(fmt: &[u8], expected: bool) {
        assert_eq!(posix_fmt_is_12h(fmt), expected);
    }

    #[test]
    fn format_message_timestamp_for_today_and_past() {
        use jiff::Timestamp;
        use jiff::tz::TimeZone;
        use maki_config::ClockFormat;

        let tz = TimeZone::UTC;
        // 2026-11-16 14:30:00 UTC
        let now = Timestamp::from_second(1794839400)
            .unwrap()
            .to_zoned(tz.clone());

        // Same day: 2026-11-16 10:15:00 UTC
        let today_ts = 1_794_824_100_000_u64;
        let today_24 = super::format_message_timestamp_with_now(today_ts, ClockFormat::Hour24, &now, &tz);
        assert_eq!(today_24, "10:15");
        let today_12 = super::format_message_timestamp_with_now(today_ts, ClockFormat::Hour12, &now, &tz);
        assert_eq!(today_12, "10:15 AM");

        // Past day: 2026-11-15 16:45:00 UTC
        let past_ts = 1_794_761_100_000_u64;
        let past_24 = super::format_message_timestamp_with_now(past_ts, ClockFormat::Hour24, &now, &tz);
        assert_eq!(past_24, "2026-11-15 16:45");
        let past_12 = super::format_message_timestamp_with_now(past_ts, ClockFormat::Hour12, &now, &tz);
        assert_eq!(past_12, "2026-11-15 4:45 PM");
    }
}
