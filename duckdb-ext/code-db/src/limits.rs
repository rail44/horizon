//! DuckDB's memory, thread and spill limits, derived from the machine's state
//! (duckdb-ext/sql/README.md, Limits). 00_load.sql receives the results as
//! the variables memory_limit, threads, temp_limit and temp_directory.

/// What DuckDB reports before anything is set: its default memory_limit (80 %
/// of physical memory), its default thread count (the CPUs) and its default
/// max_temp_directory_size, as `current_setting()` prints it.
pub struct DuckDbDefaults {
    pub memory_bytes: f64,
    pub cpus: i64,
    pub max_temp: String,
}

/// The overrides the user gave (NAME=VALUE), as text.
#[derive(Default)]
pub struct Overrides {
    pub memory_limit: Option<String>,
    pub threads: Option<String>,
    pub temp_limit: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct Limits {
    /// The value for `SET memory_limit`.
    pub memory_limit: String,
    pub threads: i64,
    /// The value for `SET max_temp_directory_size`; `None` keeps DuckDB's default.
    pub temp_limit: Option<String>,
    /// The one-line summary.
    pub line: String,
}

/// `MemAvailable` from /proc/meminfo's text, in bytes.
pub fn mem_available(meminfo: &str) -> Option<f64> {
    meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix("MemAvailable:")?;
        let kb = rest.trim().strip_suffix("kB")?.trim();
        kb.parse::<u64>().ok().map(|kb| kb as f64 * 1024.0)
    })
}

/// A size as `current_setting()` prints it or `SET` accepts it ('49.6 GiB',
/// '4GB', '512 bytes') in bytes; `None` for anything else.
pub fn setting_bytes(text: &str) -> Option<f64> {
    let t = text.trim();
    let digits = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let number: f64 = t[..digits].parse().ok()?;
    let unit = t[digits..].trim();
    if !unit.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let factor = match unit.to_ascii_uppercase().as_str() {
        "BYTES" | "B" => 1.0,
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KIB" => 1024.0,
        "MIB" => 1024f64.powi(2),
        "GIB" => 1024f64.powi(3),
        "TIB" => 1024f64.powi(4),
        _ => return None,
    };
    Some(number * factor)
}

/// DuckDB's `formatReadableDecimalSize`: the leading unit with one truncated
/// decimal ('25.3 GB', '1.0 kB'), or the byte count below 1000.
pub fn readable_decimal_size(bytes: i64) -> String {
    let bytes = bytes.max(0) as u64;
    let units = ["bytes", "kB", "MB", "GB", "TB", "PB"];
    let mut parts = [0u64; 6];
    parts[0] = bytes;
    for i in 1..6 {
        parts[i] = parts[i - 1] / 1000;
        parts[i - 1] %= 1000;
    }
    for i in (1..6).rev() {
        if parts[i] != 0 {
            return format!("{}.{} {}", parts[i], parts[i - 1] * 10 / 1000, units[i]);
        }
    }
    format!("{} {}", bytes, if bytes == 1 { "byte" } else { "bytes" })
}

/// A DOUBLE cast to BIGINT the way DuckDB does it (rounding half away from zero).
fn to_bigint(x: f64) -> i64 {
    x.round() as i64
}

/// Derive the limits. `available` is MemAvailable in bytes (`None` when
/// /proc/meminfo is unreadable), `temp_free` the free bytes of the file
/// system that holds the spill directory (`None` when unknown).
pub fn derive(
    defaults: &DuckDbDefaults,
    available: Option<f64>,
    temp_free: Option<f64>,
    overrides: &Overrides,
) -> Result<Limits, String> {
    let default_memory = defaults.memory_bytes;
    // 60 % of what the OS can give now (of DuckDB's default without
    // /proc/meminfo), at most DuckDB's default, at least 2 GB. The other 40 %
    // is for allocations outside DuckDB's accounting.
    let derived_memory = match &overrides.memory_limit {
        Some(_) => None,
        None => {
            let base = available.unwrap_or(default_memory);
            let candidate = (0.6 * base).min(default_memory);
            if candidate >= 2e9 {
                Some(candidate)
            } else {
                return Err(format!(
                    "only {} of memory is available, and 60% of it is below the 2 GB floor: \
                     free memory, or set the variable memory_limit (README.md, Limits)",
                    readable_decimal_size(to_bigint(base))
                ));
            }
        }
    };
    let (memory_limit, memory) = match (&overrides.memory_limit, derived_memory) {
        (Some(text), _) => {
            let bytes = setting_bytes(text)
                .ok_or_else(|| format!("memory_limit must be a size such as 4GB: {text}"))?;
            (text.clone(), bytes)
        }
        (None, Some(derived)) => (format!("{} bytes", to_bigint(derived)), derived),
        (None, None) => unreachable!("memory_limit is either set or derived"),
    };
    // One thread per GB of memory_limit, at most the CPU count: each thread's
    // operator state then has a share of the limit to work in.
    let threads = match &overrides.threads {
        Some(text) => text
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("threads must be a positive number: {text}"))?,
        None => defaults.cpus.min(((memory / 1e9).floor() as i64).max(1)),
    };
    // Spill files are capped at half of the free space of their file system.
    let temp_limit = match (&overrides.temp_limit, temp_free) {
        (Some(text), _) => Some(text.clone()),
        (None, Some(free)) => Some(format!("{} bytes", to_bigint(0.5 * free))),
        (None, None) => None,
    };

    let memory_source = if overrides.memory_limit.is_some() {
        "memory_limit".to_owned()
    } else {
        match available {
            None => format!(
                "60% of DuckDB's default {}; /proc/meminfo unreadable",
                readable_decimal_size(to_bigint(default_memory))
            ),
            Some(a) if derived_memory.unwrap_or(0.0) >= default_memory => format!(
                "DuckDB's default, below 60% of {} available",
                readable_decimal_size(to_bigint(a))
            ),
            Some(a) => format!(
                "60% of {} available, /proc/meminfo",
                readable_decimal_size(to_bigint(a))
            ),
        }
    };
    let temp = match (&overrides.temp_limit, temp_free) {
        (Some(text), _) => {
            let bytes = setting_bytes(text)
                .ok_or_else(|| format!("temp_limit must be a size such as 1GB: {text}"))?;
            format!("{} (temp_limit)", readable_decimal_size(to_bigint(bytes)))
        }
        (None, Some(free)) => format!(
            "{} (50% of {} free)",
            readable_decimal_size(to_bigint(0.5 * free)),
            readable_decimal_size(to_bigint(free))
        ),
        (None, None) => format!(
            "DuckDB's default ({}; free space unknown)",
            defaults.max_temp
        ),
    };
    let threads_source = if overrides.threads.is_some() {
        "threads".to_owned()
    } else if threads < defaults.cpus {
        format!("1 per GB of memory; {} CPUs", defaults.cpus)
    } else {
        "CPUs".to_owned()
    };
    let line = format!(
        "limits: memory {} ({memory_source}), temp {temp}, threads {threads} ({threads_source})",
        readable_decimal_size(to_bigint(memory))
    );
    Ok(Limits {
        memory_limit,
        threads,
        temp_limit,
        line,
    })
}

/// Free bytes (for an unprivileged user) of the file system holding `path`.
pub fn free_bytes(path: &std::path::Path) -> Option<f64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some(stat.f_bavail as f64 * stat.f_frsize as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "MemTotal:       65536000 kB\n\
                           MemFree:         1000000 kB\n\
                           MemAvailable:   41200000 kB\n\
                           Buffers:          100000 kB\n";

    fn defaults() -> DuckDbDefaults {
        DuckDbDefaults {
            memory_bytes: 53.6e9,
            cpus: 24,
            max_temp: "90% of available disk space".to_owned(),
        }
    }

    #[test]
    fn mem_available_reads_the_kb_line() {
        assert_eq!(mem_available(MEMINFO), Some(41_200_000.0 * 1024.0));
        assert_eq!(mem_available("MemTotal: 1 kB\n"), None);
    }

    #[test]
    fn sixty_percent_of_available_memory() {
        let available = mem_available(MEMINFO);
        let l = derive(&defaults(), available, Some(58.2e9), &Overrides::default()).unwrap();
        let memory = (0.6 * 41_200_000.0 * 1024.0f64).round() as i64;
        assert_eq!(l.memory_limit, format!("{memory} bytes"));
        assert_eq!(l.threads, 24);
        assert_eq!(l.temp_limit.as_deref(), Some("29100000000 bytes"));
        assert_eq!(
            l.line,
            "limits: memory 25.3 GB (60% of 42.1 GB available, /proc/meminfo), \
             temp 29.1 GB (50% of 58.2 GB free), threads 24 (CPUs)"
        );
    }

    #[test]
    fn threads_follow_memory_and_the_default_caps_memory() {
        // 6 GB available: 3.6 GB, so 3 threads.
        let l = derive(&defaults(), Some(6e9), None, &Overrides::default()).unwrap();
        assert_eq!(l.threads, 3);
        assert_eq!(l.temp_limit, None);
        assert_eq!(
            l.line,
            "limits: memory 3.6 GB (60% of 6.0 GB available, /proc/meminfo), \
             temp DuckDB's default (90% of available disk space; free space unknown), \
             threads 3 (1 per GB of memory; 24 CPUs)"
        );
        // More available than the default allows: the default.
        let l = derive(&defaults(), Some(100e9), None, &Overrides::default()).unwrap();
        assert_eq!(l.memory_limit, "53600000000 bytes");
        assert!(
            l.line
                .contains("(DuckDB's default, below 60% of 100.0 GB available)")
        );
    }

    #[test]
    fn without_meminfo_sixty_percent_of_the_default() {
        let l = derive(&defaults(), None, None, &Overrides::default()).unwrap();
        assert_eq!(l.memory_limit, "32160000000 bytes");
        assert!(
            l.line
                .contains("(60% of DuckDB's default 53.6 GB; /proc/meminfo unreadable)")
        );
    }

    #[test]
    fn below_the_floor_is_an_error() {
        let e = derive(&defaults(), Some(3e9), None, &Overrides::default()).unwrap_err();
        assert!(e.starts_with("only 3.0 GB of memory is available"), "{e}");
    }

    #[test]
    fn overrides_name_their_variable() {
        let o = Overrides {
            memory_limit: Some("4GB".to_owned()),
            threads: Some("2".to_owned()),
            temp_limit: Some("1GB".to_owned()),
        };
        let l = derive(&defaults(), Some(3e9), Some(1e12), &o).unwrap();
        assert_eq!(l.memory_limit, "4GB");
        assert_eq!(l.threads, 2);
        assert_eq!(l.temp_limit.as_deref(), Some("1GB"));
        assert_eq!(
            l.line,
            "limits: memory 4.0 GB (memory_limit), temp 1.0 GB (temp_limit), threads 2 (threads)"
        );
        // memory_limit alone: threads from it.
        let o = Overrides {
            memory_limit: Some("4 GiB".to_owned()),
            ..Overrides::default()
        };
        assert_eq!(derive(&defaults(), None, None, &o).unwrap().threads, 4);
    }

    #[test]
    fn sizes_parse_and_format_as_duckdb_does() {
        assert_eq!(setting_bytes("49.6 GiB"), Some(49.6 * 1024f64.powi(3)));
        assert_eq!(setting_bytes("4GB"), Some(4e9));
        assert_eq!(setting_bytes("512 bytes"), Some(512.0));
        assert_eq!(setting_bytes("lots"), None);
        assert_eq!(readable_decimal_size(1), "1 byte");
        assert_eq!(readable_decimal_size(999), "999 bytes");
        assert_eq!(readable_decimal_size(1000), "1.0 kB");
        assert_eq!(readable_decimal_size(1_999_999), "1.9 MB");
        assert_eq!(readable_decimal_size(25_312_345_678), "25.3 GB");
    }

    #[test]
    fn readable_sizes_equal_duckdbs() {
        let db = crate::db::Db::open().unwrap();
        for bytes in [
            0i64,
            1,
            2,
            999,
            1000,
            1001,
            999_999,
            1_000_000,
            1_999_999,
            42_188_800_000,
            25_313_280_000,
            312_700_000_000_000,
            9_999_999_999_999_999,
        ] {
            let duckdb = db
                .value(&format!("SELECT formatReadableDecimalSize({bytes})"))
                .unwrap()
                .unwrap();
            assert_eq!(readable_decimal_size(bytes), duckdb, "{bytes}");
        }
        for text in ["4GB", "1.5 GiB", "700 MiB"] {
            let duckdb = db
                .value(&format!(
                    "SET memory_limit = '{text}'; SELECT current_setting('memory_limit')"
                ))
                .unwrap()
                .unwrap();
            // DuckDB prints the size it set in its own unit; both parse to about the same bytes.
            let (ours, theirs) = (
                setting_bytes(text).unwrap(),
                setting_bytes(&duckdb).unwrap(),
            );
            assert!((ours - theirs).abs() / ours < 0.01, "{text}: {duckdb}");
        }
    }
}
