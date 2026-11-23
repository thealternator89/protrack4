use crate::domain::DomainError;

/// ASCII-ordered Base-62 alphabet (`0-9`, `A-Z`, `a-z`).
pub const ALPHABET: &[u8; 62] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
pub const FIRST_KEY: &str = "V";
pub const MAX_SORT_KEY_LEN: usize = 32;

/// Returns the initial sort key (`"V"`, midpoint of the Base-62 alphabet).
pub fn first() -> String {
    FIRST_KEY.to_string()
}

/// Returns `true` when a key exceeds the 32-byte threshold and siblings should rebalance.
pub fn needs_rebalance(key: &str) -> bool {
    key.len() > MAX_SORT_KEY_LEN
}

/// Generates a Base-62 key strictly greater than `a`.
pub fn after(a: &str) -> Result<String, DomainError> {
    midpoint(Some(a), None)
}

/// Generates a Base-62 key strictly less than `b`.
pub fn before(b: &str) -> Result<String, DomainError> {
    midpoint(None, Some(b))
}

/// Generates a Base-62 key strictly between `a` and `b` (`a < m < b`).
pub fn between(a: &str, b: &str) -> Result<String, DomainError> {
    midpoint(Some(a), Some(b))
}

/// Generates the shortest lexicographic Base-62 key between optional `lower` and `upper` bounds.
///
/// Generated keys never end with `'0'`, guaranteeing room for subsequent insertions.
/// Returns a validation error if bounds are invalid or no key fits strictly between them
/// (for instance, `"a"` and `"a0"` or before `"0"`), allowing repositories to rebalance.
pub fn midpoint(lower: Option<&str>, upper: Option<&str>) -> Result<String, DomainError> {
    if let Some(lo) = lower {
        validate_key(lo)?;
    }
    if let Some(hi) = upper {
        validate_key(hi)?;
    }

    match (lower, upper) {
        (None, None) => Ok(first()),
        (Some(a), Some(b)) if a >= b => Err(DomainError::Validation(format!(
            "fractional index requires lower < upper (got '{a}' >= '{b}')"
        ))),
        _ => compute_midpoint(lower.map(str::as_bytes), upper.map(str::as_bytes)),
    }
}

/// Generates `count` evenly spaced, fixed-width Base-62 keys in strict ascending ASCII order.
///
/// Keys are 2 bytes for `count <= 1000`, 3 bytes for `count <= 50_000`, and 4 bytes above,
/// and never end in `'0'`.
pub fn rebalance_keys(count: usize) -> Vec<String> {
    if count == 0 {
        return Vec::new();
    }

    let width: usize = if count <= 1_000 {
        2
    } else if count <= 50_000 {
        3
    } else {
        4
    };

    let prefix_space = 62usize.pow((width - 1) as u32);
    let total_slots = prefix_space * 61;
    let mut keys = Vec::with_capacity(count);
    let mut prev_slot: Option<usize> = None;

    for i in 0..count {
        let mut slot = ((i + 1) * total_slots) / (count + 1);
        if let Some(prev) = prev_slot
            && slot <= prev
        {
            slot = prev + 1;
        }
        debug_assert!(slot < total_slots, "rebalance_keys exceeded slot space");
        prev_slot = Some(slot);

        let last_digit = (slot % 61) + 1; // 1..=61, never '0'
        let mut rem = slot / 61;
        let mut buf = vec![b'0'; width];
        buf[width - 1] = ALPHABET[last_digit];
        for pos in (0..width - 1).rev() {
            buf[pos] = ALPHABET[rem % 62];
            rem /= 62;
        }

        keys.push(String::from_utf8(buf).expect("ALPHABET is valid ASCII"));
    }

    keys
}

fn validate_key(key: &str) -> Result<(), DomainError> {
    if key.is_empty() {
        return Err(DomainError::Validation(
            "fractional index key cannot be empty".to_string(),
        ));
    }
    for &b in key.as_bytes() {
        char_to_idx(b)?;
    }
    Ok(())
}

fn char_to_idx(b: u8) -> Result<usize, DomainError> {
    match b {
        b'0'..=b'9' => Ok((b - b'0') as usize),
        b'A'..=b'Z' => Ok((b - b'A' + 10) as usize),
        b'a'..=b'z' => Ok((b - b'a' + 36) as usize),
        _ => Err(DomainError::Validation(format!(
            "invalid base-62 character '{}' in fractional index key",
            b as char
        ))),
    }
}

fn compute_midpoint(lower: Option<&[u8]>, upper: Option<&[u8]>) -> Result<String, DomainError> {
    let mut out = Vec::new();
    let mut upper_unbounded = upper.is_none();
    let mut i = 0usize;

    loop {
        let lo = lower
            .and_then(|bytes| bytes.get(i).copied())
            .map(char_to_idx)
            .transpose()?
            .unwrap_or(0);

        let hi = if upper_unbounded {
            62
        } else {
            let bytes = upper.expect("upper must be Some when upper_unbounded is false");
            let Some(&b) = bytes.get(i) else {
                return Err(DomainError::Validation(
                    "no base-62 midpoint exists between adjacent keys; rebalance required"
                        .to_string(),
                ));
            };
            char_to_idx(b)?
        };

        if lo == hi {
            out.push(ALPHABET[lo]);
            i += 1;
            continue;
        }

        if hi - lo >= 2 {
            let mid = (lo + hi) / 2;
            out.push(ALPHABET[mid]);
            return Ok(String::from_utf8(out).expect("ALPHABET is valid ASCII"));
        }

        // hi - lo == 1: keep lower's digit and step deeper with an unbounded upper digit.
        out.push(ALPHABET[lo]);
        upper_unbounded = true;
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_before_after_between_basics_and_boundaries() {
        assert_eq!(first(), "V");
        assert!(!needs_rebalance("V"));
        assert!(!needs_rebalance(&"V".repeat(32)));
        assert!(needs_rebalance(&"V".repeat(33)));

        let b = before("V").unwrap();
        assert!(b.as_str() < "V");
        assert!(!b.ends_with('0'));

        let a = after("V").unwrap();
        assert!("V" < a.as_str());
        assert!(!a.ends_with('0'));

        let m = between("V", "W").unwrap();
        assert!("V" < m.as_str() && m.as_str() < "W");
        assert!(!m.ends_with('0'));

        let after_max = after("z").unwrap();
        assert!("z" < after_max.as_str());
        assert!(!after_max.ends_with('0'));

        let before_low = before("1").unwrap();
        assert!(before_low.as_str() < "1");
        assert!(!before_low.ends_with('0'));

        let before_0v = before("0V").unwrap();
        assert!(before_0v.as_str() < "0V");
        assert!(!before_0v.ends_with('0'));

        let between_legacy = between("a0", "a1").unwrap();
        assert!("a0" < between_legacy.as_str() && between_legacy.as_str() < "a1");
        assert!(!between_legacy.ends_with('0'));

        assert!(between("a", "a0").is_err());
        assert!(before("0").is_err());
        assert!(between("W", "V").is_err());
        assert!(between("V", "V").is_err());
        assert!(after("").is_err());
        assert!(after("a!").is_err());
    }

    #[test]
    fn sequential_midpoint_splits_maintain_ordering_and_trigger_rebalance() {
        let mut lo = "V".to_string();
        let hi = "W".to_string();
        let mut triggered_rebalance = false;

        for _ in 0..200 {
            let mid = between(&lo, &hi).unwrap();
            assert!(
                lo < mid && mid < hi,
                "expected {lo} < {mid} < {hi}"
            );
            assert!(!mid.ends_with('0'));
            if needs_rebalance(&mid) {
                triggered_rebalance = true;
            }
            lo = mid;
        }
        assert!(
            triggered_rebalance,
            "200 repeated upper splits should exceed 32 bytes and trigger rebalance"
        );

        let lo_fixed = "V".to_string();
        let mut hi_shrinking = "W".to_string();
        let mut lower_triggered_rebalance = false;
        for _ in 0..200 {
            let mid = between(&lo_fixed, &hi_shrinking).unwrap();
            assert!(
                lo_fixed < mid && mid < hi_shrinking,
                "expected {lo_fixed} < {mid} < {hi_shrinking}"
            );
            assert!(!mid.ends_with('0'));
            if needs_rebalance(&mid) {
                lower_triggered_rebalance = true;
            }
            hi_shrinking = mid;
        }
        assert!(
            lower_triggered_rebalance,
            "200 repeated lower splits should exceed 32 bytes and trigger rebalance"
        );
    }

    #[test]
    fn sequential_appends_and_prepends_preserve_strict_order_and_rebalance() {
        let mut tail = first();
        let mut append_rebalanced = false;
        for _ in 0..200 {
            let next = after(&tail).unwrap();
            assert!(tail < next, "expected {tail} < {next}");
            assert!(!next.ends_with('0'));
            if needs_rebalance(&next) {
                append_rebalanced = true;
            }
            tail = next;
        }
        assert!(append_rebalanced);

        let mut head = first();
        let mut prepend_rebalanced = false;
        for _ in 0..200 {
            let prev = before(&head).unwrap();
            assert!(prev < head, "expected {prev} < {head}");
            assert!(!prev.ends_with('0'));
            if needs_rebalance(&prev) {
                prepend_rebalanced = true;
            }
            head = prev;
        }
        assert!(prepend_rebalanced);
    }

    #[test]
    fn rebalance_keys_produces_strictly_ordered_compact_keys() {
        assert!(rebalance_keys(0).is_empty());

        for count in [1usize, 2, 10, 100, 1500] {
            let keys = rebalance_keys(count);
            assert_eq!(keys.len(), count);
            let expected_len = if count <= 1000 { 2 } else { 3 };

            for key in &keys {
                assert_eq!(key.len(), expected_len);
                assert!(!key.ends_with('0'));
            }

            for pair in keys.windows(2) {
                assert!(pair[0] < pair[1], "expected {} < {}", pair[0], pair[1]);
                let mid = between(&pair[0], &pair[1]).unwrap();
                assert!(pair[0] < mid && mid < pair[1]);
            }
        }
    }
}
