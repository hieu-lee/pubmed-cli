use crate::args::Args;
use crate::model::*;
use anyhow::{Result, bail};
use regex::{Regex, RegexBuilder};
use std::collections::HashMap;
use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Pmid, Title, Abs, Concl, Bg, Methods, Results, Authors, First, Last, Aff, Journal, Jt, Q, Sjr,
    H, Year, Date, Epub, Doi, Pmc, Types, Mesh, Major, Kw, Lang, Status, Grants, Nct, Cites, Rcr,
    Pct, Apt, Clin, Comments, Problems, Links, Nrefs, Url, Vol, Pages, Country, Score, Snip, All,
}

use Field::*;

pub const REG: &[(Field, &str, &[&str], &str)] = &[
    (Pmid, "pmid", &["id"], "PubMed id"),
    (Title, "title", &["t", "ti"], "title"),
    (Abs, "abs", &["a", "ab", "abstract"], "abstract (labels BG/M/R/C, capped by --abs)"),
    (Concl, "concl", &["c", "conclusion", "conclusions"], "conclusions section, else last 2 sentences"),
    (Bg, "bg", &["background"], "background/introduction section"),
    (Methods, "methods", &["m", "method"], "methods section"),
    (Results, "results", &["r", "result"], "results/findings section"),
    (Authors, "authors", &["au", "author"], "A B, C D … Z Z (first N-1 + last; N=--authors, default 3; 1 = 'A B +K')"),
    (First, "first", &["fa"], "first author"),
    (Last, "last", &["la"], "last author"),
    (Aff, "aff", &["af", "affiliation"], "first author's affiliation, shortened"),
    (Journal, "journal", &["j", "ta"], "ISO journal abbreviation"),
    (Jt, "jt", &["journal_title"], "full journal title"),
    (Q, "q", &["quartile"], "SJR best quartile Q1-Q4, '-' if unranked"),
    (Sjr, "sjr", &[], "SJR score"),
    (H, "h", &["hindex"], "journal h-index"),
    (Year, "year", &["y"], "publication year"),
    (Date, "date", &["d"], "publication date"),
    (Epub, "epub", &[], "electronic publication date"),
    (Doi, "doi", &[], "DOI"),
    (Pmc, "pmc", &[], "PMC id"),
    (Types, "types", &["type", "pt"], "publication types (minus Journal Article)"),
    (Mesh, "mesh", &["mh"], "MeSH 'Name*/qualifier' ; sep, * = major"),
    (Major, "major", &["majr"], "major MeSH headings"),
    (Kw, "kw", &["keywords", "keyword"], "author keywords"),
    (Lang, "lang", &["l"], "languages"),
    (Status, "status", &[], "publication status"),
    (Grants, "grants", &["gr", "grant"], "grants 'id agency'"),
    (Nct, "nct", &[], "ClinicalTrials.gov ids"),
    (Cites, "cites", &["cited", "citations"], "citation count (iCite)"),
    (Rcr, "rcr", &[], "relative citation ratio (iCite)"),
    (Pct, "pct", &["percentile"], "NIH citation percentile (iCite)"),
    (Apt, "apt", &[], "approximate potential to translate (iCite)"),
    (Clin, "clin", &[], "1 if cited by clinical articles (iCite)"),
    (Comments, "comments", &["cm"], "comment articles 'in:PMID title | on:PMID title'"),
    (Problems, "problems", &["pr", "prob"], "retraction/erratum/EoC 'retracted:PMID | erratum:PMID | eoc:PMID'"),
    (Links, "links", &[], "DOI and PMC urls"),
    (Nrefs, "nrefs", &["refs"], "number of references"),
    (Url, "url", &[], "PubMed url"),
    (Vol, "vol", &[], "volume(issue)"),
    (Pages, "pages", &["pg"], "pages"),
    (Country, "country", &["cn"], "journal country"),
    (Score, "score", &[], "relevance/link score of the stage"),
    (Snip, "snip", &["sn"], "KWIC: fragments around query/--grep/--phrase/--fz matches (--snip N chars, --snips K); T: = title"),
    (All, "all", &[], "every non-empty field"),
];

const EXPANDED: &[Field] = &[
    Pmid, Title, Abs, Authors, Aff, Journal, Jt, Q, Sjr, H, Year, Date, Epub, Doi, Pmc, Types, Mesh, Kw,
    Lang, Status, Grants, Nct, Cites, Rcr, Pct, Apt, Clin, Comments, Problems, Links, Nrefs, Vol,
    Pages, Country, Score,
];

impl Field {
    pub fn name(self) -> &'static str {
        reg(self).1
    }

    pub fn is_long(self) -> bool {
        matches!(
            self,
            Abs | Concl | Bg | Methods | Results | Mesh | Major | Kw | Grants | Comments | Problems | Links | All
        )
    }

    pub fn is_abstract(self) -> bool {
        matches!(self, Abs | Concl | Bg | Methods | Results)
    }

    pub fn need(self) -> Need {
        let (rank, metrics, expand) = match self {
            Q | Sjr | H => (true, false, false),
            Cites | Rcr | Pct | Apt | Clin => (false, true, false),
            Comments | Problems => (false, false, true),
            All => (true, true, true),
            _ => (false, false, false),
        };
        Need { rank, metrics, expand }
    }

    fn json(self) -> Json {
        match self {
            Pmid | Year | H | Cites | Nrefs | Sjr | Rcr | Pct | Apt | Score => Json::Num,
            Q => Json::Q,
            Clin => Json::Bool,
            Types | Mesh | Major | Kw | Lang | Grants | Nct | Comments | Problems => Json::List,
            _ => Json::Str,
        }
    }

    fn sep(self) -> &'static str {
        match self {
            Types | Lang => ", ",
            Nct => " ",
            Comments | Problems => " | ",
            _ => "; ",
        }
    }
}

#[derive(PartialEq)]
enum Json {
    Str,
    Num,
    Bool,
    Q,
    List,
}

fn reg(f: Field) -> &'static (Field, &'static str, &'static [&'static str], &'static str) {
    REG.iter().find(|r| r.0 == f).expect("field registered")
}

pub fn resolve(name: &str) -> Option<Field> {
    REG.iter()
        .find(|r| r.1.eq_ignore_ascii_case(name) || r.2.iter().any(|a| a.eq_ignore_ascii_case(name)))
        .map(|r| r.0)
}

pub fn first_author(a: &Article) -> Option<&Author> {
    a.authors.iter().find(|x| !x.coll).or_else(|| a.authors.first())
}

pub fn last_author(a: &Article) -> Option<&Author> {
    a.authors.iter().rev().find(|x| !x.coll).or_else(|| a.authors.last())
}

pub fn is_pmid_only(names: &[String]) -> bool {
    !names.is_empty() && names.iter().all(|n| resolve(n) == Some(Pmid))
}

fn lev(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + (ca != *cb) as usize).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

pub fn unknown(name: &str) -> anyhow::Error {
    let low = name.to_ascii_lowercase();
    let best = REG
        .iter()
        .flat_map(|r| std::iter::once(r.1).chain(r.2.iter().copied()))
        .map(|n| (lev(&low, n) + if n.starts_with(&low) { 0 } else { 1 }, n))
        .min_by_key(|x| x.0);
    match best {
        Some((d, n)) if d <= 3 && d * 2 <= low.len() + 1 => {
            anyhow::anyhow!("unknown field '{name}', did you mean '{}'?", resolve(n).map_or(n, Field::name))
        }
        _ => {
            let names: Vec<&str> = REG.iter().map(|r| r.1).collect();
            anyhow::anyhow!("unknown field '{name}'; fields: {}", names.join(" "))
        }
    }
}

pub fn parse(names: &[String]) -> Result<Vec<Field>> {
    let mut out: Vec<Field> = Vec::with_capacity(names.len());
    for n in names {
        let Some(f) = resolve(n) else { return Err(unknown(n)) };
        let list: &[Field] = if f == All { EXPANDED } else { &[f] };
        for &f in list {
            if !out.contains(&f) {
                out.push(f);
            }
        }
    }
    if out.is_empty() {
        bail!("no fields");
    }
    Ok(out)
}

pub fn need_for(set: &Set, a: &Args) -> Need {
    choose(set, a).map_or_else(|_| Need::default(), |(fl, _)| fl.into_iter().fold(Need::default(), |n, f| n.or(f.need())))
}

const DEFAULT_NAMES: [&str; 5] = ["year", "journal", "q", "authors", "title"];

pub fn default_fields(set: &Set) -> Vec<String> {
    let mut v = vec!["pmid"];
    if !set.scores.is_empty() {
        v.push("score");
    }
    v.extend(DEFAULT_NAMES);
    v.into_iter().map(String::from).collect()
}

pub fn choose(set: &Set, a: &Args) -> Result<(Vec<Field>, bool)> {
    let mut raw = a.csv("fields");
    if raw.is_empty() {
        if a.flags.iter().any(|f| f == "fields") {
            bail!("-f needs a value; remove from default with -f=-q,-authors");
        }
        raw = set.fields.clone().unwrap_or_default();
    }
    if raw.is_empty() {
        return Ok((parse(&default_fields(set))?, true));
    }
    if !raw.iter().any(|f| f.starts_with(['+', '-', '~'])) {
        return Ok((parse(&raw)?, false));
    }
    let mut names = set.fields.clone().filter(|f| !f.is_empty()).unwrap_or_else(|| default_fields(set));
    let mut minus = Vec::new();
    for t in raw {
        match t.strip_prefix(['-', '~']) {
            Some(m) if !m.is_empty() => minus.push(m.to_string()),
            Some(_) => {}
            None => names.push(t.trim_start_matches('+').to_string()),
        }
    }
    names.retain(|n| !n.is_empty());
    let mut fl = parse(&names)?;
    for m in &minus {
        let Some(f) = resolve(m) else { return Err(unknown(m)) };
        fl.retain(|x| *x != f);
    }
    if fl.is_empty() {
        bail!("all fields removed");
    }
    Ok((fl, true))
}

pub struct Opts<'a> {
    pub authors: usize,
    pub abs: usize,
    pub snip: Option<Kwic>,
    pub scores: Option<&'a HashMap<u32, f32>>,
}

impl<'a> Opts<'a> {
    pub fn new(set: &'a Set, a: &Args) -> Result<Opts<'a>> {
        let default_abs = if set.arts.len() == 1 || set.notes.iter().any(|n| n == "full") { 0 } else { 500 };
        Ok(Opts {
            authors: a.num("authors")?.unwrap_or(3),
            abs: a.num("abs")?.unwrap_or(default_abs),
            snip: None,
            scores: (!set.scores.is_empty()).then_some(&set.scores),
        })
    }
}

pub fn number(a: &Article, f: Field) -> Option<f64> {
    let m = a.m.as_ref();
    match f {
        Pmid => Some(a.pmid as f64),
        Year => (a.year > 0).then_some(a.year as f64),
        Q => a.jr.as_ref().filter(|j| j.q > 0).map(|j| j.q as f64),
        Sjr => a.jr.as_ref().map(|j| j.sjr as f64),
        H => a.jr.as_ref().map(|j| j.h as f64),
        Cites => m.map(|m| m.cites as f64),
        Rcr => m.and_then(|m| m.rcr).map(f64::from),
        Pct => m.and_then(|m| m.pct).map(f64::from),
        Apt => m.and_then(|m| m.apt).map(f64::from),
        Clin => m.map(|m| m.clin as u8 as f64),
        Nrefs => (a.nrefs > 0).then_some(a.nrefs as f64),
        _ => None,
    }
}

pub fn push_clean(out: &mut String, s: &str) {
    let start = out.len();
    let mut gap = false;
    for c in s.chars() {
        if c.is_whitespace() || c.is_control() {
            gap = true;
        } else {
            if gap && out.len() > start {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        }
    }
}

const KEEP_TAGS: &[&str] = &[
    "ti", "ab", "tiab", "tw", "all", "ot", "mh", "majr", "title", "abstract", "title/abstract", "text word",
    "all fields", "mesh terms", "mesh major topic", "other term",
];

const STOP: &[&str] = &[
    "a", "an", "and", "any", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on", "or",
    "the", "to", "vs", "with", "not",
];

enum Matcher {
    Re(Regex),
    Fz { term: String, len: usize, prefix: bool, tol: usize },
}

struct Pat {
    m: Matcher,
    title: bool,
    abs: bool,
}

pub struct Kwic {
    pats: Vec<Pat>,
    ctx: usize,
    max: usize,
}

struct Window {
    title: bool,
    start: usize,
    end: usize,
    hits: usize,
}

fn words(t: &str) -> impl Iterator<Item = (usize, &str)> {
    t.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(move |w| (w.as_ptr() as usize - t.as_ptr() as usize, w))
}

impl Pat {
    fn spans(&self, text: &str, out: &mut Vec<(usize, usize)>) {
        match &self.m {
            Matcher::Re(r) => out.extend(r.find_iter(text).filter(|m| m.end() > m.start()).map(|m| (m.start(), m.end()))),
            Matcher::Fz { term, len, prefix, tol } => {
                for (at, w) in words(text) {
                    let low = w.to_lowercase();
                    let cand: String = if *prefix { low.chars().take(*len).collect() } else { low };
                    if cand.chars().count().abs_diff(*len) <= *tol && lev(&cand, term) <= *tol {
                        out.push((at, at + w.len()));
                    }
                }
            }
        }
    }
}

fn lit_pattern(term: &str, loose: bool) -> Option<String> {
    let t = term.trim();
    let (first, last) = (t.chars().next()?, t.chars().next_back()?);
    let mut p = t
        .split_whitespace()
        .map(|w| w.split('*').map(regex::escape).collect::<Vec<_>>().join(r"\w*"))
        .collect::<Vec<_>>()
        .join(r"[\s-]+");
    if first.is_alphanumeric() {
        p.insert_str(0, r"\b");
    }
    if last.is_alphanumeric() {
        if loose {
            p.push_str("(?:s|es)?");
        }
        p.push_str(r"\b");
    }
    Some(p)
}

type Scope = Option<(bool, bool)>;

fn query_terms(pos: &[String]) -> Vec<(String, bool, Scope)> {
    let mut units: Vec<(String, bool, Option<String>)> = Vec::new();
    for p in pos {
        let ch: Vec<char> = p.chars().collect();
        let mut i = 0;
        while i < ch.len() {
            let find = |from: usize, pred: &dyn Fn(char) -> bool| ch[from..].iter().position(|&c| pred(c)).map_or(ch.len(), |k| from + k);
            match ch[i] {
                '"' => {
                    let j = find(i + 1, &|c| c == '"');
                    units.push((ch[i + 1..j].iter().collect(), true, None));
                    i = j + 1;
                }
                '[' => {
                    let j = find(i + 1, &|c| c == ']');
                    let tag: String = ch[i + 1..j].iter().collect::<String>().to_lowercase();
                    if let Some(u) = units.last_mut() {
                        u.2 = Some(tag.split(':').next().unwrap_or("").to_string());
                    }
                    i = j + 1;
                }
                c if c.is_whitespace() || c == '(' || c == ')' => i += 1,
                _ => {
                    let j = find(i, &|c| c.is_whitespace() || matches!(c, '(' | ')' | '"' | '['));
                    units.push((ch[i..j].iter().collect(), false, None));
                    i = j;
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut after_not = false;
    for (t, quoted, tag) in units {
        let negated = std::mem::replace(&mut after_not, !quoted && t == "NOT");
        let keep = !negated
            && match &tag {
                Some(tag) => KEEP_TAGS.contains(&tag.as_str()),
                None if quoted => !t.trim().is_empty(),
                None => {
                    t.chars().count() >= 2
                        && !t.contains(':')
                        && !STOP.contains(&t.to_lowercase().as_str())
                        && !t.chars().all(|c| c.is_ascii_digit())
                }
            };
        if keep {
            let scope = match tag.as_deref() {
                Some("ti" | "title") => Some((true, false)),
                Some("ab" | "abstract") => Some((false, true)),
                _ => None,
            };
            out.push((t, quoted, scope));
        }
    }
    out
}

fn push_abs_bodies(out: &mut String, a: &Article) {
    for (_, t) in &a.abs {
        let mark = out.len();
        if mark > 0 {
            out.push(' ');
        }
        let body = out.len();
        push_clean(out, t);
        if out.len() == body {
            out.truncate(mark);
        }
    }
}

fn write_lead(out: &mut String, a: &Article) {
    let start = out.len();
    push_abs_bodies(out, a);
    cap_chars(out, start, 160);
}

fn widen_start(t: &str, s: usize, ctx: usize) -> usize {
    let mut p = s;
    for _ in 0..ctx {
        match t[..p].chars().next_back() {
            Some(c) => p -= c.len_utf8(),
            None => break,
        }
    }
    if p > 0 && !t[..p].ends_with(' ') && !t[p..].starts_with(' ') {
        p = t[..p].rfind(' ').map_or(0, |i| i + 1);
    } else if p < s && t[p..].starts_with(' ') {
        p += 1;
    }
    p
}

fn widen_end(t: &str, e: usize, ctx: usize) -> usize {
    let mut p = e;
    for _ in 0..ctx {
        match t[p..].chars().next() {
            Some(c) => p += c.len_utf8(),
            None => break,
        }
    }
    if p < t.len() && !t[..p].ends_with(' ') && !t[p..].starts_with(' ') {
        p = t[p..].find(' ').map_or(t.len(), |i| p + i);
    } else if p > e && t[..p].ends_with(' ') {
        p -= 1;
    }
    p
}

impl Kwic {
    pub fn from_args(a: &Args) -> Result<Kwic> {
        let mut k = Kwic { pats: Vec::new(), ctx: a.num("snip")?.unwrap_or(60), max: a.num::<usize>("snips")?.unwrap_or(3).max(1) };
        for (key, title, abs) in [("grep", true, true), ("grep-ti", true, false), ("grep-ab", false, true)] {
            for re in a.all(key) {
                if let Ok(r) = RegexBuilder::new(re).case_insensitive(true).build() {
                    k.push(Matcher::Re(r), title, abs);
                }
            }
        }
        for t in a.all("phrase") {
            k.lit(t, true, true, false);
        }
        for t in a.all("near") {
            for w in t.rsplit_once(':').map_or(t, |x| x.0).split_whitespace() {
                k.lit(w, true, true, true);
            }
        }
        for t in a.csv("fz") {
            k.fz(&t);
        }
        if !k.pats.is_empty() {
            return Ok(k);
        }
        for (key, title, abs) in [("all", true, true), ("any", true, true), ("tiab", true, true), ("ti", true, false), ("ab", false, true)] {
            for t in a.csv(key) {
                if t.contains(['"', '[']) {
                    for (u, quoted, scope) in query_terms(std::slice::from_ref(&t)) {
                        let (ti, ab) = scope.unwrap_or((title, abs));
                        k.lit(&u, ti, ab, !quoted);
                    }
                } else {
                    k.lit(&t, title, abs, true);
                }
            }
        }
        let search_like = a
            .flags
            .iter()
            .find_map(|f| f.strip_prefix("cmd:"))
            .is_none_or(|c| matches!(c, "search" | "count" | "explain" | "trend" | "facet"));
        let own = if search_like { a.pos.as_slice() } else { &[] };
        let query: Vec<String> = own.iter().cloned().chain(a.all("query").into_iter().map(String::from)).collect();
        let bare = |f: &str| a.flags.iter().any(|x| x == f);
        let (bti, bab) = (bare("ti"), bare("ab"));
        let default_scope = match (bare("tiab") || (bti && bab), bti, bab) {
            (true, _, _) | (_, false, false) => (true, true),
            (_, true, false) => (true, false),
            _ => (false, true),
        };
        for (t, quoted, scope) in query_terms(&query) {
            let (ti, ab) = scope.unwrap_or(default_scope);
            k.lit(&t, ti, ab, !quoted);
        }
        Ok(k)
    }

    fn push(&mut self, m: Matcher, title: bool, abs: bool) {
        let same = |x: &Matcher| match (x, &m) {
            (Matcher::Re(a), Matcher::Re(b)) => a.as_str() == b.as_str(),
            (Matcher::Fz { term: a, prefix: p, .. }, Matcher::Fz { term: b, prefix: q, .. }) => a == b && p == q,
            _ => false,
        };
        if let Some(p) = self.pats.iter_mut().find(|p| same(&p.m)) {
            p.title |= title;
            p.abs |= abs;
        } else {
            self.pats.push(Pat { m, title, abs });
        }
    }

    fn lit(&mut self, term: &str, title: bool, abs: bool, loose: bool) {
        if let Some(r) = lit_pattern(term, loose).and_then(|p| RegexBuilder::new(&p).case_insensitive(true).build().ok()) {
            self.push(Matcher::Re(r), title, abs);
        }
    }

    fn fz(&mut self, term: &str) {
        let t = term.trim();
        let (t, prefix) = t.strip_suffix('*').map_or((t, false), |r| (r, true));
        let term = t.to_lowercase();
        let len = term.chars().count();
        if len > 0 {
            let tol = match len {
                0..=3 => 0,
                4..=7 => 1,
                _ => 2,
            };
            self.push(Matcher::Fz { term, len, prefix, tol }, true, true);
        }
    }

    fn windows(&self, text: &str, title: bool, out: &mut Vec<Window>) {
        let mut spans = Vec::new();
        for p in self.pats.iter().filter(|p| if title { p.title } else { p.abs }) {
            p.spans(text, &mut spans);
        }
        spans.sort_unstable();
        let mark = out.len();
        for (s, e) in spans {
            let (ws, we) = (widen_start(text, s, self.ctx), widen_end(text, e, self.ctx));
            match out[mark..].last_mut() {
                Some(w) if ws <= w.end => {
                    w.end = w.end.max(we);
                    w.hits += 1;
                }
                _ => out.push(Window { title, start: ws, end: we, hits: 1 }),
            }
        }
    }

    fn write(&self, out: &mut String, a: &Article) {
        let mut title = String::new();
        push_clean(&mut title, &a.title);
        let mut text = String::new();
        push_abs_bodies(&mut text, a);
        let mut wins = Vec::new();
        self.windows(&title, true, &mut wins);
        let mark = wins.len();
        self.windows(&text, false, &mut wins);
        if wins.len() == mark && !a.other_abs.is_empty() {
            text.clear();
            push_clean(&mut text, &a.other_abs);
            self.windows(&text, false, &mut wins);
        }
        if wins.is_empty() {
            return write_lead(out, a);
        }
        let mut pick: Vec<usize> = (0..wins.len()).collect();
        pick.sort_by_key(|&i| std::cmp::Reverse(wins[i].hits));
        pick.truncate(self.max);
        pick.sort_unstable();
        for (n, &i) in pick.iter().enumerate() {
            let w = &wins[i];
            let src = if w.title { &title } else { &text };
            if n > 0 {
                out.push_str(" | ");
            }
            if w.title {
                out.push_str("T: ");
            }
            if w.start > 0 {
                out.push('…');
            }
            out.push_str(&src[w.start..w.end]);
            if w.end < src.len() {
                out.push('…');
            }
        }
    }
}

#[cfg(test)]
pub fn value(a: &Article, f: Field, o: &Opts) -> String {
    let mut s = String::new();
    write_value(&mut s, a, f, o);
    s
}

pub fn write_value(out: &mut String, a: &Article, f: Field, o: &Opts) {
    match f {
        Title => push_clean(out, &a.title),
        Abs => write_abs(out, a, o.abs),
        Concl => write_sections(out, a, "C", o.abs),
        Bg => write_sections(out, a, "BG", o.abs),
        Methods => write_sections(out, a, "M", o.abs),
        Results => write_sections(out, a, "R", o.abs),
        Authors => write_authors(out, &a.authors, o.authors),
        First => first_author(a).iter().for_each(|u| push_clean(out, &u.name)),
        Last => last_author(a).iter().for_each(|u| push_clean(out, &u.name)),
        Aff => write_aff(out, a),
        Journal => {
            let j = &a.journal;
            push_clean(out, if j.iso.is_empty() { &j.title } else { &j.iso });
        }
        Jt => push_clean(out, &a.journal.title),
        Q => match a.jr.as_ref().filter(|j| (1..=4).contains(&j.q)) {
            Some(j) => {
                out.push('Q');
                out.push((b'0' + j.q) as char);
            }
            None => out.push('-'),
        },
        Pmid | Year | H | Cites | Nrefs | Clin => {
            if let Some(n) = number(a, f) {
                let _ = write!(out, "{}", n as u64);
            }
        }
        Sjr | Rcr | Apt => {
            if let Some(n) = number(a, f) {
                let _ = write!(out, "{n:.2}");
            }
        }
        Pct => {
            if let Some(n) = number(a, f) {
                let _ = write!(out, "{n:.0}");
            }
        }
        Date => push_clean(out, &a.date),
        Epub => push_clean(out, &a.epub),
        Doi => push_clean(out, &a.doi),
        Pmc => push_clean(out, &a.pmc),
        Status => push_clean(out, &a.status),
        Types | Mesh | Major | Kw | Lang | Grants | Nct | Comments | Problems => {
            let mut n = 0;
            each_item(a, f, &mut |item| {
                if n > 0 {
                    out.push_str(f.sep());
                }
                n += 1;
                out.push_str(item);
            });
        }
        Links => {
            if !a.doi.is_empty() {
                out.push_str("https://doi.org/");
                push_clean(out, &a.doi);
            }
            if !a.pmc.is_empty() {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push_str("https://pmc.ncbi.nlm.nih.gov/articles/");
                if !a.pmc.starts_with("PMC") {
                    out.push_str("PMC");
                }
                push_clean(out, &a.pmc);
            }
        }
        Url => {
            let _ = write!(out, "https://pubmed.ncbi.nlm.nih.gov/{}/", a.pmid);
        }
        Vol => {
            push_clean(out, &a.journal.vol);
            if !a.journal.issue.is_empty() {
                out.push('(');
                push_clean(out, &a.journal.issue);
                out.push(')');
            }
        }
        Pages => push_clean(out, &a.journal.pages),
        Country => push_clean(out, &a.journal.country),
        Score => {
            if let Some(&s) = o.scores.and_then(|m| m.get(&a.pmid)) {
                if s.fract() == 0.0 {
                    let _ = write!(out, "{s:.0}");
                } else {
                    let _ = write!(out, "{s:.2}");
                }
            }
        }
        Snip => match &o.snip {
            Some(k) => k.write(out, a),
            None => write_lead(out, a),
        },
        All => {}
    }
}

fn each_item(a: &Article, f: Field, cb: &mut dyn FnMut(&str)) {
    let mut buf = String::new();
    let mut emit = |buf: &mut String| {
        if !buf.is_empty() {
            cb(buf);
        }
        buf.clear();
    };
    match f {
        Types => {
            for t in a.types.iter().filter(|t| *t != "Journal Article") {
                push_clean(&mut buf, t);
                emit(&mut buf);
            }
        }
        Mesh | Major => {
            for m in a.mesh.iter().filter(|m| f == Mesh || m.major) {
                push_clean(&mut buf, &m.name);
                if f == Mesh && m.major {
                    buf.push('*');
                }
                if f == Mesh {
                    for q in &m.quals {
                        buf.push('/');
                        push_clean(&mut buf, q);
                    }
                }
                emit(&mut buf);
            }
        }
        Kw => {
            for k in &a.kw {
                push_clean(&mut buf, k);
                emit(&mut buf);
            }
        }
        Lang => {
            for l in &a.langs {
                push_clean(&mut buf, l);
                emit(&mut buf);
            }
        }
        Grants => {
            for g in &a.grants {
                push_clean(&mut buf, &g.id);
                if !g.agency.is_empty() {
                    if !buf.is_empty() {
                        buf.push(' ');
                    }
                    push_clean(&mut buf, &g.agency);
                }
                emit(&mut buf);
            }
        }
        Nct => {
            for id in a.banks.iter().flat_map(|b| &b.1).filter(|i| i.starts_with("NCT")) {
                push_clean(&mut buf, id);
                emit(&mut buf);
            }
        }
        Comments => {
            for c in a.corrs.iter().filter(|c| c.is_comment()) {
                corr_item(&mut buf, c, if c.kind == "CommentIn" { "in" } else { "on" });
                emit(&mut buf);
            }
        }
        Problems => {
            for c in a.corrs.iter().filter(|c| c.is_problem()) {
                corr_item(&mut buf, c, problem_label(&c.kind));
                emit(&mut buf);
            }
            for t in &a.types {
                let label = match t.as_str() {
                    "Retracted Publication" if !a.corrs.iter().any(|c| c.kind == "RetractionIn") => "retracted",
                    "Retraction of Publication" => "retraction",
                    "Published Erratum" => "erratum-notice",
                    _ => continue,
                };
                buf.push_str(label);
                emit(&mut buf);
            }
        }
        _ => {}
    }
}

fn problem_label(kind: &str) -> &'static str {
    match kind {
        "RetractionIn" => "retracted",
        "RetractionOf" => "retraction-of",
        "ErratumIn" => "erratum",
        "ErratumFor" => "erratum-for",
        "ExpressionOfConcernIn" => "eoc",
        "ExpressionOfConcernFor" => "eoc-for",
        "PartialRetractionIn" => "partial-retracted",
        _ => "partial-retraction-of",
    }
}

fn corr_item(buf: &mut String, c: &Corr, label: &str) {
    buf.push_str(label);
    if let Some(p) = c.pmid {
        let _ = write!(buf, ":{p}");
    }
    let text = if c.title.is_empty() { &c.note } else { &c.title };
    if !text.is_empty() {
        buf.push(' ');
        push_clean(buf, text);
    }
}

pub fn write_authors(out: &mut String, au: &[Author], n: usize) {
    let len = au.len();
    let name = |out: &mut String, i: usize| push_clean(out, &au[i].name);
    if len == 0 {
        return;
    }
    if n == 0 || n >= len {
        for i in 0..len {
            if i > 0 {
                out.push_str(", ");
            }
            name(out, i);
        }
    } else if n == 1 {
        name(out, 0);
        let _ = write!(out, " +{}", len - 1);
    } else {
        for i in 0..n - 1 {
            if i > 0 {
                out.push_str(", ");
            }
            name(out, i);
        }
        out.push_str(" … ");
        name(out, len - 1);
    }
}

const INSTITUTION: &[&str] = &[
    "univ", "instit", "hospital", "college", "center", "centre", "academy", "clinic", "school",
    "laborator", "foundation", "research", "medical", "polytechnic", "ecole", "hôpital",
];

fn write_aff(out: &mut String, a: &Article) {
    let idx = a.authors.first().and_then(|u| u.aff.first()).map_or(0, |&i| i as usize);
    let Some(aff) = a.affs.get(idx).or(a.affs.first()) else { return };
    let aff = match aff.find("Electronic address") {
        Some(i) => &aff[..i],
        None => aff,
    };
    let parts: Vec<&str> = aff
        .split(',')
        .map(|p| p.trim().trim_end_matches('.'))
        .filter(|p| !p.is_empty() && !p.contains('@'))
        .collect();
    let Some(&last) = parts.last() else { return };
    let inst = parts.iter().find(|p| INSTITUTION.iter().any(|k| has_ci(p, k)));
    let start = out.len();
    match inst {
        Some(&inst) => {
            push_clean(out, inst);
            if inst != last {
                out.push_str(", ");
                push_clean(out, last);
            }
        }
        None => {
            let from = parts.len().saturating_sub(2);
            push_clean(out, &parts[from..].join(", "));
        }
    }
    cap_chars(out, start, 90);
}

fn has_ci(h: &str, n: &str) -> bool {
    h.len() >= n.len() && h.as_bytes().windows(n.len()).any(|w| w.eq_ignore_ascii_case(n.as_bytes()))
}

pub fn cap_chars(out: &mut String, start: usize, cap: usize) {
    if cap == 0 {
        return;
    }
    let Some((cut, _)) = out[start..].char_indices().nth(cap) else { return };
    let mut end = start + cut;
    if let Some(sp) = out[start..end].rfind(' ').filter(|&sp| sp > cap / 2) {
        end = start + sp;
    }
    let kept = out[start..end].trim_end_matches([' ', ',', ';', ':', '.', '-']).len();
    out.truncate(start + kept);
    out.push('…');
}

pub fn label_abbr(label: &str) -> std::borrow::Cow<'static, str> {
    use std::borrow::Cow::*;
    let l = label.trim();
    let starts = |p: &str| l.as_bytes().get(..p.len()).is_some_and(|b| b.eq_ignore_ascii_case(p.as_bytes()));
    if has_ci(l, "METHOD") {
        Borrowed("M")
    } else if has_ci(l, "CONCLUSION") || has_ci(l, "INTERPRETATION") {
        Borrowed("C")
    } else if has_ci(l, "RESULT") || has_ci(l, "FINDING") {
        Borrowed("R")
    } else if has_ci(l, "BACKGROUND") || has_ci(l, "INTRODUCTION") {
        Borrowed("BG")
    } else if starts("OBJECTIVE") || starts("AIM") || starts("PURPOSE") {
        Borrowed("O")
    } else {
        Owned(l.chars().filter(|c| c.is_alphabetic()).take(3).flat_map(char::to_uppercase).collect())
    }
}

fn write_abs(out: &mut String, a: &Article, cap: usize) {
    let start = out.len();
    for (l, t) in &a.abs {
        let mark = out.len();
        if mark > start {
            out.push(' ');
        }
        let abbr = label_abbr(l);
        if !abbr.is_empty() {
            out.push_str(&abbr);
            out.push_str(": ");
        }
        let body = out.len();
        push_clean(out, t);
        if out.len() == body {
            out.truncate(mark);
        }
    }
    cap_chars(out, start, cap);
}

const NOT_CONCLUSION: &[&str] = &["FUNDING", "DISCLOS", "REGISTRATION", "ACKNOWLEDG", "CONFLICT", "COMPETING", "FINANCIAL"];

pub fn conclusion_sections(a: &Article) -> Vec<&str> {
    let labelled: Vec<&str> = a.abs.iter().filter(|(l, _)| label_abbr(l) == "C").map(|(_, t)| t.as_str()).collect();
    if !labelled.is_empty() {
        return labelled;
    }
    let categorised: Vec<&str> = a
        .abs
        .iter()
        .zip(a.abs_cat.iter())
        .filter(|(_, c)| c.eq_ignore_ascii_case("CONCLUSIONS"))
        .map(|((_, t), _)| t.as_str())
        .collect();
    if !categorised.is_empty() {
        return categorised;
    }
    a.abs
        .iter()
        .rev()
        .find(|(l, _)| !NOT_CONCLUSION.iter().any(|p| has_ci(l, p)))
        .map(|(_, t)| vec![last_sentences(t, 2)])
        .unwrap_or_default()
}

fn section_kind(label: &str, cat: &str) -> &'static str {
    match label_abbr(label).as_ref() {
        "M" => "M",
        "R" => "R",
        "C" => "C",
        "BG" | "O" => "BG",
        _ => match cat.to_ascii_uppercase().as_str() {
            "METHODS" => "M",
            "RESULTS" => "R",
            "CONCLUSIONS" => "C",
            "BACKGROUND" | "OBJECTIVE" => "BG",
            _ => "",
        },
    }
}

fn write_sections(out: &mut String, a: &Article, kind: &str, cap: usize) {
    let start = out.len();
    let parts: Vec<&str> = if kind == "C" {
        conclusion_sections(a)
    } else {
        a.abs
            .iter()
            .enumerate()
            .filter(|(i, (l, _))| section_kind(l, a.abs_cat.get(*i).map_or("", String::as_str)) == kind)
            .map(|(_, (_, t))| t.as_str())
            .collect()
    };
    for t in parts {
        let mark = out.len();
        if mark > start {
            out.push(' ');
        }
        let body = out.len();
        push_clean(out, t);
        if out.len() == body {
            out.truncate(mark);
        }
    }
    cap_chars(out, start, cap);
}

fn last_sentences(t: &str, n: usize) -> &str {
    let t = t.trim();
    let b = t.as_bytes();
    let mut found = 0;
    for i in (0..b.len().saturating_sub(2)).rev() {
        if b[i] == b'.' && b[i + 1] == b' ' && t[i + 2..].chars().next().is_some_and(char::is_uppercase) {
            found += 1;
            if found == n {
                return &t[i + 2..];
            }
        }
    }
    t
}

pub fn write_json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn write_json_field(out: &mut String, a: &Article, f: Field, o: &Opts, scratch: &mut String) -> bool {
    scratch.clear();
    if f.json() == Json::List {
        let mut n = 0;
        each_item(a, f, &mut |item| {
            scratch.push(if n == 0 { '[' } else { ',' });
            n += 1;
            write_json_str(scratch, item);
        });
        if n == 0 {
            return false;
        }
        scratch.push(']');
        write_json_key(out, f.name());
        out.push_str(scratch);
        return true;
    }
    write_value(scratch, a, f, o);
    if scratch.is_empty() || (f == Q && scratch == "-") {
        return false;
    }
    write_json_key(out, f.name());
    match f.json() {
        Json::Num => out.push_str(scratch),
        Json::Q => out.push_str(&scratch[1..]),
        Json::Bool => out.push_str(if scratch == "1" { "true" } else { "false" }),
        _ => write_json_str(out, scratch),
    }
    true
}

fn write_json_key(out: &mut String, name: &str) {
    if !out.ends_with('{') {
        out.push(',');
    }
    out.push('"');
    out.push_str(name);
    out.push_str("\":");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_abbr_is_unicode_safe() {
        assert_eq!(label_abbr("CONCLUSIÓN"), "CON");
        assert_eq!(label_abbr("ÓBJECTIVE"), "ÓBJ");
        assert_eq!(label_abbr("OBJECTIVES"), "O");
        assert_eq!(label_abbr("aim"), "O");
        assert_eq!(label_abbr("ÄÖ"), "ÄÖ");
        assert_eq!(label_abbr("MÉTODOS"), "MÉT");
    }

    fn au(n: &str) -> Author {
        Author { name: n.into(), ..Default::default() }
    }

    fn art() -> Article {
        Article {
            pmid: 123,
            title: "A  trial\tof things".into(),
            abs: vec![
                ("BACKGROUND".into(), "Bg text.".into()),
                ("METHODS".into(), "We did X.".into()),
                ("RESULTS".into(), "It worked.".into()),
                ("CONCLUSIONS".into(), "Good. Very good.".into()),
            ],
            authors: ["Smith J", "Lee K", "Wu H", "Roe P", "Zed Z"].iter().map(|n| au(n)).collect(),
            affs: vec!["Department of Medicine, Stanford University School of Medicine, Stanford, CA, USA.".into()],
            journal: JournalInfo { title: "The Lancet".into(), iso: "Lancet".into(), vol: "9".into(), issue: "2".into(), ..Default::default() },
            year: 2021,
            ..Default::default()
        }
    }

    fn val(a: &Article, f: &str) -> String {
        value(a, resolve(f).unwrap(), &Opts { authors: 3, abs: 0, snip: None, scores: None })
    }

    #[test]
    fn resolves_aliases() {
        assert_eq!(resolve("T"), Some(Title));
        assert_eq!(resolve("au"), Some(Authors));
        assert_eq!(resolve("a"), Some(Abs));
        assert_eq!(resolve("j"), Some(Journal));
        assert_eq!(resolve("y"), Some(Year));
        assert_eq!(resolve("nope"), None);
    }

    #[test]
    fn did_you_mean() {
        assert_eq!(unknown("titl").to_string(), "unknown field 'titl', did you mean 'title'?");
        assert_eq!(unknown("autor").to_string(), "unknown field 'autor', did you mean 'authors'?");
        assert!(unknown("zzzzzzzz").to_string().contains("fields: pmid title"));
    }

    #[test]
    fn needs() {
        let n = |v: &[&str]| {
            let mut a = Args::default();
            if !v.is_empty() {
                a.opts.push(("fields".into(), v.join(",")));
            }
            need_for(&Set::default(), &a)
        };
        assert_eq!(n(&["title", "year"]), Need::default());
        assert_eq!(n(&["q"]), Need { rank: true, ..Default::default() });
        assert_eq!(n(&["cites", "problems"]), Need { metrics: true, expand: true, rank: false });
        assert_eq!(n(&["all"]), Need { rank: true, metrics: true, expand: true });
        assert_eq!(n(&[]), Need { rank: true, ..Default::default() });
        assert_eq!(n(&["+abs"]), Need { rank: true, ..Default::default() });
        assert_eq!(n(&["+cites"]), Need { rank: true, metrics: true, expand: false });
        assert_eq!(n(&["-q", "-authors"]), Need::default());
    }

    #[test]
    fn all_expands_without_duplicates() {
        let f = parse(&["title".into(), "all".into()]).unwrap();
        assert_eq!(f[0], Title);
        assert_eq!(f.iter().filter(|x| **x == Title).count(), 1);
        assert!(f.contains(&Mesh) && !f.contains(&All));
    }

    #[test]
    fn authors_forms() {
        let a = art();
        let w = |n| {
            let mut s = String::new();
            write_authors(&mut s, &a.authors, n);
            s
        };
        assert_eq!(w(3), "Smith J, Lee K … Zed Z");
        assert_eq!(w(1), "Smith J +4");
        assert_eq!(w(2), "Smith J … Zed Z");
        assert_eq!(w(5), "Smith J, Lee K, Wu H, Roe P, Zed Z");
        assert_eq!(w(0), w(9));
    }

    #[test]
    fn text_fields() {
        let a = art();
        assert_eq!(val(&a, "title"), "A trial of things");
        assert_eq!(val(&a, "journal"), "Lancet");
        assert_eq!(val(&a, "abs"), "BG: Bg text. M: We did X. R: It worked. C: Good. Very good.");
        assert_eq!(val(&a, "concl"), "Good. Very good.");
        assert_eq!(val(&a, "methods"), "We did X.");
        assert_eq!(val(&a, "aff"), "Stanford University School of Medicine, USA");
        assert_eq!(val(&a, "vol"), "9(2)");
        assert_eq!(val(&a, "q"), "-");
        assert_eq!(val(&a, "cites"), "");
    }

    #[test]
    fn sections_use_nlm_category_for_jama_style_labels() {
        let mut a = art();
        a.abs = vec![
            ("Importance".into(), "Matters.".into()),
            ("Objective".into(), "To test X.".into()),
            ("Design, Setting, and Participants".into(), "RCT in ICUs.".into()),
            ("Interventions".into(), "Drug or placebo.".into()),
            ("Main Outcomes and Measures".into(), "Mortality.".into()),
            ("Results".into(), "Rates fell.".into()),
            ("Conclusions and Relevance".into(), "Drug works.".into()),
        ];
        a.abs_cat = vec![
            "BACKGROUND".into(),
            "OBJECTIVE".into(),
            "METHODS".into(),
            "METHODS".into(),
            "METHODS".into(),
            "RESULTS".into(),
            "CONCLUSIONS".into(),
        ];
        assert_eq!(val(&a, "methods"), "RCT in ICUs. Drug or placebo. Mortality.");
        assert_eq!(val(&a, "bg"), "Matters. To test X.");
        assert_eq!(val(&a, "results"), "Rates fell.");
        assert_eq!(val(&a, "concl"), "Drug works.");
        a.abs_cat.clear();
        assert_eq!(val(&a, "methods"), "");
        assert_eq!(val(&a, "bg"), "To test X.");
    }

    #[test]
    fn concl_fallback_last_sentences() {
        let mut a = art();
        a.abs = vec![(String::new(), "One thing. Two things. Three things. We conclude 5.0 x. Done here.".into())];
        assert_eq!(val(&a, "concl"), "We conclude 5.0 x. Done here.");
    }

    #[test]
    fn concl_skips_funding_and_reads_interpretation() {
        let mut a = art();
        a.abs = vec![
            ("Background".into(), "Bg.".into()),
            ("Findings".into(), "Rates fell.".into()),
            ("Interpretation".into(), "Drug works. Use it.".into()),
            ("Funding".into(), "US National Institutes of Health.".into()),
        ];
        assert_eq!(val(&a, "concl"), "Drug works. Use it.");
        assert!(val(&a, "abs").contains("C: Drug works. Use it."));
    }

    #[test]
    fn concl_prefers_category_then_skips_funding_in_fallback() {
        let mut a = art();
        a.abs = vec![
            ("Findings".into(), "Rates fell.".into()),
            ("Take-home".into(), "Works well.".into()),
            ("Funding".into(), "NIH.".into()),
        ];
        a.abs_cat = vec!["RESULTS".into(), "CONCLUSIONS".into(), String::new()];
        assert_eq!(val(&a, "concl"), "Works well.");
        a.abs_cat.clear();
        assert_eq!(val(&a, "concl"), "Works well.");
        a.abs = vec![(String::new(), "One thing. Two things. We conclude X. Done.".into()), ("FUNDING".into(), "NIH".into())];
        assert_eq!(val(&a, "concl"), "We conclude X. Done.");
        a.abs = vec![("Funding".into(), "NIH".into())];
        assert_eq!(val(&a, "concl"), "");
    }

    #[test]
    fn abs_cap_and_labels() {
        let mut a = art();
        a.abs = vec![("Materials and Methods".into(), "alpha beta gamma delta".into()), ("Aims".into(), "x".into()), ("Design".into(), "y".into())];
        assert_eq!(val(&a, "abs"), "M: alpha beta gamma delta O: x DES: y");
        let v = value(&a, Abs, &Opts { authors: 3, abs: 12, snip: None, scores: None });
        assert_eq!(v, "M: alpha…");
    }

    #[test]
    fn ranked_and_metrics() {
        let mut a = art();
        a.jr = Some(JournalRank { q: 2, sjr: 1.234, h: 80, area: String::new() });
        a.m = Some(Metrics { cites: 17, rcr: Some(1.5), pct: Some(88.4), apt: None, cpy: None, clin: true });
        assert_eq!(val(&a, "q"), "Q2");
        assert_eq!(val(&a, "sjr"), "1.23");
        assert_eq!(val(&a, "cites"), "17");
        assert_eq!(val(&a, "rcr"), "1.50");
        assert_eq!(val(&a, "pct"), "88");
        assert_eq!(val(&a, "apt"), "");
        assert_eq!(val(&a, "clin"), "1");
        assert_eq!(number(&a, Q), Some(2.0));
    }

    #[test]
    fn corrections() {
        let mut a = art();
        a.corrs = vec![
            Corr { kind: "CommentIn".into(), pmid: Some(9), note: "x".into(), title: "Reply".into() },
            Corr { kind: "RetractionIn".into(), pmid: Some(77), ..Default::default() },
            Corr { kind: "ErratumIn".into(), pmid: Some(5), ..Default::default() },
        ];
        a.types = vec!["Journal Article".into(), "Randomized Controlled Trial".into(), "Retracted Publication".into()];
        assert_eq!(val(&a, "comments"), "in:9 Reply");
        assert_eq!(val(&a, "problems"), "retracted:77 | erratum:5");
        assert_eq!(val(&a, "types"), "Randomized Controlled Trial, Retracted Publication");
    }

    #[test]
    fn mesh_grants_nct() {
        let mut a = art();
        a.mesh = vec![
            crate::model::Mesh { name: "Sepsis".into(), major: true, quals: vec!["drug therapy".into()] },
            crate::model::Mesh { name: "Humans".into(), ..Default::default() },
        ];
        a.grants = vec![Grant { id: "R01".into(), agency: "NCI".into(), country: "US".into() }];
        a.banks = vec![("ClinicalTrials.gov".into(), vec!["NCT0001".into(), "X".into()])];
        assert_eq!(val(&a, "mesh"), "Sepsis*/drug therapy; Humans");
        assert_eq!(val(&a, "major"), "Sepsis");
        assert_eq!(val(&a, "grants"), "R01 NCI");
        assert_eq!(val(&a, "nct"), "NCT0001");
    }

    fn pick(a: &str, set: &Set) -> Vec<&'static str> {
        let args = Args::parse(&crate::args::split_words(a).unwrap()).unwrap();
        choose(set, &args).unwrap().0.iter().map(|f| f.name()).collect()
    }

    #[test]
    fn field_spec_plus_minus_default() {
        let set = Set::default();
        assert_eq!(pick("-f +abs", &set), ["pmid", "year", "journal", "q", "authors", "title", "abs"]);
        assert_eq!(pick("-f t,a", &set), ["title", "abs"]);
        assert_eq!(pick("-f=-q,-authors", &set), ["pmid", "year", "journal", "title"]);
        assert_eq!(pick("-f ~q,+snip,-t", &set), ["pmid", "year", "journal", "authors", "snip"]);
        assert_eq!(pick("-f=+mesh,-au", &set), ["pmid", "year", "journal", "q", "title", "mesh"]);
        assert_eq!(pick("", &set), ["pmid", "year", "journal", "q", "authors", "title"]);
        assert!(!pick("-f=+all,-abs", &set).contains(&"abs"));
    }

    #[test]
    fn relative_spec_starts_from_command_defaults() {
        let set = Set { fields: Some(["pmid", "cites", "rcr", "title"].map(String::from).to_vec()), ..Default::default() };
        assert_eq!(pick("-f +doi", &set), ["pmid", "cites", "rcr", "title", "doi"]);
        assert_eq!(pick("-f=-rcr,+mesh", &set), ["pmid", "cites", "title", "mesh"]);
        let args = Args::parse(&crate::args::split_words("-f +doi").unwrap()).unwrap();
        assert_eq!(need_for(&set, &args), Need { metrics: true, ..Default::default() });
        let none = Args::parse(&[]).unwrap();
        assert_eq!(need_for(&Set::default(), &none), Need { rank: true, ..Default::default() });
    }

    #[test]
    fn field_spec_errors_are_short() {
        let set = Set::default();
        let err = |s: &str| match Args::parse(&crate::args::split_words(s).unwrap()) {
            Ok(a) => choose(&set, &a).unwrap_err().to_string(),
            Err(e) => e.to_string(),
        };
        assert_eq!(err("-f=-titl"), "unknown field 'titl', did you mean 'title'?");
        assert_eq!(err("-f=-pmid,-year,-journal,-q,-authors,-title"), "all fields removed");
        assert!(err("-f -q").starts_with("-f needs a value"));
        assert!(err("-f=+abz").contains("did you mean 'abs'"));
    }

    fn kw(args: &str, a: &Article) -> String {
        let args = Args::parse(&crate::args::split_words(args).unwrap()).unwrap();
        let o = Opts { authors: 3, abs: 0, snip: Some(Kwic::from_args(&args).unwrap()), scores: None };
        value(a, Snip, &o)
    }

    fn kw_cmd(cmd: &str, args: &str, a: &Article) -> String {
        let mut args = Args::parse(&crate::args::split_words(args).unwrap()).unwrap();
        args.flags.push(format!("cmd:{cmd}"));
        let o = Opts { authors: 3, abs: 0, snip: Some(Kwic::from_args(&args).unwrap()), scores: None };
        value(a, Snip, &o)
    }

    #[test]
    fn snip_positionals_only_for_search_like_stages() {
        let a = long_art();
        assert!(kw_cmd("search", "placebo --snip 12", &a).contains("placebo"));
        let sorted = kw_cmd("sort", "first --grep placebo --snip 12", &a);
        assert_eq!(sorted, "…okay. Finally placebo arm showed…");
        let top = kw_cmd("top", "5 --snip 12", &a);
        assert!(!top.contains('5'), "{top}");
    }

    fn long_art() -> Article {
        let mut a = art();
        a.title = "Statins and dementia risk".into();
        a.abs = vec![
            ("BACKGROUND".into(), "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau".into()),
            ("RESULTS".into(), "Dementia incidence fell with statin use in 3 cohorts. Then more filler words follow here and there until the end of text with no more hits for a long while okay. Finally placebo arm showed statin harm.".into()),
        ];
        a
    }

    #[test]
    fn snip_window_merge_and_ellipsis() {
        let a = long_art();
        let s = kw("--grep placebo --snip 12", &a);
        assert_eq!(s, "…okay. Finally placebo arm showed…");
        let s = kw("--grep placebo|statin|filler --snip 12 --snips 9", &a);
        assert_eq!(s.matches(" | ").count() + 1, 3, "{s}");
        assert!(s.starts_with("T: Statins and dementia… | …incidence"), "{s}");
        assert!(!s.contains("alpha"), "{s}");
    }

    #[test]
    fn snip_reads_forwarded_boolean_query() {
        let a = long_art();
        let s = kw("--query 'placebo NOT cohorts' --snip 12", &a);
        assert!(s.contains("Finally placebo arm showed"), "{s}");
        assert!(!s.contains("NOT"), "{s}");
        assert!(!s.contains("cohorts"), "{s}");
    }

    #[test]
    fn snip_limit_picks_best_windows() {
        let a = long_art();
        let s = kw("--grep placebo|statin|filler --snip 12 --snips 2", &a);
        assert_eq!(s.matches(" | ").count(), 1, "{s}");
        assert!(s.contains("Finally placebo arm showed statin harm."), "{s}");
    }

    #[test]
    fn snip_title_prefix_and_scope() {
        let a = long_art();
        assert_eq!(kw("--grep-ti dementia", &a), "T: Statins and dementia risk");
        assert!(!kw("--grep-ab dementia --snip 5", &a).contains("T:"));
        assert!(kw("--grep-ab dementia --snip 5", &a).contains("Dementia incidence"));
    }

    #[test]
    fn snip_terms_from_query_and_flags() {
        let a = long_art();
        let s = kw("statin AND (cohorts[tiab] OR smith[au]) NOT placebo --snip 5", &a);
        assert!(s.contains("cohorts") && s.contains("statin use") && !s.contains("placebo"), "{s}");
        assert!(!s.contains("Smith"), "{s}");
        assert!(kw("--phrase \"statin use\" --snip 4", &a).contains("with statin use in"));
        assert!(kw("--fz dementa --snip 4", &a).contains("Dementia"));
        assert!(kw("--fz demen* --snip 4", &a).contains("Dementia"));
        assert!(kw("--all cohort --snip 4", &a).contains("cohorts"));
        assert!(kw("--grep '(' --snip 4", &a).starts_with("alpha"), "invalid regex falls back");
    }

    #[test]
    fn snip_filter_flags_accept_quoted_phrases_and_tags() {
        let a = long_art();
        let s = kw("--ab '\"statin use\"' --snip 4", &a);
        assert!(s.contains("with statin use in"), "{s}");
        let s = kw("--ab 'cohorts[ab]' --snip 4", &a);
        assert!(s.contains("cohorts") && !s.contains('['), "{s}");
        assert!(kw("--all 'dementia[ti]' --snip 4", &a).starts_with("T: "));
    }

    #[test]
    fn snip_keeps_explicit_field_scope_of_terms() {
        let a = long_art();
        let s = kw("--all 'dementia[ab]' --snip 4", &a);
        assert!(!s.contains("T: ") && s.contains("Dementia"), "{s}");
        let s = kw("--tiab 'dementia[ti]' --snip 4", &a);
        assert!(s.starts_with("T: ") && !s.contains(" | "), "{s}");
        let s = kw("--any 'statin' --snip 4", &a);
        assert!(s.starts_with("T: ") && s.contains(" | "), "{s}");
    }

    #[test]
    fn snip_bare_scope_flags_limit_free_text_terms() {
        let a = long_art();
        let s = kw("dementia --ti --snip 4", &a);
        assert!(s.starts_with("T: ") && !s.contains(" | "), "{s}");
        let s = kw("dementia --ab --snip 4", &a);
        assert!(!s.contains("T:") && s.contains("Dementia"), "{s}");
        let s = kw("dementia --tiab --snip 4", &a);
        assert!(s.starts_with("T: ") && s.contains(" | "), "{s}");
        let s = kw("dementia[ab] --ti --snip 4", &a);
        assert!(!s.contains("T:"), "{s}");
        let mut b = Args::parse(&crate::args::split_words("--snip 4").unwrap()).unwrap();
        b.opts.push(("query".into(), "dementia".into()));
        b.flags.push("ti".into());
        let o = Opts { authors: 3, abs: 0, snip: Some(Kwic::from_args(&b).unwrap()), scores: None };
        let s = value(&a, Snip, &o);
        assert!(s.starts_with("T: ") && !s.contains(" | "), "{s}");
    }

    #[test]
    fn snip_falls_back_to_other_abstract() {
        let mut a = long_art();
        a.abs.clear();
        a.other_abs = "Resumen traducido con la palabra estatinas aqui.".into();
        let s = kw("--grep-ab estatinas --snip 8", &a);
        assert!(s.contains("estatinas"), "{s}");
        let mut b = long_art();
        b.other_abs = "Resumen traducido con la palabra estatinas aqui.".into();
        assert!(kw("--grep-ab estatinas --snip 8", &b).contains("estatinas"));
    }

    #[test]
    fn snip_falls_back_to_abstract_lead() {
        let a = long_art();
        let s = kw("", &a);
        assert!(s.starts_with("alpha beta") && s.ends_with('…') && s.chars().count() <= 161, "{s}");
        assert!(kw("zzzqqq", &a).starts_with("alpha beta"));
        assert!(kw("31452104 doi:10.1/x", &a).starts_with("alpha beta"));
    }

    #[test]
    fn snip_json_and_sn_alias() {
        assert_eq!(resolve("sn"), Some(Snip));
        assert_eq!(Snip.need(), Need::default());
        assert!(!Snip.is_long() && !EXPANDED.contains(&Snip));
    }

    #[test]
    fn snippet_pattern_globs_words_anywhere() {
        let re = |t: &str| regex::RegexBuilder::new(&lit_pattern(t, false).unwrap()).case_insensitive(true).build().unwrap();
        assert!(re("second* or safety").is_match("the secondary or safety outcome"));
        assert!(re("stat*in").is_match("a statin trial"));
        assert!(!re("stat*in").is_match("stating"));
        assert!(re("heart fail*").is_match("Heart failure with"));
    }

    #[test]
    fn first_and_last_author_skip_collectives() {
        let a = |n: &str, coll: bool| Author { name: n.into(), coll, ..Default::default() };
        let art = Article { authors: vec![a("Smith J", false), a("Lee K", false), a("EMPEROR Investigators", true)], ..Default::default() };
        assert_eq!(first_author(&art).unwrap().name, "Smith J");
        assert_eq!(last_author(&art).unwrap().name, "Lee K");
        let only = Article { authors: vec![a("Study Group", true)], ..Default::default() };
        assert_eq!(first_author(&only).unwrap().name, "Study Group");
        assert_eq!(last_author(&only).unwrap().name, "Study Group");
        let mut buf = String::new();
        let set = Set::default();
        write_value(&mut buf, &art, Field::Last, &Opts::new(&set, &Args::default()).unwrap());
        assert_eq!(buf, "Lee K");
    }
}
