//! License key generator — NOT included in WASM builds.
//!
//! Produces base64url-encoded HMAC-SHA256 signed license keys for pdf-core.
//!
//! # Usage
//!
//! ```text
//! cargo run --features crypto --bin keygen -- --tier pro --licensee "Acme Corp"
//! cargo run --features crypto --bin keygen -- --tier enterprise --licensee "Big Corp" --expiry 2027-01-01
//! cargo run --features crypto --bin keygen -- --tier free --licensee "Trial User"
//! ```

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let args: Vec<String> = std::env::args().collect();

    let tier_str = flag_value(&args, "--tier").unwrap_or_else(|| {
        eprintln!("error: --tier <free|pro|enterprise> is required");
        std::process::exit(1);
    });
    let licensee = flag_value(&args, "--licensee").unwrap_or_else(|| {
        eprintln!("error: --licensee <name> is required");
        std::process::exit(1);
    });
    let expiry_str = flag_value(&args, "--expiry");

    let tier = match tier_str.to_lowercase().as_str() {
        "free" => pdf_core::license::Tier::Free,
        "pro" => pdf_core::license::Tier::Pro,
        "enterprise" => pdf_core::license::Tier::Enterprise,
        other => {
            eprintln!(
                "error: unknown tier '{}'; use free, pro, or enterprise",
                other
            );
            std::process::exit(1);
        }
    };

    let expiry_ts: u64 = match expiry_str {
        None => 0, // perpetual
        Some(s) => parse_date(&s).unwrap_or_else(|e| {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }),
    };

    let key = pdf_core::license::encode_license_key(tier, expiry_ts, &licensee);
    println!("{}", key);
}

/// Parse `YYYY-MM-DD` into a Unix timestamp (midnight UTC).
#[cfg(not(target_arch = "wasm32"))]
fn parse_date(s: &str) -> Result<u64, String> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err(format!("--expiry must be YYYY-MM-DD, got '{}'", s));
    }
    let year: i64 = parts[0]
        .parse()
        .map_err(|_| format!("invalid year in '{}'", s))?;
    let month: u64 = parts[1]
        .parse()
        .map_err(|_| format!("invalid month in '{}'", s))?;
    let day: u64 = parts[2]
        .parse()
        .map_err(|_| format!("invalid day in '{}'", s))?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(format!("date out of range: '{}'", s));
    }

    // Days since Unix epoch using the proleptic Gregorian calendar (good
    // enough for license expiry precision — off by at most 1 day for dates
    // near leap years, which is acceptable here).
    let days = days_since_epoch(year, month, day)?;
    Ok(days * 86_400)
}

#[cfg(not(target_arch = "wasm32"))]
fn days_since_epoch(year: i64, month: u64, day: u64) -> Result<u64, String> {
    // Zeller / civil day count: days from 1970-01-01.
    let m = month as i64;
    let d = day as i64;
    let y = if m <= 2 { year - 1 } else { year };
    let m_adj = if m <= 2 { m + 9 } else { m - 3 };

    let jdn = (365 * y) + (y / 4) - (y / 100) + (y / 400) + ((153 * m_adj + 2) / 5) + d + 1_721_119;

    // Julian Day Number of 1970-01-01 is 2_440_588.
    let epoch_jdn: i64 = 2_440_588;
    let delta = jdn - epoch_jdn;
    if delta < 0 {
        return Err(format!("expiry date {} is before 1970-01-01", year));
    }
    Ok(delta as u64)
}

#[cfg(not(target_arch = "wasm32"))]
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

#[cfg(target_arch = "wasm32")]
fn main() {}
