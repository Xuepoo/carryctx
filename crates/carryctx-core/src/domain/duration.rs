use crate::error::CarryCtxError;

/// Parse a duration string (e.g., "2h30m", "500ms", "7d") into milliseconds
pub fn parse_duration(s: &str) -> Result<u64, CarryCtxError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(CarryCtxError::validation_error("Duration cannot be empty."));
    }

    let mut total: u64 = 0;
    let mut num_buf = String::new();
    let mut suffix_buf = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            if !suffix_buf.is_empty() {
                // Starting a new number after a suffix - apply previous suffix
                apply_suffix(&mut total, &num_buf, &suffix_buf)?;
                num_buf.clear();
                suffix_buf.clear();
            }
            num_buf.push(ch);
        } else {
            suffix_buf.push(ch);
        }
    }

    // Apply final suffix
    if !num_buf.is_empty() {
        apply_suffix(&mut total, &num_buf, &suffix_buf)?;
    } else if !suffix_buf.is_empty() {
        return Err(CarryCtxError::validation_error(format!(
            "Invalid duration: {}",
            s
        )));
    }

    if total == 0 {
        return Err(CarryCtxError::validation_error(
            "Duration must be positive.",
        ));
    }

    Ok(total)
}

fn apply_suffix(total: &mut u64, num_str: &str, suffix: &str) -> Result<(), CarryCtxError> {
    let value: u64 = num_str.parse().map_err(|_| {
        CarryCtxError::validation_error(format!("Invalid duration number: {}", num_str))
    })?;

    let ms = match suffix {
        "ms" => value,
        "s" => value.checked_mul(1000).ok_or_else(overflow_err)?,
        "m" => value.checked_mul(60 * 1000).ok_or_else(overflow_err)?,
        "h" => value.checked_mul(3600 * 1000).ok_or_else(overflow_err)?,
        "d" => value
            .checked_mul(24 * 3600 * 1000)
            .ok_or_else(overflow_err)?,
        "w" => value
            .checked_mul(7 * 24 * 3600 * 1000)
            .ok_or_else(overflow_err)?,
        _ => {
            return Err(CarryCtxError::validation_error(format!(
                "Unknown duration suffix: {}",
                suffix
            )));
        }
    };

    *total = total.checked_add(ms).ok_or_else(overflow_err)?;
    Ok(())
}

fn overflow_err() -> CarryCtxError {
    CarryCtxError::validation_error("Duration overflow.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("500ms").unwrap(), 500);
        assert_eq!(parse_duration("5s").unwrap(), 5000);
        assert_eq!(parse_duration("2h").unwrap(), 2 * 60 * 60 * 1000);
        assert_eq!(parse_duration("1d").unwrap(), 24 * 60 * 60 * 1000);
        assert_eq!(parse_duration("1w").unwrap(), 7 * 24 * 60 * 60 * 1000);
        assert_eq!(
            parse_duration("2h30m").unwrap(),
            2 * 60 * 60 * 1000 + 30 * 60 * 1000
        );
        assert_eq!(
            parse_duration("1h30m15s500ms").unwrap(),
            3600000 + 1800000 + 15000 + 500
        );
    }

    #[test]
    fn test_invalid_durations() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("5").is_err());
    }
}
