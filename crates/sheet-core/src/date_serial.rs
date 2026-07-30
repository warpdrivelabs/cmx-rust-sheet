//! Excel 日期序列基座（1900 日期系统）。对标 cmx-megasheet 的 core/dateSerial.ts。
//!
//! Excel 把日期时间存成「序列号」：整数部分 = 自 1900-01-01 起的天数（serial 1 = 1900-01-01），
//! 小数部分 = 当天时刻（0.5 = 12:00:00）。含著名的「1900 闰年 bug」——Excel 误认 1900 为闰年，
//! 存在虚构的 serial 60 = 1900-02-29。为与 Excel 对齐：serial ≥ 61（1900-03-01 起）实际天数减 1
//! 补偿虚构日；反向 dateToSerial 对 1900-03-01 起的日期 +1 补回。
//!
//! 供 numfmt 日期掩码渲染、日期函数（DATE/YEAR/TODAY…）、日期编辑器共用。
//!
//! Rust 移植取舍：TS 用浏览器 `Date.UTC` 做历法换算；Rust 用 Howard Hinnant 的
//! days-from-civil / civil-from-days 纯整数算法（无 chrono 依赖，sheet-core 保持轻量）。

/// 日期时间分量（1-based 月，0=周日..6=周六）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateParts {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    /// 0=Sun .. 6=Sat。
    pub weekday: u32,
    pub hours: u32,
    pub minutes: u32,
    pub seconds: u32,
}

/// 历法（1-based 月）→ 自 1970-01-01 的天数（Howard Hinnant）。
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - (m <= 2) as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mi = m as i64;
    let doy = (153 * (if mi > 2 { mi - 3 } else { mi + 9 }) + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// 自 1970-01-01 的天数 → 历法（1-based 月）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (y + (m <= 2) as i64, m, d)
}

/// 锚点：1899-12-31 的天数偏移（serial 1 → +1 天 → 1900-01-01）。
fn epoch_days() -> i64 {
    days_from_civil(1899, 12, 31)
}

/// 天数 → 星期（0=Sun..6=Sat）。1970-01-01（day 0）是周四=4。
fn weekday_of(abs_days: i64) -> u32 {
    (((abs_days % 7) + 4) % 7 + 7) as u32 % 7
}

/// 序列号 → 日期时间分量。整数走日期（含闰年 bug 补偿），小数走时刻。
pub fn serial_to_parts(serial: f64) -> DateParts {
    let day_part = serial.floor();
    // 闰年 bug 补偿：serial ≥ 61 实际比朴素天数少 1 天
    let shifted = if day_part >= 61.0 {
        day_part - 1.0
    } else {
        day_part
    };
    let frac = serial - day_part;
    let sec_of_day = (frac * 86400.0).round() as i64; // 0..=86400（86400 跨到次日）
    let extra_days = sec_of_day / 86400;
    let sec = sec_of_day % 86400;
    let abs_days = epoch_days() + shifted as i64 + extra_days;
    let (year, month, day) = civil_from_days(abs_days);
    DateParts {
        year,
        month,
        day,
        weekday: weekday_of(abs_days),
        hours: (sec / 3600) as u32,
        minutes: ((sec % 3600) / 60) as u32,
        seconds: (sec % 60) as u32,
    }
}

/// 历法（1-based 月）→ 序列号（整数天）。1900-03-01 起 +1 补回虚构闰日。
pub fn date_to_serial(year: i64, month: u32, day: u32) -> f64 {
    let mut s = days_from_civil(year, month, day) - epoch_days();
    if s >= 60 {
        s += 1; // 1900-03-01（朴素第 60 天）→ serial 61
    }
    s as f64
}

/// 时刻 → 序列号小数部分。
pub fn time_to_fraction(hours: i64, minutes: i64, seconds: i64) -> f64 {
    (hours * 3600 + minutes * 60 + seconds) as f64 / 86400.0
}

/// 序列号 → 时刻分量（丢弃日期部分）。
pub fn serial_to_time(serial: f64) -> (u32, u32, u32) {
    let p = serial_to_parts(serial);
    (p.hours, p.minutes, p.seconds)
}

/// 历法 + 时刻 → 完整序列号（整数天 + 小数时刻）。
pub fn parts_to_serial(
    year: i64,
    month: u32,
    day: u32,
    hours: i64,
    minutes: i64,
    seconds: i64,
) -> f64 {
    date_to_serial(year, month, day) + time_to_fraction(hours, minutes, seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_anchors() {
        assert_eq!(date_to_serial(1900, 1, 1), 1.0);
        assert_eq!(date_to_serial(1900, 3, 1), 61.0); // 跳过虚构 serial 60
        assert_eq!(date_to_serial(1999, 12, 31), 36525.0);
        assert_eq!(date_to_serial(2024, 1, 1), 45292.0);
        assert_eq!(date_to_serial(2024, 1, 15), 45306.0);
    }

    #[test]
    fn parts_and_weekday() {
        let p = serial_to_parts(45306.0);
        assert_eq!(p.year, 2024);
        assert_eq!(p.month, 1);
        assert_eq!(p.day, 15);
        assert_eq!(p.weekday, 1); // 周一
    }

    #[test]
    fn fractional_time() {
        assert_eq!(serial_to_time(0.5), (12, 0, 0));
        let t = serial_to_time(0.25 + 30.0 / 86400.0);
        assert_eq!(t, (6, 0, 30));
    }

    #[test]
    fn round_trip_time() {
        assert!((time_to_fraction(12, 0, 0) - 0.5).abs() < 1e-10);
        let s = parts_to_serial(2024, 6, 15, 13, 45, 30);
        let p = serial_to_parts(s);
        assert_eq!(
            (p.year, p.month, p.day, p.hours, p.minutes, p.seconds),
            (2024, 6, 15, 13, 45, 30)
        );
    }

    #[test]
    fn date_serial_round_trip() {
        for (y, m, d) in [(1900, 3, 1), (1901, 1, 1), (2000, 2, 29), (2024, 12, 31)] {
            let s = date_to_serial(y, m, d);
            let p = serial_to_parts(s);
            assert_eq!((p.year, p.month, p.day), (y, m, d));
        }
    }
}
