//! Exact port of the parts of CPython's `difflib` the DCGM adapter uses (`get_close_matches`, built on
//! `SequenceMatcher` with `isjunk=None, autojunk=True`). PORTING.md risk R-4: no crate reproduces Python's
//! tie-breaking, so the algorithm is ported line for line (`find_longest_match`, `get_matching_blocks`, the three
//! ratios). Sequences are compared per code point, like Python strings.
//!
//! OWNER: workstream B. Pinned by tests/fixtures/dcgm/difflib_golden.json (generated with CPython).

use std::collections::HashMap;

/// `difflib.SequenceMatcher(None, a, b)` over two strings (code points).
pub struct SequenceMatcher {
    a: Vec<char>,
    b: Vec<char>,
    b2j: HashMap<char, Vec<usize>>,
    fullbcount: Option<HashMap<char, usize>>,
}

impl SequenceMatcher {
    #[must_use]
    pub fn new(a: &str, b: &str) -> Self {
        let mut m = SequenceMatcher {
            a: a.chars().collect(),
            b: Vec::new(),
            b2j: HashMap::new(),
            fullbcount: None,
        };
        m.set_seq2(b);
        m
    }

    pub fn set_seq1(&mut self, a: &str) {
        self.a = a.chars().collect();
    }

    pub fn set_seq2(&mut self, b: &str) {
        self.b = b.chars().collect();
        self.fullbcount = None;
        self.chain_b();
    }

    /// `__chain_b`: index of every element of b; with autojunk, "popular" elements of a long b are dropped.
    fn chain_b(&mut self) {
        let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
        for (i, elt) in self.b.iter().enumerate() {
            b2j.entry(*elt).or_default().push(i);
        }
        let n = self.b.len();
        if n >= 200 {
            let ntest = n / 100 + 1;
            let popular: Vec<char> = b2j
                .iter()
                .filter(|(_, idxs)| idxs.len() > ntest)
                .map(|(e, _)| *e)
                .collect();
            for elt in popular {
                b2j.remove(&elt);
            }
        }
        self.b2j = b2j;
    }

    /// `find_longest_match(alo, ahi, blo, bhi)` -> (i, j, size). No junk set (isjunk is None).
    fn find_longest_match(
        &self,
        alo: usize,
        ahi: usize,
        blo: usize,
        bhi: usize,
    ) -> (usize, usize, usize) {
        let (a, b) = (&self.a, &self.b);
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for (i, ai) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(js) = self.b2j.get(ai) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let prev = if j == 0 {
                        0
                    } else {
                        j2len.get(&(j - 1)).copied().unwrap_or(0)
                    };
                    let k = prev + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }
        // Extend over equal (popular) elements on both sides; the junk-side loops are no-ops without a junk set.
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && a[besti + bestsize] == b[bestj + bestsize]
        {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    }

    /// Sum of the sizes of `get_matching_blocks()` (the collapse of adjacent blocks does not change it).
    fn matches(&self) -> usize {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0usize, la, 0usize, lb)];
        let mut total = 0usize;
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let (i, j, k) = self.find_longest_match(alo, ahi, blo, bhi);
            if k > 0 {
                total += k;
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        total
    }

    fn calc(matches: usize, length: usize) -> f64 {
        if length == 0 {
            return 1.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let r = 2.0 * matches as f64 / length as f64;
        r
    }

    #[must_use]
    pub fn ratio(&self) -> f64 {
        Self::calc(self.matches(), self.a.len() + self.b.len())
    }

    pub fn quick_ratio(&mut self) -> f64 {
        let full = self.fullbcount.get_or_insert_with(|| {
            let mut m: HashMap<char, usize> = HashMap::new();
            for elt in &self.b {
                *m.entry(*elt).or_insert(0) += 1;
            }
            m
        });
        let mut avail: HashMap<char, i64> = HashMap::new();
        let mut matches = 0usize;
        for elt in &self.a {
            let numb = match avail.get(elt) {
                Some(n) => *n,
                None => i64::try_from(full.get(elt).copied().unwrap_or(0)).unwrap_or(i64::MAX),
            };
            avail.insert(*elt, numb - 1);
            if numb > 0 {
                matches += 1;
            }
        }
        Self::calc(matches, self.a.len() + self.b.len())
    }

    #[must_use]
    pub fn real_quick_ratio(&self) -> f64 {
        let (la, lb) = (self.a.len(), self.b.len());
        Self::calc(la.min(lb), la + lb)
    }
}

/// `difflib.get_close_matches(word, possibilities, n, cutoff)`: best matches first; ties broken like
/// `heapq.nlargest` on `(score, string)` tuples (the larger string wins).
#[must_use]
pub fn get_close_matches(word: &str, possibilities: &[&str], n: usize, cutoff: f64) -> Vec<String> {
    if n == 0 || !(0.0..=1.0).contains(&cutoff) {
        return Vec::new();
    }
    let mut s = SequenceMatcher::new("", word);
    let mut result: Vec<(f64, &str)> = Vec::new();
    for x in possibilities {
        s.set_seq1(x);
        if s.real_quick_ratio() >= cutoff && s.quick_ratio() >= cutoff && s.ratio() >= cutoff {
            result.push((s.ratio(), x));
        }
    }
    // heapq.nlargest(n, result): descending by (score, x); stable for equal tuples
    result.sort_by(|p, q| q.0.total_cmp(&p.0).then_with(|| q.1.cmp(p.1)));
    result
        .into_iter()
        .take(n)
        .map(|(_, x)| x.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_cpython_answers() {
        // CPython 3.13: difflib.get_close_matches("appel", ["ape", "apple", "peach", "puppy"]) == ['apple', 'ape']
        assert_eq!(
            get_close_matches("appel", &["ape", "apple", "peach", "puppy"], 3, 0.6),
            vec!["apple", "ape"]
        );
        assert!((SequenceMatcher::new("abcd", "bcde").ratio() - 0.75).abs() < 1e-12);
        assert_eq!(
            get_close_matches(
                "DCGM_FI_DEV_GPU_TMP",
                &["DCGM_FI_DEV_GPU_TEMP", "DCGM_FI_DEV_GPU_UTIL"],
                1,
                0.85
            ),
            vec!["DCGM_FI_DEV_GPU_TEMP"]
        );
    }
}
