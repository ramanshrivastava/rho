//! A faithful port of the slice of CPython's `difflib` that tau's `edit` tool
//! relies on: `SequenceMatcher`, `unified_diff`, and `ndiff` (the `Differ`
//! line/char differencer with `_fancy_replace`).
//!
//! The port is 1:1 with CPython 3.12's `difflib.py` so the `diff`/`patch`
//! strings in an `edit` tool result's `details` match tau byte-for-byte. Only
//! the pieces `generate_diff_string` / `generate_unified_patch` use are ported;
//! `autojunk` and the junk machinery are carried because `SequenceMatcher`'s
//! matching depends on them even for the no-junk line diff.
//!
//! `SequenceMatcher` is generic over a hashable element type so it serves both
//! the line-level diff (`elements = lines`) and the intraline char-level diff
//! (`elements = chars`, with `IS_CHARACTER_JUNK`).
//!
//! The index loops and terse `alo`/`ahi`/`blo`/`bhi` names mirror CPython's
//! `difflib` one-to-one (the port's whole point), so a few pedantic lints that
//! would push toward idiomatic-but-divergent shapes are allowed module-wide.

#![allow(
    clippy::similar_names,
    clippy::needless_range_loop,
    clippy::doc_markdown
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::Hash;

/// A maximal matching block `a[i..i+size] == b[j..j+size]` (tau `Match`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MatchBlock {
    a: usize,
    b: usize,
    size: usize,
}

/// One edit opcode `(tag, i1, i2, j1, j2)` (tau `get_opcodes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Opcode {
    tag: Tag,
    i1: usize,
    i2: usize,
    j1: usize,
    j2: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Replace,
    Delete,
    Insert,
    Equal,
}

/// A port of `difflib.SequenceMatcher` (no-`isjunk` and char-junk uses only).
struct SequenceMatcher<'j, T: Eq + Hash + Clone> {
    a: Vec<T>,
    b: Vec<T>,
    isjunk: Option<&'j dyn Fn(&T) -> bool>,
    autojunk: bool,
    b2j: HashMap<T, Vec<usize>>,
    bjunk: std::collections::HashSet<T>,
    fullbcount: Option<HashMap<T, usize>>,
}

impl<'j, T: Eq + Hash + Clone> SequenceMatcher<'j, T> {
    fn new(isjunk: Option<&'j dyn Fn(&T) -> bool>, a: Vec<T>, b: Vec<T>, autojunk: bool) -> Self {
        let mut m = Self {
            a,
            b,
            isjunk,
            autojunk,
            b2j: HashMap::new(),
            bjunk: std::collections::HashSet::new(),
            fullbcount: None,
        };
        m.chain_b();
        m
    }

    fn chain_b(&mut self) {
        let b = &self.b;
        let mut b2j: HashMap<T, Vec<usize>> = HashMap::new();
        for (i, elt) in b.iter().enumerate() {
            b2j.entry(elt.clone()).or_default().push(i);
        }

        // Purge junk elements.
        let mut junk = std::collections::HashSet::new();
        if let Some(isjunk) = self.isjunk {
            for elt in b2j.keys() {
                if isjunk(elt) {
                    junk.insert(elt.clone());
                }
            }
            for elt in &junk {
                b2j.remove(elt);
            }
        }

        // Purge popular elements that are not junk (autojunk, n >= 200).
        let n = b.len();
        if self.autojunk && n >= 200 {
            let ntest = n / 100 + 1;
            let popular: Vec<T> = b2j
                .iter()
                .filter(|(_, idxs)| idxs.len() > ntest)
                .map(|(elt, _)| elt.clone())
                .collect();
            for elt in popular {
                b2j.remove(&elt);
            }
        }

        self.b2j = b2j;
        self.bjunk = junk;
    }

    fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> MatchBlock {
        let a = &self.a;
        let b = &self.b;
        let b2j = &self.b2j;
        let isbjunk = |x: &T| self.bjunk.contains(x);

        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        let empty: Vec<usize> = Vec::new();

        for i in alo..ahi {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            let indices = b2j.get(&a[i]).unwrap_or(&empty);
            for &j in indices {
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
            j2len = newj2len;
        }

        // Extend by non-junk elements on each end.
        while besti > alo && bestj > blo && !isbjunk(&b[bestj - 1]) && a[besti - 1] == b[bestj - 1]
        {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && !isbjunk(&b[bestj + bestsize])
            && a[besti + bestsize] == b[bestj + bestsize]
        {
            bestsize += 1;
        }

        // Suck up matching junk on each side.
        while besti > alo && bestj > blo && isbjunk(&b[bestj - 1]) && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && isbjunk(&b[bestj + bestsize])
            && a[besti + bestsize] == b[bestj + bestsize]
        {
            bestsize += 1;
        }

        MatchBlock {
            a: besti,
            b: bestj,
            size: bestsize,
        }
    }

    fn get_matching_blocks(&self) -> Vec<MatchBlock> {
        let la = self.a.len();
        let lb = self.b.len();
        let mut queue = vec![(0usize, la, 0usize, lb)];
        let mut matching_blocks: Vec<MatchBlock> = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let m = self.find_longest_match(alo, ahi, blo, bhi);
            let (i, j, k) = (m.a, m.b, m.size);
            if k > 0 {
                matching_blocks.push(m);
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        matching_blocks.sort_by_key(|m| (m.a, m.b, m.size));

        // Collapse adjacent equal blocks.
        let (mut i1, mut j1, mut k1) = (0usize, 0usize, 0usize);
        let mut non_adjacent: Vec<MatchBlock> = Vec::new();
        for m in &matching_blocks {
            let (i2, j2, k2) = (m.a, m.b, m.size);
            if i1 + k1 == i2 && j1 + k1 == j2 {
                k1 += k2;
            } else {
                if k1 > 0 {
                    non_adjacent.push(MatchBlock {
                        a: i1,
                        b: j1,
                        size: k1,
                    });
                }
                i1 = i2;
                j1 = j2;
                k1 = k2;
            }
        }
        if k1 > 0 {
            non_adjacent.push(MatchBlock {
                a: i1,
                b: j1,
                size: k1,
            });
        }
        non_adjacent.push(MatchBlock {
            a: la,
            b: lb,
            size: 0,
        });
        non_adjacent
    }

    fn get_opcodes(&self) -> Vec<Opcode> {
        let (mut i, mut j) = (0usize, 0usize);
        let mut answer: Vec<Opcode> = Vec::new();
        for m in self.get_matching_blocks() {
            let (ai, bj, size) = (m.a, m.b, m.size);
            let tag = if i < ai && j < bj {
                Some(Tag::Replace)
            } else if i < ai {
                Some(Tag::Delete)
            } else if j < bj {
                Some(Tag::Insert)
            } else {
                None
            };
            if let Some(tag) = tag {
                answer.push(Opcode {
                    tag,
                    i1: i,
                    i2: ai,
                    j1: j,
                    j2: bj,
                });
            }
            i = ai + size;
            j = bj + size;
            if size > 0 {
                answer.push(Opcode {
                    tag: Tag::Equal,
                    i1: ai,
                    i2: i,
                    j1: bj,
                    j2: j,
                });
            }
        }
        answer
    }

    fn get_grouped_opcodes(&self, n: usize) -> Vec<Vec<Opcode>> {
        let mut codes = self.get_opcodes();
        if codes.is_empty() {
            codes = vec![Opcode {
                tag: Tag::Equal,
                i1: 0,
                i2: 1,
                j1: 0,
                j2: 1,
            }];
        }
        if codes[0].tag == Tag::Equal {
            let c = &mut codes[0];
            c.i1 = c.i1.max(c.i2.saturating_sub(n));
            c.j1 = c.j1.max(c.j2.saturating_sub(n));
        }
        let last = codes.len() - 1;
        if codes[last].tag == Tag::Equal {
            let c = &mut codes[last];
            c.i2 = c.i2.min(c.i1 + n);
            c.j2 = c.j2.min(c.j1 + n);
        }

        let nn = n + n;
        let mut groups: Vec<Vec<Opcode>> = Vec::new();
        let mut group: Vec<Opcode> = Vec::new();
        for mut code in codes {
            if code.tag == Tag::Equal && code.i2 - code.i1 > nn {
                group.push(Opcode {
                    tag: code.tag,
                    i1: code.i1,
                    i2: code.i2.min(code.i1 + n),
                    j1: code.j1,
                    j2: code.j2.min(code.j1 + n),
                });
                groups.push(std::mem::take(&mut group));
                code.i1 = code.i1.max(code.i2.saturating_sub(n));
                code.j1 = code.j1.max(code.j2.saturating_sub(n));
            }
            group.push(code);
        }
        if !(group.is_empty() || (group.len() == 1 && group[0].tag == Tag::Equal)) {
            groups.push(group);
        }
        groups
    }

    fn ratio(&self) -> f64 {
        let matches: usize = self.get_matching_blocks().iter().map(|m| m.size).sum();
        calculate_ratio(matches, self.a.len() + self.b.len())
    }

    fn quick_ratio(&mut self) -> f64 {
        if self.fullbcount.is_none() {
            let mut fullbcount: HashMap<T, usize> = HashMap::new();
            for elt in &self.b {
                *fullbcount.entry(elt.clone()).or_insert(0) += 1;
            }
            self.fullbcount = Some(fullbcount);
        }
        let fullbcount = self.fullbcount.as_ref().unwrap();
        let mut avail: HashMap<T, i64> = HashMap::new();
        let mut matches = 0usize;
        for elt in &self.a {
            let numb = if let Some(&v) = avail.get(elt) {
                v
            } else {
                i64::try_from(fullbcount.get(elt).copied().unwrap_or(0)).unwrap_or(i64::MAX)
            };
            avail.insert(elt.clone(), numb - 1);
            if numb > 0 {
                matches += 1;
            }
        }
        calculate_ratio(matches, self.a.len() + self.b.len())
    }

    fn real_quick_ratio(&self) -> f64 {
        let la = self.a.len();
        let lb = self.b.len();
        calculate_ratio(la.min(lb), la + lb)
    }
}

fn calculate_ratio(matches: usize, length: usize) -> f64 {
    if length > 0 {
        #[allow(clippy::cast_precision_loss)]
        {
            2.0 * matches as f64 / length as f64
        }
    } else {
        1.0
    }
}

// ---------------------------------------------------------------------------
// unified_diff (line-level)
// ---------------------------------------------------------------------------

fn format_range_unified(start: usize, stop: usize) -> String {
    let beginning = start + 1;
    let length = stop - start;
    if length == 1 {
        return format!("{beginning}");
    }
    if length == 0 {
        return format!("{},{}", beginning - 1, length);
    }
    format!("{beginning},{length}")
}

/// Port of `difflib.unified_diff` for `fromfile == tofile`, default `n = 3`,
/// `lineterm = "\n"`, no dates. `a`/`b` are lines (with keepends, as tau passes
/// `splitlines(keepends=True)`).
pub fn unified_diff(a: &[String], b: &[String], fromfile: &str, tofile: &str) -> String {
    let matcher = SequenceMatcher::new(
        None::<&dyn Fn(&String) -> bool>,
        a.to_vec(),
        b.to_vec(),
        true,
    );
    let mut out = String::new();
    let mut started = false;
    for group in matcher.get_grouped_opcodes(3) {
        if !started {
            started = true;
            let _ = writeln!(out, "--- {fromfile}");
            let _ = writeln!(out, "+++ {tofile}");
        }
        let first = group[0];
        let last = group[group.len() - 1];
        let file1_range = format_range_unified(first.i1, last.i2);
        let file2_range = format_range_unified(first.j1, last.j2);
        let _ = writeln!(out, "@@ -{file1_range} +{file2_range} @@");
        for code in group {
            match code.tag {
                Tag::Equal => {
                    for line in &a[code.i1..code.i2] {
                        out.push(' ');
                        out.push_str(line);
                    }
                }
                Tag::Replace | Tag::Delete => {
                    for line in &a[code.i1..code.i2] {
                        out.push('-');
                        out.push_str(line);
                    }
                    if code.tag == Tag::Replace {
                        for line in &b[code.j1..code.j2] {
                            out.push('+');
                            out.push_str(line);
                        }
                    }
                }
                Tag::Insert => {
                    for line in &b[code.j1..code.j2] {
                        out.push('+');
                        out.push_str(line);
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// ndiff (Differ) — line-level with intraline char marking
// ---------------------------------------------------------------------------

// Signature is `&char` because it is passed as `&dyn Fn(&T) -> bool` with
// `T = char` (the char-junk `SequenceMatcher`); a by-value `char` would not fit.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_character_junk(ch: &char) -> bool {
    *ch == ' ' || *ch == '\t'
}

/// Port of `difflib.ndiff(a, b)` with default `linejunk=None`,
/// `charjunk=IS_CHARACTER_JUNK`. Returns the delta lines. As in CPython, `?`
/// hint lines carry a trailing `\n`; the caller joins with `\n`
/// (`generate_diff_string`).
pub fn ndiff(a: &[String], b: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let matcher = SequenceMatcher::new(
        None::<&dyn Fn(&String) -> bool>,
        a.to_vec(),
        b.to_vec(),
        true,
    );
    for code in matcher.get_opcodes() {
        match code.tag {
            Tag::Replace => fancy_replace(&mut out, a, code.i1, code.i2, b, code.j1, code.j2),
            Tag::Delete => dump(&mut out, '-', a, code.i1, code.i2),
            Tag::Insert => dump(&mut out, '+', b, code.j1, code.j2),
            Tag::Equal => dump(&mut out, ' ', a, code.i1, code.i2),
        }
    }
    out
}

fn dump(out: &mut Vec<String>, tag: char, x: &[String], lo: usize, hi: usize) {
    for line in &x[lo..hi] {
        out.push(format!("{tag} {line}"));
    }
}

fn plain_replace(
    out: &mut Vec<String>,
    a: &[String],
    alo: usize,
    ahi: usize,
    b: &[String],
    blo: usize,
    bhi: usize,
) {
    debug_assert!(alo < ahi && blo < bhi);
    if bhi - blo < ahi - alo {
        dump(out, '+', b, blo, bhi);
        dump(out, '-', a, alo, ahi);
    } else {
        dump(out, '-', a, alo, ahi);
        dump(out, '+', b, blo, bhi);
    }
}

#[allow(clippy::too_many_arguments)]
fn fancy_replace(
    out: &mut Vec<String>,
    a: &[String],
    alo: usize,
    ahi: usize,
    b: &[String],
    blo: usize,
    bhi: usize,
) {
    let mut best_ratio = 0.74_f64;
    let cutoff = 0.75_f64;
    let mut best_i = alo;
    let mut best_j = blo;
    let mut eqi: Option<usize> = None;
    let mut eqj: Option<usize> = None;

    for j in blo..bhi {
        let bj: Vec<char> = b[j].chars().collect();
        for i in alo..ahi {
            let ai: Vec<char> = a[i].chars().collect();
            if a[i] == b[j] {
                if eqi.is_none() {
                    eqi = Some(i);
                    eqj = Some(j);
                }
                continue;
            }
            let mut cruncher =
                SequenceMatcher::new(Some(&is_character_junk), ai.clone(), bj.clone(), true);
            if cruncher.real_quick_ratio() > best_ratio
                && cruncher.quick_ratio() > best_ratio
                && cruncher.ratio() > best_ratio
            {
                best_ratio = cruncher.ratio();
                best_i = i;
                best_j = j;
            }
        }
    }

    if best_ratio < cutoff {
        let Some(ei) = eqi else {
            plain_replace(out, a, alo, ahi, b, blo, bhi);
            return;
        };
        best_i = ei;
        best_j = eqj.expect("eqj set when eqi set");
        // best_ratio = 1.0 (unused past here beyond the eqi flag).
    } else {
        eqi = None;
    }

    // Diffs before the synch point.
    fancy_helper(out, a, alo, best_i, b, blo, best_j);

    let aelt = &a[best_i];
    let belt = &b[best_j];
    if eqi.is_none() {
        let a_chars: Vec<char> = aelt.chars().collect();
        let b_chars: Vec<char> = belt.chars().collect();
        let mut atags = String::new();
        let mut btags = String::new();
        let cruncher = SequenceMatcher::new(
            Some(&is_character_junk),
            a_chars.clone(),
            b_chars.clone(),
            true,
        );
        for code in cruncher.get_opcodes() {
            let la = code.i2 - code.i1;
            let lb = code.j2 - code.j1;
            match code.tag {
                Tag::Replace => {
                    atags.push_str(&"^".repeat(la));
                    btags.push_str(&"^".repeat(lb));
                }
                Tag::Delete => atags.push_str(&"-".repeat(la)),
                Tag::Insert => btags.push_str(&"+".repeat(lb)),
                Tag::Equal => {
                    atags.push_str(&" ".repeat(la));
                    btags.push_str(&" ".repeat(lb));
                }
            }
        }
        qformat(out, aelt, belt, &atags, &btags);
    } else {
        out.push(format!("  {aelt}"));
    }

    // Diffs after the synch point.
    fancy_helper(out, a, best_i + 1, ahi, b, best_j + 1, bhi);
}

#[allow(clippy::too_many_arguments)]
fn fancy_helper(
    out: &mut Vec<String>,
    a: &[String],
    alo: usize,
    ahi: usize,
    b: &[String],
    blo: usize,
    bhi: usize,
) {
    if alo < ahi {
        if blo < bhi {
            fancy_replace(out, a, alo, ahi, b, blo, bhi);
        } else {
            dump(out, '-', a, alo, ahi);
        }
    } else if blo < bhi {
        dump(out, '+', b, blo, bhi);
    }
}

/// Replace whitespace in a tag string with the original whitespace characters
/// (tau `_keep_original_ws`): `c if tag_c == " " and c.isspace() else tag_c`.
fn keep_original_ws(s: &str, tag_s: &str) -> String {
    s.chars()
        .zip(tag_s.chars())
        .map(|(c, tag_c)| {
            // tau: `c if tag_c == " " and c.isspace() else tag_c` — Python
            // `str.isspace`, which includes the C0 separators Rust's
            // `char::is_whitespace` omits.
            if tag_c == ' ' && crate::pystr::is_python_space(c) {
                c
            } else {
                tag_c
            }
        })
        .collect()
}

fn qformat(out: &mut Vec<String>, aline: &str, bline: &str, atags: &str, btags: &str) {
    // tau: `_keep_original_ws(...).rstrip()` — Python `str.rstrip()` whitespace.
    let atags = crate::pystr::py_rstrip(&keep_original_ws(aline, atags));
    let btags = crate::pystr::py_rstrip(&keep_original_ws(bline, btags));

    out.push(format!("- {aline}"));
    if !atags.is_empty() {
        out.push(format!("? {atags}\n"));
    }
    out.push(format!("+ {bline}"));
    if !btags.is_empty() {
        out.push(format!("? {btags}\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        // Python str.splitlines() (no keepends).
        splitlines(s, false)
    }

    fn keepends(s: &str) -> Vec<String> {
        splitlines(s, true)
    }

    // Local splitlines mirroring the edit_match helper (tested there too).
    fn splitlines(s: &str, keepends: bool) -> Vec<String> {
        super::super::edit_match::splitlines(s, keepends)
    }

    fn ndiff_join(o: &str, n: &str) -> String {
        ndiff(&lines(o), &lines(n)).join("\n")
    }

    #[test]
    fn ndiff_simple_replace() {
        assert_eq!(
            ndiff_join("alpha\nbeta\ngamma\n", "one\nbeta\nthree\n"),
            "- alpha\n+ one\n  beta\n- gamma\n+ three"
        );
    }

    #[test]
    fn ndiff_insert() {
        assert_eq!(ndiff_join("a\nb\n", "a\nX\nb\n"), "  a\n+ X\n  b");
    }

    #[test]
    fn ndiff_delete() {
        assert_eq!(ndiff_join("a\nb\nc\n", "a\nc\n"), "  a\n- b\n  c");
    }

    #[test]
    fn ndiff_fancy_replace_marks_intraline() {
        assert_eq!(
            ndiff_join("the quick brown fox\n", "the quick red fox\n"),
            "- the quick brown fox\n?           - ^^^\n\n+ the quick red fox\n?            ^^\n"
        );
    }

    #[test]
    fn unified_patch_simple() {
        assert_eq!(
            unified_diff(
                &keepends("alpha\nbeta\ngamma\n"),
                &keepends("one\nbeta\nthree\n"),
                "/f.txt",
                "/f.txt"
            ),
            "--- /f.txt\n+++ /f.txt\n@@ -1,3 +1,3 @@\n-alpha\n+one\n beta\n-gamma\n+three\n"
        );
    }

    #[test]
    fn unified_patch_insert() {
        assert_eq!(
            unified_diff(
                &keepends("a\nb\n"),
                &keepends("a\nX\nb\n"),
                "/f.txt",
                "/f.txt"
            ),
            "--- /f.txt\n+++ /f.txt\n@@ -1,2 +1,3 @@\n a\n+X\n b\n"
        );
    }

    #[test]
    fn unified_patch_single_line_range() {
        assert_eq!(
            unified_diff(
                &keepends("the quick brown fox\n"),
                &keepends("the quick red fox\n"),
                "/f.txt",
                "/f.txt"
            ),
            "--- /f.txt\n+++ /f.txt\n@@ -1 +1 @@\n-the quick brown fox\n+the quick red fox\n"
        );
    }
}
