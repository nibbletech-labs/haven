//! Fractional indexing for intra-band ordering (`sort_key`, SPEC §0 Q2).
//!
//! A `sort_key` is a base-36 digit string interpreted as the fraction `0.d1d2…`.
//! `between(lo, hi)` mints a key strictly between two neighbours (either bound
//! optional), so `rank --before/--after` only ever writes one row — no global
//! renumbering. Keys are kept canonical (no trailing `0`), which guarantees the
//! DB's lexicographic `ORDER BY sort_key` matches the intended numeric order.
//!
//! Implementation: the canonical recursive digit-wise midpoint (as used by the
//! `fractional-indexing` family). It strips the common prefix, then resolves the
//! first differing digit — either by picking a digit strictly between the two,
//! or by recursing into the lower bound's tail. It works for arbitrary key
//! lengths with O(length) work and no big-integer limits. Keys keep the
//! invariant "no trailing `0`", which is what makes the DB's lexicographic
//! `ORDER BY sort_key` equal the intended numeric order.

use crate::error::{HavenError, Result};

const BASE: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
/// Middle digit ('i') — the default first key when there are no neighbours.
const MID: u8 = BASE[18];

fn digit_value(c: u8) -> usize {
    // BASE is contiguous 0-9 then a-z.
    match c {
        b'0'..=b'9' => (c - b'0') as usize,
        b'a'..=b'z' => (c - b'a' + 10) as usize,
        _ => 0,
    }
}

/// A key strictly between `a` and `b`. `a` is the lower bound ("" means the
/// smallest possible key); `b = None` means unbounded above. Invariants on
/// entry: `a` and `b` carry no trailing `'0'`, and `a < b` when `b` is `Some`.
/// The returned key preserves the no-trailing-`'0'` invariant.
fn midpoint(a: &str, b: Option<&str>) -> String {
    if let Some(b) = b {
        // Strip the longest common prefix (treating `a` as zero-padded past its
        // end). The differing tail is what we actually bisect. `b` has no
        // trailing '0', so the loop can never consume all of `b`.
        let ab = a.as_bytes();
        let bb = b.as_bytes();
        let mut n = 0;
        while n < bb.len() && *ab.get(n).unwrap_or(&b'0') == bb[n] {
            n += 1;
        }
        if n > 0 {
            let a_tail = &a[n.min(a.len())..];
            let rest = midpoint(a_tail, Some(&b[n..]));
            return format!("{}{}", &b[..n], rest);
        }
    }

    // First digits differ (or `a` is empty / `b` is unbounded).
    let digit_a = a.as_bytes().first().map(|&c| digit_value(c)).unwrap_or(0);
    let digit_b = match b {
        Some(b) => digit_value(b.as_bytes()[0]),
        None => BASE.len(),
    };

    if digit_b - digit_a > 1 {
        // Room for a digit strictly between — pick the (rounded) midpoint digit.
        let mid = (digit_a + digit_b).div_ceil(2);
        (BASE[mid] as char).to_string()
    } else {
        // Adjacent digits: borrow `b`'s first digit if it has more precision,
        // else descend into `a`'s tail with no upper bound.
        match b {
            Some(b) if b.len() > 1 => b[..1].to_string(),
            _ => {
                let head = BASE[digit_a] as char;
                let a_tail = if a.is_empty() { "" } else { &a[1..] };
                format!("{head}{}", midpoint(a_tail, None))
            }
        }
    }
}

/// Mint a `sort_key` strictly between two neighbours' keys. `None` bounds mean
/// unbounded (top of list for `lo = None`, bottom for `hi = None`).
pub fn between(lo: Option<&str>, hi: Option<&str>) -> Result<String> {
    match (lo, hi) {
        (None, None) => Ok((MID as char).to_string()),
        (Some(a), None) => Ok(midpoint(a, None)),
        (None, Some(b)) => Ok(midpoint("", Some(b))),
        (Some(a), Some(b)) => {
            if a >= b {
                return Err(HavenError::GraphRule(format!(
                    "rank bounds out of order: {a:?} is not before {b:?}"
                )));
            }
            Ok(midpoint(a, Some(b)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_key_is_middle() {
        assert_eq!(between(None, None).unwrap(), "i");
    }

    #[test]
    fn between_is_strictly_ordered() {
        let a = between(None, None).unwrap();
        let before = between(None, Some(&a)).unwrap();
        let after = between(Some(&a), None).unwrap();
        assert!(before < a, "{before} < {a}");
        assert!(a < after, "{a} < {after}");

        let mid = between(Some(&before), Some(&a)).unwrap();
        assert!(before < mid && mid < a, "{before} < {mid} < {a}");
    }

    #[test]
    fn repeated_bisection_stays_ordered() {
        // Insert repeatedly between the first two keys; order must always hold.
        let mut lo = between(None, None).unwrap();
        let hi = between(Some(&lo), None).unwrap();
        let mut prev = lo.clone();
        for _ in 0..50 {
            let k = between(Some(&lo), Some(&hi)).unwrap();
            assert!(lo < k && k < hi, "invariant broke: {lo} < {k} < {hi}");
            assert!(k != prev);
            prev = k.clone();
            lo = k; // keep squeezing toward hi from the left
        }
    }

    #[test]
    fn appending_sequence_is_increasing() {
        // Simulate "rank --after last" repeatedly.
        let mut keys = vec![between(None, None).unwrap()];
        for _ in 0..30 {
            let last = keys.last().unwrap().clone();
            let next = between(Some(&last), None).unwrap();
            assert!(next > last, "{next} > {last}");
            keys.push(next);
        }
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "append order must equal lexical order");
    }

    #[test]
    fn prepending_sequence_is_decreasing() {
        let mut first = between(None, None).unwrap();
        for _ in 0..30 {
            let next = between(None, Some(&first)).unwrap();
            assert!(next < first, "{next} < {first}");
            first = next;
        }
    }

    #[test]
    fn out_of_order_bounds_error() {
        assert!(between(Some("z"), Some("a")).is_err());
        assert!(between(Some("a"), Some("a")).is_err());
    }

    #[test]
    fn lexical_equals_intended_order_under_mixed_ops() {
        // Build a list with a mix of before/after/between ops, then assert the
        // stored keys sort into the list order.
        let mut list: Vec<String> = vec![between(None, None).unwrap()];
        // append three
        for _ in 0..3 {
            let last = list.last().unwrap().clone();
            list.push(between(Some(&last), None).unwrap());
        }
        // insert between index 1 and 2 a few times
        for _ in 0..5 {
            let lo = list[1].clone();
            let hi = list[2].clone();
            let k = between(Some(&lo), Some(&hi)).unwrap();
            list.insert(2, k);
        }
        let mut sorted = list.clone();
        sorted.sort();
        assert_eq!(list, sorted);
        // all unique
        let mut dedup = list.clone();
        dedup.dedup();
        assert_eq!(dedup.len(), list.len());
    }
}
