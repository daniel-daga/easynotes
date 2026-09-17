//! Fractional indices for checklist item order (decision D3).
//!
//! An item's position is a short digit string stored as an ordinary field on the
//! item. Moving an item is a *write to one field*, not a delete plus an insert, so
//! two people reordering at the same time can never duplicate or lose an item —
//! the worst case is that one person's placement wins.
//!
//! The alphabet is ordered by ASCII value, so byte-wise string comparison is
//! numeric comparison of the underlying fraction.
//!
//! Invariant, relied on by `midpoint`: **a key never ends in the smallest digit.**
//! Without it there is no room left below a key and the algorithm cannot subdivide.

const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn digit_index(c: u8) -> usize {
    DIGITS
        .iter()
        .position(|&d| d == c)
        .expect("order keys only ever contain alphabet digits")
}

/// A key strictly between `a` and `b`, where `a` is treated as 0 when empty and
/// `b` as 1 when `None`. Requires `a < b`.
fn midpoint(a: &str, b: Option<&str>) -> String {
    if let Some(b) = b {
        debug_assert!(a < b, "midpoint requires a < b, got {a:?} and {b:?}");
        // Keep any shared prefix and subdivide what is left of it.
        let ab = a.as_bytes();
        let bb = b.as_bytes();
        let mut n = 0;
        while n < bb.len() && ab.get(n).copied().unwrap_or(DIGITS[0]) == bb[n] {
            n += 1;
        }
        if n > 0 {
            let tail_a = if n < ab.len() { &a[n..] } else { "" };
            return format!("{}{}", &b[..n], midpoint(tail_a, Some(&b[n..])));
        }
    }

    let digit_a = if a.is_empty() {
        0
    } else {
        digit_index(a.as_bytes()[0])
    };
    let digit_b = match b {
        Some(b) => digit_index(b.as_bytes()[0]),
        None => DIGITS.len(),
    };

    if digit_b - digit_a > 1 {
        // There is a spare digit between them; one character is enough.
        let mid = (digit_a + digit_b) / 2;
        return (DIGITS[mid] as char).to_string();
    }

    // The two leading digits are adjacent, so we have to go one place deeper.
    match b {
        Some(b) if b.len() > 1 => b[..1].to_string(),
        _ => format!(
            "{}{}",
            DIGITS[digit_a] as char,
            midpoint(if a.is_empty() { "" } else { &a[1..] }, None)
        ),
    }
}

/// The first key in an empty list.
pub fn first_key() -> String {
    midpoint("", None)
}

/// A key that sorts before every existing key.
pub fn key_before(next: &str) -> String {
    midpoint("", Some(next))
}

/// A key that sorts after every existing key.
pub fn key_after(prev: &str) -> String {
    midpoint(prev, None)
}

/// A key strictly between two neighbours.
pub fn key_between(prev: &str, next: &str) -> String {
    midpoint(prev, Some(next))
}

/// The key for the slot at `index` in a list whose current keys are `keys`
/// (which must already be sorted). `index == keys.len()` means "append".
pub fn key_for_index(keys: &[String], index: usize) -> String {
    match (index == 0, index >= keys.len()) {
        (true, true) => first_key(),
        (true, false) => key_before(&keys[0]),
        (false, true) => key_after(&keys[keys.len() - 1]),
        (false, false) => key_between(&keys[index - 1], &keys[index]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appending_produces_increasing_keys() {
        let mut keys: Vec<String> = Vec::new();
        for _ in 0..500 {
            let k = key_for_index(&keys, keys.len());
            if let Some(last) = keys.last() {
                assert!(last < &k, "{last} should sort before {k}");
            }
            keys.push(k);
        }
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn prepending_produces_decreasing_keys() {
        let mut keys: Vec<String> = vec![first_key()];
        for _ in 0..500 {
            let k = key_for_index(&keys, 0);
            assert!(k < keys[0], "{k} should sort before {}", keys[0]);
            keys.insert(0, k);
        }
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn repeatedly_splitting_the_same_gap_stays_ordered() {
        // The pathological case: always insert into the same position.
        let mut keys = vec![first_key()];
        keys.push(key_after(&keys[0]));
        for _ in 0..500 {
            let k = key_between(&keys[0], &keys[1]);
            assert!(
                keys[0] < k && k < keys[1],
                "{} < {k} < {}",
                keys[0],
                keys[1]
            );
            keys.insert(1, k);
            keys.truncate(3);
            keys = vec![keys[0].clone(), keys[1].clone()];
        }
    }

    #[test]
    fn inserting_at_every_position_keeps_the_list_sorted() {
        let mut keys: Vec<String> = Vec::new();
        for i in 0..200usize {
            let at = if keys.is_empty() {
                0
            } else {
                (i * 7) % (keys.len() + 1)
            };
            let k = key_for_index(&keys, at);
            keys.insert(at, k);
            let mut sorted = keys.clone();
            sorted.sort();
            assert_eq!(
                keys, sorted,
                "list stopped being sorted after inserting at {at}"
            );
        }
    }

    #[test]
    fn keys_never_end_in_the_smallest_digit() {
        // This is the invariant that keeps subdivision possible forever.
        let mut keys = vec![first_key()];
        for i in 0..200 {
            let k = if i % 2 == 0 {
                key_after(keys.last().unwrap())
            } else {
                key_before(&keys[0])
            };
            assert_ne!(
                k.as_bytes().last(),
                Some(&DIGITS[0]),
                "key {k} ends in the smallest digit"
            );
            if i % 2 == 0 {
                keys.push(k);
            } else {
                keys.insert(0, k);
            }
        }
    }
}
