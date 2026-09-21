//! Replaying one person's edit to a body onto a body that has moved on.
//!
//! A screen sends the body it was showing and the body it shows now: "this
//! person changed it from `base` to `text`". By the time that arrives the core's
//! body may no longer be `base`, because a peer's edit landed in between — which
//! in a note two people are typing into is not an edge case but most keystrokes.
//! Writing `text` over the body would then quietly delete the peer's words, and
//! it would do it through the CRDT, so every device would agree they were gone.
//!
//! [`rebase`] instead works out what this person did — one splice from `base` to
//! `text` — and replays it where it now belongs in the current body. Where it
//! belongs is found by diffing `base` against the current body, so the peer's
//! changes can be anywhere, and as many as they like, and survive. The one case
//! that loses anything is the honest conflict: this person replaced characters
//! the peer was changing at the same moment, and then this person's replacement
//! wins over exactly those characters.

/// The body that results from replaying the edit `base` → `text` onto
/// `current`.
pub fn rebase(base: &str, text: &str, current: &str) -> String {
    if base == current {
        return text.to_owned();
    }
    let base: Vec<char> = base.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let current: Vec<char> = current.chars().collect();

    let (at, end, inserted) = splice(&base, &text);
    let Some(map) = Map::between(&base, &current) else {
        // The body changed too much underneath this edit to say where it
        // belongs. Rare — it takes a peer rewriting hundreds of characters
        // between two keystrokes — and the only answer that keeps this person's
        // text is to write it.
        return text.into_iter().collect();
    };
    let from = map.after[at];
    let to = map.before[end].max(from);
    current[..from]
        .iter()
        .chain(inserted)
        .chain(&current[to..])
        .collect()
}

/// The one splice that turns `a` into `b`, in characters: the range of `a` it
/// replaces, and what replaces it.
fn splice<'a>(a: &[char], b: &'a [char]) -> (usize, usize, &'a [char]) {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    (prefix, a.len() - suffix, &b[prefix..b.len() - suffix])
}

/// Where each boundary between two characters of `base` ended up in `current`.
///
/// Two answers per boundary, because a peer may have inserted text exactly
/// there: `before[i]` is the position ahead of that insertion and `after[i]` the
/// one behind it. An edit starts after and ends before, so it never swallows an
/// insertion that only touches it.
struct Map {
    before: Vec<usize>,
    after: Vec<usize>,
}

enum Op {
    Equal,
    Delete,
    Insert,
}

/// How far apart two bodies may be before [`rebase`] stops trying. The diff's
/// cost grows with the square of the distance, and a distance this large between
/// two keystrokes is a paste or a rewrite, not typing.
const MAX_DISTANCE: usize = 512;

impl Map {
    fn between(base: &[char], current: &[char]) -> Option<Self> {
        let ops = diff(base, current)?;
        let mut before = vec![0; base.len() + 1];
        let mut after = vec![0; base.len() + 1];
        let (mut b, mut c) = (0, 0);
        for op in ops {
            match op {
                Op::Insert => c += 1,
                Op::Equal | Op::Delete => {
                    after[b] = c;
                    b += 1;
                    if matches!(op, Op::Equal) {
                        c += 1;
                    }
                    before[b] = c;
                }
            }
        }
        after[base.len()] = c;
        Some(Self { before, after })
    }
}

/// A shortest edit script from `a` to `b`, one op per character: Myers's
/// algorithm, after trimming what the two share at either end.
///
/// `None` past [`MAX_DISTANCE`]. Linear in the length of the text times the
/// number of characters that differ, which for typing is a handful.
fn diff(a: &[char], b: &[char]) -> Option<Vec<Op>> {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (ma, mb) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);

    let mut ops: Vec<Op> = (0..prefix).map(|_| Op::Equal).collect();
    ops.extend(myers(ma, mb)?);
    ops.extend((0..suffix).map(|_| Op::Equal));
    Some(ops)
}

fn myers(a: &[char], b: &[char]) -> Option<Vec<Op>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    if max == 0 {
        return Some(Vec::new());
    }
    let offset = max as isize + 1;
    let at = |k: isize| (k + offset) as usize;
    let mut v = vec![0isize; 2 * max + 3];
    // `trace[d]` is `v` as it stood when round `d` began, kept only over the
    // diagonals that round can read, so the whole trace is quadratic in the
    // distance rather than in the length of the text.
    let mut trace: Vec<Vec<isize>> = Vec::new();

    for d in 0..=(max as isize) {
        if d as usize > MAX_DISTANCE {
            return None;
        }
        trace.push(v[at(-d - 1)..=at(d + 1)].to_vec());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m));
            }
            k += 2;
        }
    }
    unreachable!("a script of length n + m always exists")
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize) -> Vec<Op> {
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for (d, v) in trace.iter().enumerate().rev() {
        let d = d as isize;
        // This round's slice starts at diagonal `-d - 1`.
        let get = |k: isize| v[(k + d + 1) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && get(k - 1) < get(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = get(prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops.push(Op::Equal);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            ops.push(if x == prev_x { Op::Insert } else { Op::Delete });
        }
        x = prev_x;
        y = prev_y;
    }
    ops.reverse();
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_nothing_underneath_it_is_the_edit() {
        assert_eq!(rebase("milk", "milk and eggs", "milk"), "milk and eggs");
    }

    #[test]
    fn a_peer_typing_before_the_edit_is_kept() {
        // This person added "and eggs"; the peer had meanwhile added "oat ".
        assert_eq!(
            rebase("milk", "milk and eggs", "oat milk"),
            "oat milk and eggs"
        );
    }

    #[test]
    fn a_peer_typing_after_the_edit_is_kept() {
        assert_eq!(
            rebase("milk bread", "milk, bread", "milk bread, eggs"),
            "milk, bread, eggs"
        );
    }

    #[test]
    fn a_peer_typing_on_both_sides_of_the_edit_is_kept() {
        // One splice from base to current would span the whole line and put
        // this person's comma inside it; the diff knows better.
        assert_eq!(
            rebase(
                "milk bread eggs",
                "milk bread, eggs",
                "oat milk bread eggs!"
            ),
            "oat milk bread, eggs!"
        );
    }

    #[test]
    fn a_deletion_keeps_an_insertion_at_its_edge() {
        // This person deleted "bread "; the peer typed "rye " right in front of
        // where it was. The peer's word touches the deletion but is not in it.
        assert_eq!(
            rebase("milk bread eggs", "milk eggs", "milk rye bread eggs"),
            "milk rye eggs"
        );
    }

    #[test]
    fn a_peers_deletion_elsewhere_stays_deleted() {
        assert_eq!(
            rebase("milk bread eggs", "milk bread eggs!", "milk eggs"),
            "milk eggs!"
        );
    }

    #[test]
    fn replacing_what_the_peer_was_changing_wins_over_just_that() {
        // Both changed "bread" at once. This person's replacement takes the
        // characters both touched, and nothing else.
        assert_eq!(
            rebase("milk bread eggs", "milk toast eggs", "tea, milk bead eggs"),
            "tea, milk toast eggs"
        );
    }

    #[test]
    fn characters_outside_ascii_are_whole() {
        assert_eq!(
            rebase("Grüße 👋", "Grüße 👋👋", "Liebe Grüße 👋"),
            "Liebe Grüße 👋👋"
        );
    }

    #[test]
    fn the_diff_is_a_real_edit_script() {
        let a: Vec<char> = "the quick brown fox".chars().collect();
        let b: Vec<char> = "a quick brown dog, the fox".chars().collect();
        let ops = diff(&a, &b).unwrap();
        let (mut i, mut j) = (0, 0);
        for op in &ops {
            match op {
                Op::Equal => {
                    assert_eq!(a[i], b[j]);
                    i += 1;
                    j += 1;
                }
                Op::Delete => i += 1,
                Op::Insert => j += 1,
            }
        }
        assert_eq!((i, j), (a.len(), b.len()));
    }

    #[test]
    fn too_far_apart_writes_the_edit() {
        let base = "x".repeat(10);
        let current = "y".repeat(2 * MAX_DISTANCE);
        assert_eq!(rebase(&base, "xx", &current), "xx");
    }
}
