use crate::args::Args;
use crate::eutils::SearchOpts;
use crate::fuzzy::{self, Person};
use crate::model::*;
use anyhow::{Result, anyhow, bail};
use regex::{Regex, RegexBuilder, RegexSet, RegexSetBuilder};
use std::cell::OnceCell;
use std::time::{SystemTime, UNIX_EPOCH};

const FUZZY_THR: f32 = 0.85;
const LIKE_THR: f32 = 0.9;
const WHERE_FIELDS: &str = "pmid year date epub age title abs text concl journal jt q sjr h cites rcr pct apt clin aff authors nauthors first last type mesh kw lang status nct country nrefs doi";
const MAX_NEAR: usize = 8;
const MAX_NEAR_DIST: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Relevance,
    Date,
    Pubdate,
    Author,
    Journal,
    Cites,
    Sjr,
    Rcr,
}

impl SortKey {
    fn parse(s: &str) -> Result<SortKey> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "relevance" | "rel" | "score" => SortKey::Relevance,
            "date" | "recent" | "most_recent" => SortKey::Date,
            "pubdate" | "pub_date" | "year" => SortKey::Pubdate,
            "author" | "first" => SortKey::Author,
            "journal" => SortKey::Journal,
            "cites" | "citations" => SortKey::Cites,
            "sjr" => SortKey::Sjr,
            "rcr" => SortKey::Rcr,
            _ => bail!("--sort: {s}? relevance|date|pubdate|author|journal|cites|sjr|rcr"),
        })
    }

    pub fn server(self) -> &'static str {
        match self {
            SortKey::Relevance | SortKey::Cites | SortKey::Sjr | SortKey::Rcr => "relevance",
            SortKey::Date => "most_recent",
            SortKey::Pubdate => "pub_date",
            SortKey::Author => "Author",
            SortKey::Journal => "JournalName",
        }
    }

    pub fn local(self) -> Option<&'static str> {
        match self {
            SortKey::Cites => Some("cites"),
            SortKey::Sjr => Some("sjr"),
            SortKey::Rcr => Some("rcr"),
            _ => None,
        }
    }
}

pub fn need_for_key(key: &str) -> Need {
    Need { rank: key == "sjr", metrics: matches!(key, "cites" | "rcr"), expand: false }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Ymd {
    pub y: i32,
    pub m: u8,
    pub d: u8,
}

fn days_in_month(y: i32, m: u8) -> u8 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn days_from_civil(y: i32, m: u8, d: u8) -> i64 {
    let y = y as i64 - (m <= 2) as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> Ymd {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    Ymd { y: (yoe + era * 400 + (m <= 2) as i64) as i32, m, d }
}

fn today_days() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() / 86400).unwrap_or(0) as i64
}

pub fn today() -> Ymd {
    civil_from_days(today_days())
}

impl Ymd {
    pub fn parse(s: &str) -> Result<Ymd> {
        let bad = || anyhow::anyhow!("bad date '{s}' (YYYY[-MM[-DD]])");
        let mut it = s.trim().split(['-', '/']);
        let y = it.next().filter(|p| p.len() == 4).and_then(|p| p.parse::<i32>().ok()).ok_or_else(bad)?;
        let m = match it.next() {
            Some(p) => p.parse::<u8>().ok().filter(|m| (1..=12).contains(m)).ok_or_else(bad)?,
            None => 0,
        };
        let d = match it.next() {
            Some(p) => p.parse::<u8>().ok().filter(|d| (1..=days_in_month(y, m.max(1))).contains(d)).ok_or_else(bad)?,
            None => 0,
        };
        if it.next().is_some() {
            return Err(bad());
        }
        Ok(Ymd { y, m, d })
    }

    pub fn lo(self) -> (i32, u8, u8) {
        (self.y, self.m.max(1), self.d.max(1))
    }

    pub fn hi(self) -> (i32, u8, u8) {
        let m = if self.m == 0 { 12 } else { self.m };
        (self.y, m, if self.d == 0 { days_in_month(self.y, m) } else { self.d })
    }

    pub fn esearch(self) -> String {
        match (self.m, self.d) {
            (0, _) => format!("{:04}", self.y),
            (m, 0) => format!("{:04}/{m:02}", self.y),
            (m, d) => format!("{:04}/{m:02}/{d:02}", self.y),
        }
    }

    fn shift(self, n: i64, unit: char) -> Ymd {
        match unit {
            'd' | 'w' => civil_from_days(days_from_civil(self.y, self.m, self.d) - n * if unit == 'w' { 7 } else { 1 }),
            _ => {
                let months = if unit == 'y' { n * 12 } else { n };
                let t = self.y as i64 * 12 + self.m as i64 - 1 - months;
                let (y, m) = ((t.div_euclid(12)) as i32, (t.rem_euclid(12) + 1) as u8);
                Ymd { y, m, d: self.d.min(days_in_month(y, m)) }
            }
        }
    }
}

pub fn parse_last(s: &str, today: Ymd) -> Result<(Ymd, char), &'static str> {
    let s = s.trim().to_ascii_lowercase();
    let unit = s.chars().last().filter(|c| matches!(c, 'd' | 'w' | 'm' | 'y')).ok_or("bad")?;
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err("bad");
    }
    let max = match unit {
        'd' => 3_650_000,
        'w' => 520_000,
        'm' => 120_000,
        _ => 10_000,
    };
    match digits.parse::<i64>() {
        Ok(n) if n <= max => Ok((today.shift(n, unit), unit)),
        _ => Err("too large"),
    }
}

fn year_range(s: &str) -> Result<(Ymd, Ymd)> {
    let parts: Vec<&str> = s.split([':', '–']).collect();
    let parts = if parts.len() == 1 {
        let p: Vec<&str> = s.split('-').collect();
        if p.len() == 2 && p[0].len() == 4 && p[1].len() == 4 { p } else { parts }
    } else {
        parts
    };
    match parts.as_slice() {
        [a, b] => Ok((Ymd::parse(a)?, Ymd::parse(b)?)),
        _ => {
            let d = Ymd::parse(s)?;
            Ok((d, d))
        }
    }
}

type Triple = (i32, u8, u8);

fn art_span(a: &Article) -> Option<(Triple, Triple)> {
    span_of(&a.date, a.year)
}

fn valid_date(v: &str) -> bool {
    let mut it = v.split('-');
    let ok = |p: Option<&str>, lo: u32, hi: u32| p.is_none_or(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u32>().is_ok_and(|n| (lo..=hi).contains(&n)));
    let y = it.next();
    let (m, d) = (it.next(), it.next());
    y.is_some_and(|y| y.len() == 4 && ok(Some(y), 1, 9999)) && ok(m, 1, 12) && ok(d, 1, 31) && it.next().is_none() && (m.is_some() || d.is_none())
}

fn span_of(date: &str, year: u16) -> Option<(Triple, Triple)> {
    let mut it = date.split(['-', '/', ' ']);
    let y = it.next().and_then(|p| p.parse::<i32>().ok()).filter(|y| *y > 0).or((year > 0).then_some(year as i32))?;
    let mut num = |lim: u8| it.next().and_then(|p| p.parse::<u8>().ok()).filter(|v| (1..=lim).contains(v)).unwrap_or(0);
    let m = num(12);
    let d = if m > 0 { num(31) } else { 0 };
    let d0 = Ymd { y, m, d };
    Some((d0.lo(), d0.hi()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Ti,
    Ab,
    Tiab,
    Concl,
}

struct Hay<'a> {
    a: &'a Article,
    raw: [OnceCell<String>; 4],
    fold: [OnceCell<String>; 4],
    kw: OnceCell<Vec<String>>,
}

impl<'a> Hay<'a> {
    fn new(a: &'a Article) -> Hay<'a> {
        Hay { a, raw: Default::default(), fold: Default::default(), kw: OnceCell::new() }
    }

    fn raw(&self, p: Part) -> &str {
        match p {
            Part::Ti if self.a.coll_title.is_empty() => &self.a.title,
            Part::Ti => self.raw[0].get_or_init(|| format!("{}\n{}", self.a.title, self.a.coll_title)),
            Part::Ab => self.raw[1].get_or_init(|| {
                let mut s = self.a.abstract_text();
                if !self.a.other_abs.is_empty() {
                    s.push('\n');
                    s.push_str(&self.a.other_abs);
                }
                s
            }),
            Part::Tiab => self.raw[2].get_or_init(|| {
                let (ti, ab) = (self.raw(Part::Ti), self.raw(Part::Ab));
                let mut s = String::with_capacity(ti.len() + ab.len() + 1);
                s.push_str(ti);
                s.push('\n');
                s.push_str(ab);
                s
            }),
            Part::Concl => self.raw[3].get_or_init(|| concl_text(self.a)),
        }
    }

    fn folded(&self, p: Part) -> &str {
        self.fold[p as usize].get_or_init(|| fuzzy::fold_padded(self.raw(p)))
    }

    fn keywords(&self) -> &[String] {
        self.kw.get_or_init(|| self.a.kw.iter().map(|k| fuzzy::fold_padded(k)).collect())
    }
}

fn concl_text(a: &Article) -> String {
    crate::fields::conclusion_sections(a).join(" ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WF {
    Pmid,
    Year,
    Date,
    Epub,
    Age,
    Title,
    Abs,
    Text,
    Concl,
    Journal,
    Jt,
    Q,
    Sjr,
    H,
    Cites,
    Rcr,
    Pct,
    Apt,
    Clin,
    Aff,
    Authors,
    Nauthors,
    First,
    Last,
    Type,
    Mesh,
    Kw,
    Lang,
    Status,
    Nct,
    Country,
    Nrefs,
    Doi,
}

impl WF {
    fn parse(s: &str) -> Option<WF> {
        Some(match s {
            "pmid" => WF::Pmid,
            "year" => WF::Year,
            "date" => WF::Date,
            "epub" => WF::Epub,
            "age" => WF::Age,
            "title" => WF::Title,
            "abs" | "abstract" => WF::Abs,
            "text" => WF::Text,
            "concl" | "conclusion" | "conclusions" => WF::Concl,
            "journal" => WF::Journal,
            "jt" => WF::Jt,
            "q" => WF::Q,
            "sjr" => WF::Sjr,
            "h" => WF::H,
            "cites" | "ncites" => WF::Cites,
            "rcr" => WF::Rcr,
            "pct" => WF::Pct,
            "apt" => WF::Apt,
            "clin" => WF::Clin,
            "aff" => WF::Aff,
            "authors" => WF::Authors,
            "nauthors" => WF::Nauthors,
            "first" => WF::First,
            "last" => WF::Last,
            "type" => WF::Type,
            "mesh" => WF::Mesh,
            "kw" => WF::Kw,
            "lang" => WF::Lang,
            "status" => WF::Status,
            "nct" => WF::Nct,
            "country" => WF::Country,
            "nrefs" => WF::Nrefs,
            "doi" => WF::Doi,
            _ => return None,
        })
    }

    fn need(self) -> Need {
        match self {
            WF::Q | WF::Sjr | WF::H => Need { rank: true, ..Need::default() },
            WF::Cites | WF::Rcr | WF::Pct | WF::Apt | WF::Clin => Need { metrics: true, ..Need::default() },
            _ => Need::default(),
        }
    }

    fn is_num(self) -> bool {
        matches!(
            self,
            WF::Pmid | WF::Year | WF::Age | WF::Q | WF::Sjr | WF::H | WF::Cites | WF::Rcr | WF::Pct | WF::Apt | WF::Clin | WF::Nauthors | WF::Nrefs
        )
    }

    fn orderable_text(self) -> bool {
        matches!(
            self,
            WF::Date | WF::Epub | WF::Title | WF::Abs | WF::Text | WF::Concl | WF::Jt | WF::Status | WF::Doi | WF::Country | WF::First | WF::Last
        )
    }

    fn part(self) -> Option<Part> {
        match self {
            WF::Title => Some(Part::Ti),
            WF::Abs => Some(Part::Ab),
            WF::Text => Some(Part::Tiab),
            WF::Concl => Some(Part::Concl),
            _ => None,
        }
    }
}

enum V<'h> {
    N(Option<f64>),
    S(&'h str),
    L(Vec<&'h str>),
}

fn num<T: Into<f64>>(v: Option<T>) -> V<'static> {
    V::N(v.map(Into::into))
}

fn age_days(a: &Article, now: i64) -> Option<i64> {
    let ((y, m, d), _) = art_span(a)?;
    Some((now - days_from_civil(y, m, d)).max(0))
}

fn value<'h>(h: &'h Hay, f: WF) -> V<'h> {
    let a = h.a;
    let m = a.m.as_ref();
    let jr = a.jr.as_ref();
    match f {
        WF::Pmid => num(Some(a.pmid)),
        WF::Year => num((a.year > 0).then_some(a.year)),
        WF::Date => V::S(&a.date),
        WF::Epub if a.epub.is_empty() => V::N(None),
        WF::Epub => V::S(&a.epub),
        WF::Age => V::N(age_days(a, today_days()).map(|d| d as f64)),
        WF::Title => V::S(&a.title),
        WF::Text => V::L(vec![h.raw(Part::Ti), h.raw(Part::Ab)]),
        WF::Abs | WF::Concl => V::S(h.raw(f.part().unwrap_or(Part::Ab))),
        WF::Journal => V::L(vec![&a.journal.iso, &a.journal.title]),
        WF::Jt => V::S(&a.journal.title),
        WF::Q => num(jr.map(|j| j.q)),
        WF::Sjr => num(jr.map(|j| j.sjr)),
        WF::H => num(jr.map(|j| j.h)),
        WF::Cites => num(m.map(|m| m.cites)),
        WF::Rcr => num(m.and_then(|m| m.rcr)),
        WF::Pct => num(m.and_then(|m| m.pct)),
        WF::Apt => num(m.and_then(|m| m.apt)),
        WF::Clin => V::N(m.map(|m| m.clin as u8 as f64)),
        WF::Aff => V::L(a.affs.iter().map(String::as_str).collect()),
        WF::Authors => V::L(a.authors.iter().map(|x| x.name.as_str()).collect()),
        WF::Nauthors => num(Some(a.authors.len() as u32)),
        WF::First => V::L(a.authors.iter().find(|x| !x.coll).map(|x| x.name.as_str()).into_iter().collect()),
        WF::Last => V::L(a.authors.iter().rev().find(|x| !x.coll).map(|x| x.name.as_str()).into_iter().collect()),
        WF::Type => V::L(a.types.iter().map(String::as_str).collect()),
        WF::Mesh => V::L(a.mesh.iter().map(|x| x.name.as_str()).collect()),
        WF::Kw => V::L(a.kw.iter().map(String::as_str).collect()),
        WF::Lang => V::L(a.langs.iter().map(String::as_str).collect()),
        WF::Status => V::S(&a.status),
        WF::Nct => V::L(ncts(a)),
        WF::Country => V::S(&a.journal.country),
        WF::Nrefs => num(Some(a.nrefs)),
        WF::Doi => V::S(&a.doi),
    }
}

const PROBLEMS_QUERY: &str = "(hasretractionin OR hasretractionof OR haserratumin OR haserratumfor OR hasexpressionofconcernin OR hasexpressionofconcernfor OR \"Retracted Publication\"[pt] OR \"Retraction Notice\"[pt] OR \"Expression of Concern\"[pt] OR \"Published Erratum\"[pt])";

fn notice_or_problem(a: &Article) -> bool {
    a.corrs.iter().any(|c| {
        matches!(
            c.kind.as_str(),
            "RetractionIn" | "RetractionOf" | "ErratumIn" | "ErratumFor" | "ExpressionOfConcernIn" | "ExpressionOfConcernFor"
        )
    }) || a.types.iter().any(|t| {
        matches!(t.as_str(), "Retracted Publication" | "Retraction Notice" | "Expression of Concern" | "Published Erratum")
    })
}

fn ncts(a: &Article) -> Vec<&str> {
    a.banks
        .iter()
        .flat_map(|(_, ids)| ids.iter())
        .filter(|i| i.to_ascii_uppercase().starts_with("NCT"))
        .map(String::as_str)
        .collect()
}

fn rx_err(p: &str, e: &regex::Error) -> String {
    let s = e.to_string();
    let msg = s.lines().rev().find_map(|l| l.strip_prefix("error: ")).or_else(|| s.lines().next()).unwrap_or("");
    format!("bad regex '{p}': {msg}")
}

fn rx_one(p: &str) -> Result<Regex, String> {
    RegexBuilder::new(p).case_insensitive(true).build().map_err(|e| rx_err(p, &e))
}

#[derive(Debug, Clone)]
enum Rx {
    One(Regex),
    Set(RegexSet),
}

impl Rx {
    fn new(pats: &[&str]) -> Result<Option<Rx>> {
        let mut rs = pats.iter().map(|p| rx_one(p)).collect::<Result<Vec<_>, String>>().map_err(|e| anyhow!(e))?;
        Ok(match rs.len() {
            0 => None,
            1 => rs.pop().map(Rx::One),
            _ => Some(Rx::Set(RegexSetBuilder::new(pats).case_insensitive(true).build()?)),
        })
    }

    fn any2(&self, a: &str, b: &str) -> bool {
        match self {
            Rx::One(r) => r.is_match(a) || (!b.is_empty() && r.is_match(b)),
            Rx::Set(s) => s.is_match(a) || (!b.is_empty() && s.is_match(b)),
        }
    }

    fn all2(&self, a: &str, b: &str) -> bool {
        match self {
            Rx::One(r) => r.is_match(a) || (!b.is_empty() && r.is_match(b)),
            Rx::Set(s) => {
                let (x, y) = (s.matches(a), if b.is_empty() { None } else { Some(s.matches(b)) });
                (0..s.len()).all(|i| x.matched(i) || y.as_ref().is_some_and(|y| y.matched(i)))
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Cmp {
    op: Op,
    val: String,
    fval: String,
    n: Option<f64>,
}

#[derive(Debug, Clone, Default)]
struct In {
    strs: Vec<String>,
    nums: Vec<f64>,
    sels: Vec<Sel>,
    dates: Vec<Cmp>,
}

#[derive(Debug, Clone)]
enum Test {
    Cmp(Cmp),
    Like(fuzzy::Needle, String),
    Rx(Regex),
    Fz(fuzzy::Typo),
    In(In),
}

#[derive(Debug, Clone)]
struct Clause {
    f: WF,
    t: Test,
    neg: bool,
}

#[derive(Debug, Clone)]
enum Expr {
    Cmp(Clause),
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
}

impl Expr {
    fn eval(&self, h: &Hay) -> bool {
        match self {
            Expr::Cmp(c) => c.eval(h),
            Expr::Not(e) => !e.eval(h),
            Expr::And(v) => v.iter().all(|e| e.eval(h)),
            Expr::Or(v) => v.iter().any(|e| e.eval(h)),
        }
    }

    fn need(&self) -> Need {
        match self {
            Expr::Cmp(c) => c.f.need(),
            Expr::Not(e) => e.need(),
            Expr::And(v) | Expr::Or(v) => v.iter().fold(Need::default(), |n, e| n.or(e.need())),
        }
    }

    fn has_rx(&self) -> bool {
        match self {
            Expr::Cmp(c) => matches!(c.t, Test::Rx(_)),
            Expr::Not(e) => e.has_rx(),
            Expr::And(v) | Expr::Or(v) => v.iter().any(Expr::has_rx),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum OpK {
    Cmp(Op),
    Like,
    Rx,
    Fz,
    In,
}

const OPS: [(&str, OpK, bool); 13] = [
    ("!~~", OpK::Rx, true),
    ("~~", OpK::Rx, false),
    ("!~", OpK::Like, true),
    ("~", OpK::Like, false),
    ("!%", OpK::Fz, true),
    ("%", OpK::Fz, false),
    ("!=", OpK::Cmp(Op::Eq), true),
    ("==", OpK::Cmp(Op::Eq), false),
    ("=", OpK::Cmp(Op::Eq), false),
    (">=", OpK::Cmp(Op::Ge), false),
    ("<=", OpK::Cmp(Op::Le), false),
    (">", OpK::Cmp(Op::Gt), false),
    ("<", OpK::Cmp(Op::Lt), false),
];

fn build_in(f: WF, items: &[String], lhs: &str) -> Result<In, String> {
    let mut i = In::default();
    for v in items {
        if f == WF::Type {
            let sel = type_sel(v);
            if let Some((w, _)) = server_sb(&sel) {
                return Err(w.into());
            }
            i.sels.push(sel);
        } else if f.is_num() {
            i.nums.push(v.parse().map_err(|_| format!("'{lhs}' needs numbers, got '{v}'"))?);
        } else if matches!(f, WF::Date | WF::Epub) {
            let val = v.replace('/', "-");
            if !valid_date(&val) {
                return Err(format!("'{lhs}' needs dates (YYYY[-MM[-DD]]), got '{v}'"));
            }
            i.dates.push(Cmp { op: Op::Eq, fval: fuzzy::fold(&val), val, n: None });
        } else if f == WF::Lang {
            i.strs.extend(lang_names(v).1);
        } else {
            i.strs.push(fuzzy::fold(v));
        }
    }
    Ok(i)
}

fn build(f: WF, k: OpK, neg: bool, rhs: &[String], lhs: &str) -> Result<Clause, String> {
    let v = &rhs[0];
    let t = match k {
        OpK::Cmp(op) => {
            let n = v.parse::<f64>().ok();
            if op != Op::Eq && n.is_none() && !f.orderable_text() {
                return Err(format!("'{lhs}' needs a number, got '{v}'"));
            }
            if op == Op::Eq && matches!(f, WF::Type | WF::Lang) {
                Test::In(build_in(f, rhs, lhs)?)
            } else {
                let val = if matches!(f, WF::Date | WF::Epub) { v.replace('/', "-") } else { v.clone() };
                if matches!(f, WF::Date | WF::Epub) && op != Op::Eq && !valid_date(&val) {
                    return Err(format!("'{lhs}' needs a date (YYYY[-MM[-DD]]), got '{v}'"));
                }
                Test::Cmp(Cmp { op, fval: fuzzy::fold(&val), val, n })
            }
        }
        OpK::Like => Test::Like(fuzzy::Needle::new(v), v.clone()),
        OpK::Rx => Test::Rx(rx_one(v)?),
        OpK::Fz => {
            let t = fuzzy::Typo::new(v);
            if t.is_empty() {
                return Err(format!("empty typo term in '{lhs}'"));
            }
            Test::Fz(t)
        }
        OpK::In => Test::In(build_in(f, rhs, lhs)?),
    };
    Ok(Clause { f, t, neg })
}

const MAX_WHERE_LEN: usize = 4000;
const MAX_WHERE_DEPTH: usize = 64;

struct Parser<'a> {
    s: &'a str,
    i: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn fail<T>(&self, at: usize, msg: impl std::fmt::Display) -> Result<T> {
        bail!("where: {msg} @{}", at + 1)
    }

    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    fn ws(&mut self) {
        self.i = self.s.len() - self.rest().trim_start().len();
    }

    fn eat(&mut self, t: &str) -> bool {
        let ok = self.rest().starts_with(t);
        if ok {
            self.i += t.len();
        }
        ok
    }

    fn word(&mut self, w: &str) -> bool {
        let ok = self.rest().strip_prefix(w).is_some_and(|r| !r.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'));
        if ok {
            self.i += w.len();
        }
        ok
    }

    fn or(&mut self) -> Result<Expr> {
        let mut v = vec![self.and()?];
        loop {
            self.ws();
            if !self.eat("||") {
                break;
            }
            v.push(self.and()?);
        }
        Ok(if v.len() == 1 { v.remove(0) } else { Expr::Or(v) })
    }

    fn and(&mut self) -> Result<Expr> {
        let mut v = vec![self.unary()?];
        loop {
            self.ws();
            if !self.eat("&") {
                break;
            }
            self.eat("&");
            v.push(self.unary()?);
        }
        Ok(if v.len() == 1 { v.remove(0) } else { Expr::And(v) })
    }

    fn unary(&mut self) -> Result<Expr> {
        if self.depth >= MAX_WHERE_DEPTH {
            bail!("where: too deeply nested");
        }
        self.depth += 1;
        let r = self.unary_inner();
        self.depth -= 1;
        r
    }

    fn unary_inner(&mut self) -> Result<Expr> {
        self.ws();
        let kw = self.rest().strip_prefix("not").is_some_and(|r| r.starts_with(|c: char| c.is_whitespace() || c == '(' || c == '!'));
        if self.eat("!") || (kw && self.eat("not")) {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.eat("(") {
            let e = self.or()?;
            self.ws();
            if !self.eat(")") {
                return self.fail(self.i, "expected ')'");
            }
            return Ok(e);
        }
        self.cmp()
    }

    fn quoted(&mut self) -> Result<Option<&'a str>> {
        let r = self.rest();
        let Some(q) = r.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
            return Ok(None);
        };
        let Some(end) = r[1..].find(q) else {
            return self.fail(self.i, "unterminated quote");
        };
        self.i += end + 2;
        Ok(Some(&r[1..1 + end]))
    }

    fn bare_value(&mut self) -> &'a str {
        let r = self.rest();
        let mut depth = 0;
        let mut end = r.len();
        for (j, c) in r.char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth > 0 => depth -= 1,
                ')' | '&' => {
                    end = j;
                    break;
                }
                '|' if r[j..].starts_with("||") => {
                    end = j;
                    break;
                }
                _ => {}
            }
        }
        self.i += end;
        r[..end].trim()
    }

    fn bare_item(&mut self) -> &'a str {
        let r = self.rest();
        let end = r
            .char_indices()
            .find(|&(j, c)| matches!(c, ',' | ')' | '&') || (c == '|' && r[j..].starts_with("||")))
            .map_or(r.len(), |(j, _)| j);
        self.i += end;
        r[..end].trim()
    }

    fn value(&mut self, lhs: &str) -> Result<String> {
        self.ws();
        let at = self.i;
        let v = match self.quoted()? {
            Some(q) => {
                self.ws();
                if !self.rest().is_empty() && !self.rest().starts_with(['&', '|', ')']) {
                    return self.fail(self.i, format!("unexpected '{}' after value", self.rest().chars().next().unwrap_or(' ')));
                }
                q.trim()
            }
            None => self.bare_value(),
        };
        if v.is_empty() {
            return self.fail(at, format!("empty value in '{lhs}'"));
        }
        Ok(v.to_string())
    }

    fn list(&mut self) -> Result<Vec<String>> {
        self.ws();
        let paren = self.eat("(");
        let mut out = Vec::new();
        loop {
            self.ws();
            let at = self.i;
            let v = match self.quoted()? {
                Some(q) => q.trim(),
                None => self.bare_item(),
            };
            if v.is_empty() {
                return self.fail(at, "empty list item");
            }
            out.push(v.to_string());
            self.ws();
            if paren && self.eat(")") {
                return Ok(out);
            }
            if !self.eat(",") {
                if paren {
                    return self.fail(self.i, "expected ',' or ')'");
                }
                return Ok(out);
            }
        }
    }

    fn cmp(&mut self) -> Result<Expr> {
        let at = self.i;
        let r = self.rest();
        let len = r.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(r.len());
        if len == 0 {
            return match r.chars().next() {
                Some(c) => self.fail(at, format!("expected field, got '{c}'")),
                None => self.fail(at, "expected condition"),
            };
        }
        let name = r[..len].to_ascii_lowercase();
        self.i += len;
        let Some(f) = WF::parse(&name) else {
            return self.fail(at, format!("unknown field '{name}' ({WHERE_FIELDS})"));
        };
        self.ws();
        let op_at = self.i;
        let mut op = OPS.iter().find(|(t, _, _)| self.eat(t)).map(|(t, k, n)| (*t, *k, *n));
        if op.is_none() {
            let neg = self.word("not");
            self.ws();
            if self.word("in") {
                op = Some((if neg { "not in" } else { "in" }, OpK::In, neg));
            }
        }
        let Some((opname, k, neg)) = op else {
            return self.fail(op_at, format!("no operator after '{name}' (= != > >= < <= ~ !~ ~~ !~~ % in)"));
        };
        let lhs = format!("{name}{}{opname}", if opname.ends_with("in") { " " } else { "" });
        self.ws();
        let vat = self.i;
        let rhs = if matches!(k, OpK::In) { self.list()? } else { vec![self.value(&lhs)?] };
        match build(f, k, neg, &rhs, &lhs) {
            Ok(c) => Ok(Expr::Cmp(c)),
            Err(e) => self.fail(vat, e),
        }
    }
}

fn parse_where(s: &str) -> Result<Expr> {
    if s.trim().is_empty() {
        bail!("where: empty expression");
    }
    if s.len() > MAX_WHERE_LEN {
        bail!("where: expression too long (max {MAX_WHERE_LEN} chars)");
    }
    let mut p = Parser { s, i: 0, depth: 0 };
    let e = p.or()?;
    p.ws();
    if let Some(c) = p.rest().chars().next() {
        return p.fail(p.i, format!("unexpected '{c}'"));
    }
    Ok(e)
}

fn cmp_ord(o: std::cmp::Ordering, op: Op) -> bool {
    use std::cmp::Ordering::*;
    match op {
        Op::Eq => o == Equal,
        Op::Gt => o == Greater,
        Op::Ge => o != Less,
        Op::Lt => o == Less,
        Op::Le => o != Greater,
    }
}

impl Cmp {
    fn date(&self, s: &str) -> bool {
        if !valid_date(&self.val) {
            return false;
        }
        let (Some((alo, ahi)), Some((llo, lhi))) = (span_of(s, 0), span_of(&self.val, 0)) else {
            return false;
        };
        match self.op {
            Op::Eq => alo >= llo && ahi <= lhi,
            Op::Lt => alo < llo,
            Op::Le => alo <= lhi,
            Op::Gt => ahi > lhi,
            Op::Ge => ahi >= llo,
        }
    }

    fn text(&self, f: WF, s: &str) -> bool {
        if matches!(f, WF::Date | WF::Epub) {
            return self.date(s);
        }
        if self.op == Op::Eq {
            return fuzzy::fold(s) == self.fval;
        }
        if let (Ok(x), Some(n)) = (s.trim().parse::<f64>(), self.n) {
            x.partial_cmp(&n).is_some_and(|o| cmp_ord(o, self.op))
        } else {
            cmp_ord(fuzzy::fold(s).cmp(&self.fval), self.op)
        }
    }
}

impl Clause {
    fn eval(&self, h: &Hay) -> bool {
        let hit = match (&self.t, self.f) {
            (Test::In(i), WF::Type) => i.sels.iter().any(|s| s.eval(h.a)),
            _ => match value(h, self.f) {
                V::N(None) => false,
                V::N(Some(x)) => self.number(x),
                V::S(s) => self.text(s, self.f.part().map(|p| h.folded(p))),
                V::L(l) => l.iter().any(|s| self.text(s, None)),
            },
        };
        hit != self.neg
    }

    fn number(&self, x: f64) -> bool {
        match &self.t {
            Test::Cmp(c) => c.n.is_some_and(|n| x.partial_cmp(&n).is_some_and(|o| cmp_ord(o, c.op))),
            Test::In(i) => i.nums.contains(&x),
            Test::Like(_, v) => x.to_string().contains(v.as_str()),
            _ => self.text(&x.to_string(), None),
        }
    }

    fn text(&self, s: &str, folded: Option<&str>) -> bool {
        let fold = |f: &dyn Fn(&str) -> bool| match folded {
            Some(h) => f(h),
            None => f(&fuzzy::fold_padded(s)),
        };
        match &self.t {
            Test::Cmp(c) => c.text(self.f, s),
            Test::Like(n, _) => fold(&|h| n.matches(h, LIKE_THR)),
            Test::Rx(r) => r.is_match(s),
            Test::Fz(t) => fold(&|h| t.in_text(h)),
            Test::In(i) if matches!(self.f, WF::Date | WF::Epub) => i.dates.iter().any(|c| c.date(s)),
            Test::In(i) => i.strs.contains(&fuzzy::fold(s)),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Sel {
    Pt(String),
    Sb(String),
    Mh(String),
    Or(Vec<Sel>),
}

const SERVER_SB: &[(&str, &str, &str)] = &[
    ("free full text", "free full text is server-side; use search", "filter: free full text is server-side; use search"),
    ("full text", "full text is server-side; use search", "filter: full text is server-side; use search"),
    ("pubmed pmc", "pubmed pmc is server-side; use search", "filter: pubmed pmc is server-side; use search"),
    (
        "pubmed pmc open access",
        "pubmed pmc open access is server-side; use search",
        "filter: pubmed pmc open access is server-side; use search",
    ),
    ("medline", "medline is server-side; use search", "filter: medline is server-side; use search"),
    ("systematic", "systematic[sb] is server-side; use search", "filter: systematic[sb] is server-side; use search"),
    (
        "Cohort Studies",
        "cohort uses MeSH explosion (server-side); use search",
        "filter: cohort uses MeSH explosion (server-side); use search",
    ),
    (
        "Case-Control Studies",
        "case control uses MeSH explosion (server-side); use search",
        "filter: case control uses MeSH explosion (server-side); use search",
    ),
];

fn server_sb(s: &Sel) -> Option<(&'static str, &'static str)> {
    match s {
        Sel::Sb(x) | Sel::Mh(x) => SERVER_SB.iter().find(|(k, _, _)| k == x).map(|(_, w, f)| (*w, *f)),
        Sel::Or(v) => v.iter().find_map(server_sb),
        _ => None,
    }
}

fn type_sel(s: &str) -> Sel {
    let s = s.trim().trim_matches('"').trim();
    let k = fuzzy::fold(s);
    let pt = |x: &str| Sel::Pt(x.to_string());
    match k.as_str() {
        "rct" | "randomized" | "randomised" | "randomized controlled trial" => pt("Randomized Controlled Trial"),
        "review" | "reviews" => pt("Review"),
        "meta" | "meta analysis" | "metaanalysis" => pt("Meta-Analysis"),
        "sr" | "sys" | "systematic" | "systematic review" => Sel::Sb("systematic".into()),
        "trial" | "ct" | "clinical trial" => pt("Clinical Trial"),
        "case" | "cases" | "case report" | "case reports" => pt("Case Reports"),
        "guideline" | "guidelines" => Sel::Or(vec![pt("Practice Guideline"), pt("Guideline")]),
        "editorial" => pt("Editorial"),
        "letter" => pt("Letter"),
        "comment" => pt("Comment"),
        "news" => pt("News"),
        "obs" | "observational" => pt("Observational Study"),
        "cohort" => Sel::Mh("Cohort Studies".into()),
        "case control" => Sel::Mh("Case-Control Studies".into()),
        "protocol" => pt("Clinical Trial Protocol"),
        "erratum" => pt("Published Erratum"),
        "retracted" | "retraction" => pt("Retracted Publication"),
        "preprint" => pt("Preprint"),
        "article" | "journal" => pt("Journal Article"),
        "multicenter" => pt("Multicenter Study"),
        "comparative" => pt("Comparative Study"),
        "validation" => pt("Validation Study"),
        "phase1" | "phase i" => pt("Clinical Trial, Phase I"),
        "phase2" | "phase ii" => pt("Clinical Trial, Phase II"),
        "phase3" | "phase iii" => pt("Clinical Trial, Phase III"),
        "phase4" | "phase iv" => pt("Clinical Trial, Phase IV"),
        "systematic reviews" => Sel::Sb("systematic".into()),
        "free full text" | "full text" | "pubmed pmc" | "pubmed pmc open access" | "ahead of print" | "medline" => Sel::Sb(k),
        _ => Sel::Pt(s.to_string()),
    }
}

impl Sel {
    fn server(&self) -> String {
        match self {
            Sel::Pt(x) => qtag(x, "pt"),
            Sel::Sb(x) => qtag(x, "sb"),
            Sel::Mh(x) => qtag(x, "mh"),
            Sel::Or(v) => or(v.iter().map(Sel::server).collect()),
        }
    }

    fn eval(&self, a: &Article) -> bool {
        match self {
            Sel::Pt(x) => a.types.iter().any(|t| pt_is(t, x)),
            Sel::Mh(x) => a.mesh.iter().any(|m| m.name.eq_ignore_ascii_case(x)),
            Sel::Or(v) => v.iter().any(|s| s.eval(a)),
            Sel::Sb(x) => match x.as_str() {
                "systematic" => a.types.iter().any(|t| matches!(t.as_str(), "Systematic Review" | "Meta-Analysis")),
                "free full text" | "pubmed pmc" | "pubmed pmc open access" => !a.pmc.is_empty(),
                "full text" => !a.pmc.is_empty() || !a.doi.is_empty(),
                "ahead of print" => is_aop(a),
                _ => true,
            },
        }
    }
}

const PT_CHILDREN: &[(&str, &[&str])] = &[
    (
        "Clinical Study",
        &[
            "Clinical Trial",
            "Observational Study",
            "Clinical Trial Protocol",
            "Clinical Trial, Veterinary",
            "Observational Study, Veterinary",
        ],
    ),
    ("Clinical Trial, Veterinary", &["Randomized Controlled Trial, Veterinary"]),
    (
        "Clinical Trial",
        &[
            "Controlled Clinical Trial",
            "Clinical Trial, Phase I",
            "Clinical Trial, Phase II",
            "Clinical Trial, Phase III",
            "Clinical Trial, Phase IV",
            "Pragmatic Clinical Trial",
            "Equivalence Trial",
            "Adaptive Clinical Trial",
        ],
    ),
    ("Controlled Clinical Trial", &["Randomized Controlled Trial"]),
    ("Randomized Controlled Trial", &["Equivalence Trial"]),
    (
        "Review",
        &[
            "Evidence Synthesis",
            "Systematic Review",
            "Scoping Review",
            "Consensus Development Conference, NIH",
            "Consensus Statement",
            "Guideline",
        ],
    ),
    ("Evidence Synthesis", &["Systematic Review", "Scoping Review"]),
    ("Guideline", &["Practice Guideline"]),
    ("Meta-Analysis", &["Network Meta-Analysis"]),
];

fn pt_is(have: &str, want: &str) -> bool {
    if want.contains('*') {
        return glob_hit(&have.to_lowercase(), &want.to_lowercase());
    }
    have.eq_ignore_ascii_case(want)
        || PT_CHILDREN
            .iter()
            .find(|(p, _)| p.eq_ignore_ascii_case(want))
            .is_some_and(|(_, kids)| kids.iter().any(|k| pt_is(have, k)))
}

fn is_aop(a: &Article) -> bool {
    let s = a.status.to_ascii_lowercase();
    s.contains("ahead") || s == "aop"
}

fn is_preprint(a: &Article) -> bool {
    a.types.iter().any(|t| t.eq_ignore_ascii_case("preprint")) || a.status.to_ascii_lowercase().contains("preprint")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Published,
    Unpublished,
    Aop,
    Preprint,
    Retracted,
    NoRetracted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flag {
    Free,
    Fulltext,
    Abstract,
    Data,
    HasComments,
    HasProblems,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Any,
    First,
    Last,
}

#[derive(Debug, Clone)]
enum Kind {
    Terms { p: Part, needles: Vec<String>, any: bool, not: bool },
    Tagged { items: Vec<Tm>, any: bool, not: bool, partial: bool },
    Mesh { names: Vec<String>, major: bool },
    MeshAny(Vec<String>),
    Tag(String),
    Kw(String),
    Author { p: Person, which: Which, fuzzy: bool },
    Aff { alts: Vec<Vec<String>>, fuzzy: bool },
    Country(Vec<String>),
    Journal { names: Vec<String>, fuzzy: bool },
    Issn(Vec<String>),
    Date { from: Option<Ymd>, to: Option<Ymd>, epub: bool },
    Types { sels: Vec<Sel>, not: bool },
    Status(Status),
    Species(bool),
    Lang(Vec<String>),
    Flag(Flag),
    Nct(Option<String>),
    Grant(String),
    Quartile(Vec<u8>),
    SjrMin(f32),
    HMin(u32),
    Area(String),
    CitesMin(u32),
    RcrMin(f32),
    Clinical,
    Where(Expr),
    Rx { part: Part, rx: Rx, none: bool },
    Fz(fuzzy::Typo),
    Phrase(Vec<String>),
    Near { words: Vec<String>, span: usize },
}

#[derive(Debug, Clone)]
struct Check {
    k: Kind,
    post: bool,
    enr: bool,
}

const GLOB: char = '\u{1}';

fn text_has(h: &Hay, p: Part, needle: &str) -> bool {
    if let Some(pat) = needle.strip_prefix(GLOB) {
        let words = phrase_words(pat);
        let hit = |f| phrase_hit(h.folded(f), &words);
        return match p {
            Part::Tiab => hit(Part::Ti) || hit(Part::Ab) || h.keywords().iter().any(|k| phrase_hit(k, &words)),
            _ => hit(p),
        };
    }
    match p {
        Part::Tiab => {
            h.folded(Part::Ti).contains(needle)
                || h.folded(Part::Ab).contains(needle)
                || h.keywords().iter().any(|k| k.contains(needle))
        }
        _ => h.folded(p).contains(needle),
    }
}

fn phrase_words(p: &str) -> Vec<String> {
    p.split(|c: char| !(c.is_alphanumeric() || matches!(c, '*' | '\'' | '’' | '`' | '\u{0300}'..='\u{036f}')))
        .map(|w| w.split('*').map(fuzzy::fold).collect::<Vec<_>>().join("*"))
        .filter(|g| g.chars().any(|c| c != '*'))
        .collect()
}

fn glob_hit(tok: &str, pat: &str) -> bool {
    if !pat.contains('*') {
        return tok == pat;
    }
    let parts: Vec<&str> = pat.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !tok.starts_with(first) || tok.len() < first.len() + last.len() || !tok.ends_with(last) {
        return false;
    }
    let mut rest = &tok[first.len()..tok.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

fn phrase_hit(folded: &str, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let toks: Vec<&str> = folded.split(' ').filter(|t| !t.is_empty()).collect();
    toks.windows(words.len()).any(|w| w.iter().zip(words).all(|(t, x)| glob_hit(t, x)))
}

fn near_hit(folded: &str, words: &[String], span: usize) -> bool {
    let mut last = [usize::MAX; MAX_NEAR];
    for (pos, tok) in folded.split(' ').filter(|t| !t.is_empty()).enumerate() {
        let Some(j) = words.iter().position(|w| w == tok) else { continue };
        last[j] = pos;
        let seen = &last[..words.len()];
        if !seen.contains(&usize::MAX) && seen.iter().min().is_some_and(|lo| pos - lo <= span) {
            return true;
        }
    }
    false
}

impl Check {
    fn new(k: Kind, post: bool) -> Check {
        let enr = match &k {
            Kind::Quartile(_) | Kind::SjrMin(_) | Kind::HMin(_) | Kind::Area(_) | Kind::CitesMin(_) | Kind::RcrMin(_) | Kind::Clinical => true,
            Kind::Where(e) => e.need() != Need::default(),
            _ => false,
        };
        Check { k, post, enr }
    }

    fn cost(&self) -> u8 {
        match &self.k {
            Kind::Rx { .. } => 3,
            Kind::Fz(_) | Kind::Near { .. } => 2,
            Kind::Where(_) => 1,
            _ => 0,
        }
    }

    fn eval(&self, h: &Hay) -> bool {
        let a = h.a;
        match &self.k {
            Kind::Terms { p, needles, any, not } => {
                let hit = |n: &String| text_has(h, *p, n.as_str());
                let r = if *any || *not { needles.iter().any(hit) } else { needles.iter().all(hit) };
                r != *not
            }
            Kind::Tagged { any: true, not: false, partial: true, .. } => true,
            Kind::Tagged { items, any, not, .. } => {
                let hit = |t: &Tm| match t.target {
                    Target::Text(p) => text_has(h, p, t.needle.as_str()),
                    Target::Kw => a.kw.iter().any(|k| term_in(k, &t.needle)),
                    Target::Journal => {
                        [&a.journal.title, &a.journal.iso].iter().any(|s| term_in(s, &t.needle)) || issn_hit(&a.journal, &t.needle)
                    }
                };
                let r = if *any || *not { items.iter().any(hit) } else { items.iter().all(hit) };
                r != *not
            }
            Kind::Rx { part, rx, none } => {
                let (x, y) = if *part == Part::Tiab { (h.raw(Part::Ti), h.raw(Part::Ab)) } else { (h.raw(*part), "") };
                if *none { !rx.any2(x, y) } else { rx.all2(x, y) }
            }
            Kind::Fz(t) => t.in_text(h.folded(Part::Tiab)) || h.keywords().iter().any(|k| t.in_text(k)),
            Kind::Phrase(p) => {
                [Part::Ti, Part::Ab].iter().any(|f| phrase_hit(h.folded(*f), p)) || h.keywords().iter().any(|k| phrase_hit(k, p))
            }
            Kind::Near { words, span } => {
                [Part::Ti, Part::Ab].iter().any(|f| near_hit(h.folded(*f), words, *span)) || h.keywords().iter().any(|k| near_hit(k, words, *span))
            }
            Kind::Mesh { names, major } => {
                names.iter().all(|n| a.mesh.iter().any(|m| (!*major || m.major) && fuzzy::fold(&m.name).contains(n.as_str())))
            }
            Kind::MeshAny(names) => names.iter().any(|n| mesh_has(a, n)),
            Kind::Tag(n) => a.mesh.iter().any(|m| term_in(&m.name, n)) || a.kw.iter().any(|k| term_in(k, n)),
            Kind::Kw(n) => a.kw.iter().any(|k| term_in(k, n)),
            Kind::Author { p, which, fuzzy: fz } => {
                let thr = if *fz { FUZZY_THR } else { 1.0 };
                let ok = |x: &Author| (if *fz { fuzzy::person_sim_loose } else { fuzzy::person_sim })(p, &fuzzy::parse_person(&x.name)) >= thr;
                match which {
                    Which::Any => a.authors.iter().any(ok),
                    Which::First => a.authors.iter().find(|x| !x.coll).is_some_and(ok),
                    Which::Last => a.authors.iter().rev().find(|x| !x.coll).is_some_and(ok),
                }
            }
            Kind::Aff { alts, fuzzy: fz } => a.affs.iter().any(|aff| alts.iter().any(|t| aff_has(aff, t, *fz))),
            Kind::Country(needles) => a.affs.iter().any(|aff| {
                let f = fuzzy::fold_padded(aff);
                needles.iter().any(|n| f.contains(n.as_str()))
            }),
            Kind::Journal { names, fuzzy: fz } => names.iter().any(|n| journal_has(&a.journal, n, *fz)),
            Kind::Issn(v) => v.iter().any(|i| {
                let i = i.replace('-', "");
                [&a.journal.issn, &a.journal.eissn].iter().any(|x| x.replace('-', "").eq_ignore_ascii_case(&i))
            }),
            Kind::Date { from, to, epub } => {
                let hit = |s: Option<(Triple, Triple)>| {
                    s.is_some_and(|(lo, hi)| from.is_none_or(|f| hi >= f.lo()) && to.is_none_or(|t| lo <= t.hi()))
                };
                if *epub {
                    hit(span_of(&a.epub, 0).or_else(|| art_span(a)))
                } else {
                    let first_day = |s: &str, y: u16| span_of(s, y).map(|(lo, _)| (lo, lo));
                    let print = first_day(&a.date, a.year);
                    let electronic = first_day(&a.epub, 0).filter(|e| print.is_none_or(|p| e.0 <= p.0));
                    hit(print) || hit(electronic) || hit(first_day(&a.book_date, 0)) || hit(first_day(&a.revised, 0))
                }
            }
            Kind::Types { sels, not } => sels.iter().any(|s| s.eval(a)) != *not,
            Kind::Status(s) => match s {
                Status::Aop => is_aop(a),
                Status::Preprint => is_preprint(a),
                Status::Unpublished => is_aop(a) || is_preprint(a),
                Status::Published => !is_aop(a) && !is_preprint(a),
                Status::Retracted => a.is_retracted(),
                Status::NoRetracted => !a.is_retracted(),
            },
            Kind::Species(human) => {
                let h = mesh_has(a, "humans");
                if *human { h } else { mesh_has(a, "animals") && !h }
            }
            Kind::Lang(v) => a.langs.iter().any(|l| v.iter().any(|x| x.eq_ignore_ascii_case(l))),
            Kind::Flag(f) => match f {
                Flag::Free | Flag::Fulltext => !a.pmc.is_empty(),
                Flag::Abstract => !a.abs.is_empty(),
                Flag::Data => !a.banks.is_empty(),
                Flag::HasComments => a.corrs.iter().any(Corr::is_comment),
                Flag::HasProblems => notice_or_problem(a),
            },
            Kind::Nct(None) => !ncts(a).is_empty(),
            Kind::Nct(Some(id)) => ncts(a).iter().any(|n| n.eq_ignore_ascii_case(id)),
            Kind::Grant(g) => a.grants.iter().any(|x| {
                fuzzy::fold(&x.id).contains(g.as_str()) || fuzzy::fold(&x.agency).contains(g.as_str()) || term_in(&x.country, g)
            }),
            Kind::Quartile(qs) => a.jr.as_ref().is_some_and(|j| qs.contains(&j.q)),
            Kind::SjrMin(v) => a.jr.as_ref().is_some_and(|j| j.sjr >= *v),
            Kind::HMin(v) => a.jr.as_ref().is_some_and(|j| j.h >= *v),
            Kind::Area(s) => crate::journals::rank_in_area(a, s).is_some(),
            Kind::CitesMin(v) => a.m.as_ref().is_some_and(|m| m.cites >= *v),
            Kind::RcrMin(v) => a.m.as_ref().and_then(|m| m.rcr).is_some_and(|r| r >= *v),
            Kind::Clinical => a.m.as_ref().is_some_and(|m| m.clin),
            Kind::Where(e) => e.eval(h),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Target {
    Text(Part),
    Kw,
    Journal,
}

#[derive(Debug, Clone)]
struct Tm {
    target: Target,
    needle: String,
}

const TAG_SERVER_ONLY: &str = "filter: [tag] clauses are server-side; use search";
const MESH_TAG_SERVER_ONLY: &str = "filter: [mh]/[majr] explode descendants server-side; use search";

fn canon_tag(tag: &str) -> String {
    let t = tag.trim().to_ascii_lowercase();
    match t.as_str() {
        "ti" | "title" => "ti",
        "ab" | "abstract" => "ab",
        "tiab" | "title/abstract" => "tiab",
        "ot" | "other term" | "other terms" => "ot",
        "ta" | "jour" | "journal" => "ta",
        "mh" | "mesh" | "mesh terms" | "mesh term" => "mh",
        "majr" | "mesh major topic" => "majr",
        "sh" | "mesh subheading" | "subheading" => "sh",
        "au" | "auth" | "author" => "au",
        "1au" | "first author" => "1au",
        "lastau" | "last author" | "last author name" => "lastau",
        "fau" | "full author name" => "fau",
        "ad" | "affiliation" => "ad",
        "pt" | "publication type" => "pt",
        "tw" | "text word" => "tw",
        "all" | "all fields" => "all",
        "sb" | "subset" | "filter" => "sb",
        "la" | "language" => "la",
        "dp" | "publication date" | "date publication" => "dp",
        "pmid" | "pubmed id" => "pmid",
        "si" | "secondary source id" => "si",
        "gr" | "grant number" => "gr",
        _ => return t,
    }
    .to_string()
}

fn mesh_tag(tag: &str) -> bool {
    matches!(canon_tag(tag).as_str(), "mh" | "majr")
}

fn tag_target(tag: &str) -> Option<Target> {
    Some(match canon_tag(tag).as_str() {
        "ti" => Target::Text(Part::Ti),
        "ab" => Target::Text(Part::Ab),
        "tiab" => Target::Text(Part::Tiab),
        "ot" => Target::Kw,
        "ta" => Target::Journal,
        _ => return None,
    })
}

fn has_operator(s: &str) -> bool {
    s.contains(['(', ')', '|', '&']) || s.split_whitespace().any(|w| matches!(w, "AND" | "OR" | "NOT"))
}

fn split_tag(t: &str) -> Option<(String, &str)> {
    let body = strip_outer_parens(t).strip_suffix(']')?;
    let (text, tag) = body.rsplit_once('[')?;
    if text.contains(['[', ']']) || tag.contains(['[', ']']) || has_operator(text) {
        return None;
    }
    let text = text.replace('"', "");
    let text = text.trim();
    (!text.is_empty()).then(|| (text.to_string(), tag))
}

fn scoped(items: &[String], default: Part) -> (Option<Vec<Tm>>, Option<&'static str>) {
    if !items.iter().any(|t| t.contains('[')) {
        return (None, None);
    }
    let mut out = Vec::with_capacity(items.len());
    let mut server_only = None;
    for t in items {
        let tagged = if t.contains('[') { split_tag(t) } else { Some((t.trim().replace('"', ""), "")) };
        let parsed = tagged.and_then(|(text, tag)| {
            if tag.is_empty() {
                Some((text, Target::Text(default)))
            } else {
                tag_target(tag).map(|tg| (text, tg))
            }
        });
        let Some((text, target)) = parsed else {
            let msg = if split_tag(t).is_some_and(|(_, tag)| mesh_tag(tag)) { MESH_TAG_SERVER_ONLY } else { TAG_SERVER_ONLY };
            server_only = server_only.or(Some(msg));
            continue;
        };
        let needle = needles(fold_all(vec![text])).pop().unwrap_or_default();
        out.push(Tm { target, needle });
    }
    (Some(out), server_only)
}

fn needles(v: Vec<String>) -> Vec<String> {
    v.into_iter()
        .map(|t| match t {
            t if t.contains('*') && t.replace('*', "").trim().is_empty() => String::new(),
            t if t.contains('*') => format!("{GLOB}{}", t.trim()),
            t if t.is_empty() => t,
            t => format!(" {t} "),
        })
        .collect()
}

fn issn_hit(j: &JournalInfo, needle: &str) -> bool {
    let n: String = needle.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    n.len() == 8
        && n.chars().take(7).all(|c| c.is_ascii_digit())
        && [&j.issn, &j.eissn].iter().any(|i| i.replace('-', "").eq_ignore_ascii_case(&n))
}

fn term_in(text: &str, needle: &str) -> bool {
    phrase_hit(&fuzzy::fold(text), &phrase_words(needle.strip_prefix(GLOB).unwrap_or(needle)))
}

fn glob_needle(t: &str) -> String {
    if t.contains('*') { format!("{GLOB}{}", t.trim()) } else { fuzzy::fold(t) }
}

fn mesh_has(a: &Article, name: &str) -> bool {
    let n = fuzzy::fold(name);
    a.mesh.iter().any(|m| {
        let f = fuzzy::fold(&m.name);
        f == n || f.strip_prefix(n.as_str()).is_some_and(|r| r.starts_with(' '))
    })
}

fn aff_has(aff: &str, toks: &[String], fz: bool) -> bool {
    let at = fuzzy::tokens(aff);
    toks.iter().all(|t| at.iter().any(|u| if fz { fuzzy::token_sim(t, u) >= FUZZY_THR } else { u == t || u.starts_with(t.as_str()) }))
}

fn strip_paren(s: &str) -> &str {
    s.split('(').next().unwrap_or(s).trim()
}

fn journal_has(j: &JournalInfo, q: &str, fz: bool) -> bool {
    let fq = fuzzy::fold(strip_paren(q));
    let fi = |s: &str| fuzzy::fold(strip_paren(s));
    let issn = q.replace('-', "");
    if [&j.issn, &j.eissn].iter().any(|i| !i.is_empty() && i.replace('-', "").eq_ignore_ascii_case(&issn)) {
        return true;
    }
    if q.contains('*') {
        let pat = fold_all(vec![strip_paren(q).to_string()]).pop().unwrap_or_default();
        return [&j.title, &j.iso].iter().any(|s| glob_hit(&fi(s), &pat));
    }
    [&j.title, &j.iso].iter().filter(|s| !s.is_empty()).any(|s| if fz { fuzzy::journal_sim(&fq, &fi(s)) >= FUZZY_THR } else { fi(s) == fq })
}

fn fold_all(v: Vec<String>) -> Vec<String> {
    v.iter().map(|t| fold_words(t)).collect()
}

fn fold_words(t: &str) -> String {
    if !t.contains('*') {
        return fuzzy::fold(t).trim().to_string();
    }
    t.split_whitespace()
        .map(|w| w.split('*').map(|p| fuzzy::fold(p).trim().to_string()).collect::<Vec<_>>().join("*"))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn split_list(v: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut it = v.chars().peekable();
    while let Some(c) = it.next() {
        if c == '"' {
            quoted = !quoted;
        }
        if !quoted && (c == ';' || (c == ',' && it.peek().is_some_and(|n| !n.is_whitespace()))) {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

fn split_plain(v: &str) -> Vec<String> {
    let mut pieces: Vec<String> = vec![String::new()];
    let mut depth = 0usize;
    let mut quoted = false;
    for c in v.chars() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.saturating_sub(1),
            ',' | ';' if depth == 0 && !quoted => {
                pieces.push(String::new());
                continue;
            }
            _ => {}
        }
        pieces.last_mut().expect("seeded").push(c);
    }
    let mut out: Vec<String> = Vec::new();
    for piece in pieces {
        let t = piece.trim();
        if t.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(prev) if t.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("phase ")) => {
                prev.push_str(", ");
                prev.push_str(t);
            }
            _ => out.push(t.to_string()),
        }
    }
    out
}

fn groups_plain(a: &Args, k: &str) -> Vec<Vec<String>> {
    a.all(k).into_iter().map(split_plain).filter(|g| !g.is_empty()).collect()
}

fn flat_plain(a: &Args, k: &str) -> Vec<String> {
    groups_plain(a, k).into_iter().flatten().collect()
}

fn groups(a: &Args, k: &str) -> Vec<Vec<String>> {
    a.all(k).into_iter().map(split_list).filter(|g| !g.is_empty()).collect()
}

fn flat(a: &Args, k: &str) -> Vec<String> {
    groups(a, k).into_iter().flatten().collect()
}

fn split_unquoted(v: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut quoted = false;
    for c in v.chars() {
        if c == '"' {
            quoted = !quoted;
        }
        if !quoted && matches!(c, ',' | ';') {
            out.push(String::new());
        } else {
            out.last_mut().expect("seeded").push(c);
        }
    }
    out
}

fn flat_text(a: &Args, k: &str) -> Vec<String> {
    a.all(k)
        .into_iter()
        .flat_map(split_unquoted)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

fn clean(t: &str) -> String {
    t.trim().replace('"', "")
}

fn plain(t: &str) -> String {
    clean(t).chars().filter(|c| !matches!(c, '[' | ']' | '(' | ')')).collect()
}

const DROP: &str = "\u{1}drop";

fn short_stem(v: &str) -> bool {
    v.split_whitespace().any(|w| w.find('*').is_some_and(|i| w[..i].chars().filter(|c| c.is_alphanumeric()).count() < 4))
}

fn text_tag(tag: &str) -> bool {
    matches!(canon_tag(tag).as_str(), "tiab" | "ti" | "ot" | "ta")
}

fn wild(v: &str, tag: &str) -> String {
    if v.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '*')) { format!("{v}[{tag}]") } else { format!("\"{v}\"[{tag}]") }
}

fn quoted(v: &str, tag: &str) -> String {
    format!("\"{v}\"[{tag}]")
}

fn ab_tag(tag: &str) -> bool {
    canon_tag(tag) == "ab"
}

fn rewrite_ab(t: &str) -> String {
    let mut out = String::with_capacity(t.len() + 4);
    let mut rest = t;
    while let Some(i) = rest.find('[') {
        let Some(j) = rest[i..].find(']') else { break };
        out.push_str(&rest[..i]);
        let tag = &rest[i + 1..i + j];
        if ab_tag(tag) {
            out.push_str("[tiab]");
        } else {
            out.push_str(&rest[i..=i + j]);
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out
}

fn has_ab_tag(t: &str) -> bool {
    let mut rest = t;
    while let Some(i) = rest.find('[') {
        let Some(j) = rest[i..].find(']') else { return false };
        if ab_tag(&rest[i + 1..i + j]) {
            return true;
        }
        rest = &rest[i + j + 1..];
    }
    false
}

fn short_word(text: &str) -> &str {
    text.split_whitespace().find(|w| short_stem(w)).unwrap_or(text)
}

fn short_wild_in(text: &str) -> Option<String> {
    let mut plain = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '[' => {
                in_tag = true;
                plain.push(' ');
            }
            ']' => in_tag = false,
            '(' | ')' | '"' => plain.push(' '),
            c if !in_tag => plain.push(c),
            _ => {}
        }
    }
    plain
        .split_whitespace()
        .filter(|w| !matches!(*w, "AND" | "OR" | "NOT" | "and" | "or" | "not"))
        .find(|w| short_stem(w))
        .map(str::to_string)
}

fn is_compound(t: &str) -> bool {
    t.contains(['(', '[']) || t.split_whitespace().any(|w| matches!(w, "AND" | "OR" | "NOT"))
}

fn check_short_tagged(flag: &str, items: &[String]) -> Result<()> {
    for t in items {
        if split_tag(t).is_none()
            && is_compound(t)
            && let Some(w) = short_wild_in(t)
        {
            bail!("{flag}: short wildcard {w} needs >=4 chars before * (compound clause)");
        }
        if let Some((text, tag)) = split_tag(t)
            && short_stem(&text)
            && tag_target(tag).is_none()
        {
            bail!("{flag}: short wildcard {} needs >=4 chars before * under [{}]", short_word(&text), tag.trim());
        }
    }
    Ok(())
}

fn check_ab_items(flag: &str, items: &[String]) -> Result<()> {
    if items.iter().any(|t| has_ab_tag(t) && split_tag(t).is_none()) {
        bail!("{flag}: [ab] works only on single term[ab] items (comma-separate them; no boolean expressions)");
    }
    Ok(())
}

fn strip_outer_parens(t: &str) -> &str {
    let mut t = t.trim();
    while t.starts_with('(') && t.ends_with(')') {
        let (mut depth, mut quoted) = (0i32, false);
        let mut closes_at_end = true;
        for (i, c) in t.char_indices() {
            match c {
                '"' => quoted = !quoted,
                '(' if !quoted => depth += 1,
                ')' if !quoted => {
                    depth -= 1;
                    if depth == 0 && i + 1 != t.len() {
                        closes_at_end = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !closes_at_end || depth != 0 {
            break;
        }
        t = t[1..t.len() - 1].trim();
    }
    t
}

fn tagged(t: &str, tag: &str) -> String {
    if t.contains('[') {
        let short = |(text, tg): (String, &str)| (text_tag(tg) || ab_tag(tg)) && short_stem(&text);
        if split_tag(t).is_some_and(short) {
            return DROP.to_string();
        }
        return group(&rewrite_ab(t.trim()), true);
    }
    let t = clean(t);
    if t.contains('*') {
        let v = plain(&t);
        if text_tag(tag) && short_stem(&v) {
            return DROP.to_string();
        }
        return wild(&v, tag);
    }
    if t.chars().any(|c| !c.is_alphanumeric()) { format!("\"{t}\"[{tag}]") } else { format!("{t}[{tag}]") }
}

fn qtag(t: &str, tag: &str) -> String {
    let v = plain(t);
    if v.contains('*') {
        if text_tag(tag) && short_stem(&v) { DROP.to_string() } else { wild(&v, tag) }
    } else {
        quoted(&v, tag)
    }
}

fn bare(t: &str, tag: &str) -> String {
    let v = plain(t);
    if !v.is_empty() && v.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '*')) { format!("{v}[{tag}]") } else { quoted(&v, tag) }
}

fn top_level_operator(s: &str, and: bool) -> bool {
    let (mut depth, mut quoted, mut start) = (0i32, false, None);
    let check = |w: &str| w.eq_ignore_ascii_case("OR") || w.eq_ignore_ascii_case("NOT") || (and && w.eq_ignore_ascii_case("AND"));
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            '|' if !quoted && depth == 0 => return true,
            '&' if !quoted && depth == 0 && and => return true,
            _ => {}
        }
        let sep = c.is_whitespace() && !quoted;
        match (sep, start) {
            (false, None) => start = Some(i),
            (true, Some(b)) => {
                if depth == 0 && check(&s[b..i]) {
                    return true;
                }
                start = None;
            }
            _ => {}
        }
    }
    depth == 0 && start.is_some_and(|b| check(&s[b..]))
}

fn group(s: &str, and: bool) -> String {
    if top_level_operator(s, and) { format!("({s})") } else { s.to_string() }
}

fn scope_free(free: &str, tag: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut prev_term = false;
    let mut cur = String::new();
    let mut quoted = false;
    let mut words: Vec<String> = Vec::new();
    for c in free.chars().chain(std::iter::once(' ')) {
        if c == '"' {
            quoted = !quoted;
            cur.push(c);
        } else if c.is_whitespace() && !quoted {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    for w in words {
        if matches!(w.as_str(), "AND" | "OR" | "NOT") {
            out.push(w);
            prev_term = false;
            continue;
        }
        let open = w.len() - w.trim_start_matches('(').len();
        let w2 = w.trim_start_matches('(');
        let close = w2.len() - w2.trim_end_matches(')').len();
        let core = w2.trim_end_matches(')');
        if core.is_empty() {
            out.push(format!("{}{}", "(".repeat(open), ")".repeat(close)));
            prev_term = open == 0 && close > 0;
            continue;
        }
        if prev_term {
            out.push("AND".into());
        }
        out.push(format!("{}{}{}", "(".repeat(open), tagged(core, tag), ")".repeat(close)));
        prev_term = true;
    }
    let mut res = String::new();
    for (i, t) in out.iter().enumerate() {
        if i > 0 && !out[i - 1].ends_with('(') && !t.starts_with(')') {
            res.push(' ');
        }
        res.push_str(t);
    }
    res
}

fn simple_words(free: &str) -> Option<Vec<String>> {
    let free = free.trim();
    let plain = !free.is_empty()
        && !free.contains(['[', '(', ')'])
        && !free.split_whitespace().any(|w| matches!(w, "AND" | "OR" | "NOT"))
        && !free.split_whitespace().all(|w| w.bytes().all(|b| b.is_ascii_digit()));
    plain.then(|| {
        quote_split(free)
            .into_iter()
            .flat_map(|(w, quoted)| {
                if quoted {
                    let g = w.split('*').map(fuzzy::fold).collect::<Vec<_>>().join("*");
                    let g = g.trim().to_string();
                    return if g.replace('*', "").trim().is_empty() { vec![] } else { vec![g] };
                }
                if w.contains('*') {
                    let g = w.split('*').map(fuzzy::fold).collect::<Vec<_>>().join("*");
                    return if g.replace('*', "").trim().is_empty() { vec![] } else { vec![g] };
                }
                fuzzy::tokens(w)
            })
            .collect()
    })
}

fn quote_split(s: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find(|c: char| !c.is_whitespace()) {
        rest = &rest[i..];
        if let Some(r) = rest.strip_prefix('"')
            && let Some(end) = r.find('"')
        {
            out.push((&r[..end], true));
            rest = &r[end + 1..];
            continue;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        out.push((rest[..end].trim_matches('"'), false));
        rest = &rest[end..];
    }
    out
}

fn or(v: Vec<String>) -> String {
    if v.iter().any(|s| s == DROP) {
        return DROP.to_string();
    }
    if v.len() == 1 { v.into_iter().next().unwrap() } else { format!("({})", v.join(" OR ")) }
}

fn parse_author(raw: &str) -> Vec<Person> {
    let parts: Vec<&str> = if raw.contains(';') {
        raw.split(';').map(str::trim).filter(|s| !s.is_empty()).collect()
    } else {
        let c: Vec<&str> = raw.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        let shaped = |p: &str| {
            let t: Vec<String> = p.split_whitespace().map(|t| t.replace(['.', '-'], "")).collect();
            t.len() >= 2
                && t.last().is_some_and(|i| {
                    i.chars().all(char::is_alphabetic)
                        && match i.chars().count() {
                            1 => true,
                            2..=3 => i.chars().all(char::is_uppercase),
                            _ => false,
                        }
                })
        };
        let split = c.len() > 1 && c.iter().all(|p| shaped(p));
        if split { c } else { vec![raw] }
    };
    parts.into_iter().map(fuzzy::parse_person).filter(|p| !p.last.is_empty()).collect()
}

fn lang_names(s: &str) -> (String, Vec<String>) {
    const T: &[(&str, &str, &str)] = &[
        ("en", "english", "eng"),
        ("fr", "french", "fre"),
        ("de", "german", "ger"),
        ("es", "spanish", "spa"),
        ("zh", "chinese", "chi"),
        ("ja", "japanese", "jpn"),
        ("it", "italian", "ita"),
        ("pt", "portuguese", "por"),
        ("ru", "russian", "rus"),
        ("ko", "korean", "kor"),
        ("nl", "dutch", "dut"),
        ("pl", "polish", "pol"),
        ("sv", "swedish", "swe"),
        ("tr", "turkish", "tur"),
        ("ar", "arabic", "ara"),
    ];
    let k = s.trim().to_ascii_lowercase();
    match T.iter().find(|(a, b, c)| k == *a || k == *b || k == *c) {
        Some((a, b, c)) => (b.to_string(), vec![a.to_string(), b.to_string(), c.to_string()]),
        None => (k.clone(), vec![k]),
    }
}

fn country_names(s: &str) -> Vec<String> {
    let k = fuzzy::fold(s);
    let v: &[&str] = match k.as_str() {
        "us" | "usa" | "united states" | "america" => &["usa", "united states"],
        "uk" | "gb" | "britain" | "united kingdom" | "england" => &["uk", "united kingdom", "england"],
        "china" | "prc" | "cn" => &["china"],
        "korea" | "south korea" => &["korea"],
        "de" | "germany" => &["germany"],
        "fr" | "france" => &["france"],
        "jp" | "japan" => &["japan"],
        _ => return vec![k],
    };
    v.iter().map(|s| s.to_string()).collect()
}

fn age_mesh(s: &str) -> String {
    match fuzzy::fold(s).as_str() {
        "newborn" | "neonate" | "neonatal" => "Infant, Newborn",
        "infant" | "baby" => "Infant",
        "preschool" | "toddler" => "Child, Preschool",
        "child" | "children" | "pediatric" | "paediatric" => "Child",
        "adolescent" | "teen" | "teenager" => "Adolescent",
        "young" | "young adult" => "Young Adult",
        "adult" => "Adult",
        "middle" | "middle aged" => "Middle Aged",
        "aged" | "elderly" | "older" | "senior" => "Aged",
        "oldest" | "80" | "aged 80" => "Aged, 80 and over",
        _ => return s.trim().to_string(),
    }
    .to_string()
}

fn species_mesh(s: &str) -> String {
    match fuzzy::fold(s).as_str() {
        "mouse" | "mice" => "Mice",
        "rat" | "rats" => "Rats",
        "dog" | "dogs" => "Dogs",
        "pig" | "pigs" | "swine" => "Swine",
        "monkey" | "primate" | "primates" => "Primates",
        "rabbit" | "rabbits" => "Rabbits",
        "zebrafish" => "Zebrafish",
        _ => return s.trim().to_string(),
    }
    .to_string()
}

#[derive(Debug, Clone, Default)]
pub struct Filters {
    pos: Vec<String>,
    neg: Vec<String>,
    checks: Vec<Check>,
    from: Option<Ymd>,
    to: Option<Ymd>,
    datetype: Option<String>,
    new_from: Option<Ymd>,
    sort: Option<SortKey>,
    asc: Option<bool>,
    need: Need,
    stolen: String,
    soft: Vec<String>,
    scope: Option<(&'static str, Part)>,
    server_only: Vec<&'static str>,
    rx: bool,
    desc: Vec<String>,
    pub scan: usize,
    pub local: Vec<&'static str>,
}

impl Filters {
    pub fn from_args(a: &Args) -> Result<Filters> {
        if has_ab_tag(&a.pos.join(" ")) {
            bail!("[ab] is not a PubMed tag; use --ab 'x' or [tiab]");
        }
        if let Some(w) = short_wild_in(&a.pos.join(" ")) {
            bail!("short wildcard {w} needs >=4 chars before * (PubMed ignores shorter stems)");
        }
        let mut f = Filters { scan: a.num::<usize>("scan")?.unwrap_or(600), ..Default::default() };
        f.terms(a)?;
        f.text_filters(a)?;
        f.mesh_family(a)?;
        f.people(a)?;
        f.journals(a);
        f.dates(a)?;
        f.types(a);
        f.status(a)?;
        f.population(a);
        f.access(a);
        f.ranks(a)?;
        for w in a.all("where") {
            let e = parse_where(w)?;
            f.need = f.need.or(e.need());
            f.rx |= e.has_rx();
            f.desc.push(format!("where {w}"));
            f.add(Kind::Where(e), None, false, Some("where"));
        }
        if let Some(s) = a.get("sort") {
            let k = SortKey::parse(s)?;
            f.sort = Some(k);
            if let Some(l) = k.local() {
                f.need = f.need.or(need_for_key(l));
            }
        }
        f.asc = if a.flag("asc") {
            Some(true)
        } else if a.flag("desc") {
            Some(false)
        } else {
            None
        };
        if let (Some(k), Some(asc)) = (f.sort, f.asc)
            && k.local().is_none()
            && asc != matches!(k, SortKey::Author | SortKey::Journal)
        {
            let (s, d) = (a.get("sort").unwrap_or_default(), if asc { "asc" } else { "desc" });
            bail!("--{d} not supported for server sort {s} (use search -n N :: sort {s} --{d})");
        }
        f.checks.sort_by_key(Check::cost);
        Ok(f)
    }

    fn add(&mut self, k: Kind, server: Option<String>, neg: bool, post: Option<&'static str>) {
        let mut post = post;
        match server {
            Some(s) if s == DROP => {
                self.rx = true;
                post = post.or(Some("wildcard"));
            }
            Some(s) => {
                if neg { self.neg.push(s) } else { self.pos.push(s) }
            }
            None => {}
        }
        if let Some(n) = post
            && !self.local.contains(&n)
        {
            self.local.push(n);
        }
        self.checks.push(Check::new(k, post.is_some()));
    }

    fn add_soft(&mut self, k: Kind, server: String, soft: bool, post: Option<&'static str>) {
        if soft {
            self.soft.push(server);
            self.add(k, None, false, post);
        } else {
            self.add(k, Some(server), false, post);
        }
    }

    fn text_filters(&mut self, a: &Args) -> Result<()> {
        for (key, part, none) in [("grep", Part::Tiab, false), ("grep-ti", Part::Ti, false), ("grep-ab", Part::Ab, false), ("no-grep", Part::Tiab, true)] {
            let pats = a.all(key);
            if let Some(rx) = Rx::new(&pats)? {
                self.rx = true;
                self.desc.push(format!("{key} {}", pats.join(" & ")));
                self.add(Kind::Rx { part, rx, none }, None, false, Some("grep"));
            }
        }
        for t in a.all("fz") {
            let ty = fuzzy::Typo::new(t);
            if ty.is_empty() {
                bail!("--fz: empty term");
            }
            self.rx = true;
            self.desc.push(format!("fz {t}"));
            self.add(Kind::Fz(ty), None, false, Some("fz"));
        }
        for p in a.all("phrase") {
            let words = phrase_words(p);
            if words.is_empty() {
                bail!("--phrase: empty");
            }
            let v = plain(p);
            let server = if short_stem(&v) { DROP.to_string() } else { format!("\"{v}\"[tiab]") };
            self.add(Kind::Phrase(words), Some(server), false, Some("phrase"));
        }
        for raw in a.all("near") {
            let (phrase, dist) =
                raw.rsplit_once(':').and_then(|(l, r)| r.trim().parse::<usize>().ok().map(|n| (l, n.min(MAX_NEAR_DIST)))).unwrap_or((raw, 3));
            let mut words: Vec<String> = Vec::new();
            for t in fuzzy::tokens(phrase) {
                if !words.contains(&t) {
                    words.push(t);
                }
            }
            if !(2..=MAX_NEAR).contains(&words.len()) {
                bail!("--near: need 2-{MAX_NEAR} distinct words, e.g. \"heart failure:3\"");
            }
            if phrase.contains('*') {
                bail!("--near: wildcards are not supported");
            }
            let server = format!("\"{}\"[tiab:~{dist}]", plain(phrase));
            self.add(Kind::Near { span: dist + words.len() - 1, words }, Some(server), false, None);
        }
        Ok(())
    }

    fn terms_kind(&mut self, items: Vec<String>, p: Part, any: bool, not: bool) -> Kind {
        match scoped(&items, p) {
            (Some(items), server_only) => {
                let partial = server_only.is_some();
                if let Some(m) = server_only
                    && !self.server_only.contains(&m)
                {
                    self.server_only.push(m);
                }
                Kind::Tagged { items, any, not, partial }
            }
            (None, _) => Kind::Terms { p, needles: needles(fold_all(items)), any, not },
        }
    }

    fn terms(&mut self, a: &Args) -> Result<()> {
        let all = flat_text(a, "all");
        check_ab_items("--all", &all)?;
        check_short_tagged("--all", &all)?;
        if !all.is_empty() {
            let parts: Vec<String> = all.iter().map(|t| tagged(t, "tiab")).collect();
            let dropped = parts.iter().any(|s| s == DROP);
            let kept: Vec<String> = parts.into_iter().filter(|s| s != DROP).collect();
            let s = if kept.is_empty() { DROP.to_string() } else { kept.join(" AND ") };
            let post = if all.iter().any(|t| has_ab_tag(t)) { Some("ab") } else if dropped { Some("wildcard") } else { None };
            let k = self.terms_kind(all, Part::Tiab, false, false);
            self.add(k, Some(s), false, post);
        }
        let any = flat_text(a, "any");
        check_ab_items("--any", &any)?;
        check_short_tagged("--any", &any)?;
        if !any.is_empty() {
            if any.iter().any(|t| has_ab_tag(t)) && scoped(&any, Part::Tiab).1.is_some() {
                bail!("--any: [ab] cannot be mixed with other tags in one OR group; use separate queries");
            }
            let parts: Vec<String> = any.iter().map(|t| tagged(t, "tiab")).collect();
            if parts.iter().any(|s| s == DROP) && scoped(&any, Part::Tiab).1.is_some() {
                bail!("--any: short wildcard cannot be mixed with server-only tags in one OR group; use separate queries");
            }
            let s = or(parts);
            let post = any.iter().any(|t| has_ab_tag(t)).then_some("ab");
            let k = self.terms_kind(any, Part::Tiab, true, false);
            self.add(k, Some(s), false, post);
        }
        let not = flat_text(a, "not");
        check_ab_items("--not", &not)?;
        check_short_tagged("--not", &not)?;
        if !not.is_empty() {
            let server: Vec<String> = not.iter().filter(|t| !has_ab_tag(t)).map(|t| tagged(t, "tiab")).filter(|s| s != DROP).collect();
            let post = (server.len() < not.len()).then_some("ab");
            let s = (!server.is_empty()).then(|| or(server));
            let k = self.terms_kind(not, Part::Tiab, true, true);
            self.add(k, s, true, post);
        }
        for (k, tag, p) in [("ti", "ti", Part::Ti), ("ab", "tiab", Part::Ab), ("tiab", "tiab", Part::Tiab)] {
            let post = (p == Part::Ab).then_some("ab");
            let list = flat_text(a, k);
            check_ab_items(&format!("--{k}"), &list)?;
            check_short_tagged(&format!("--{k}"), &list)?;
            for t in list {
                let kind = self.terms_kind(vec![t.clone()], p, false, false);
                let post = if has_ab_tag(&t) { Some("ab") } else { post };
                self.add(kind, Some(tagged(&t, tag)), false, post);
            }
            if self.scope.is_none() && a.flags.iter().any(|f| f == k) {
                self.scope = Some((tag, p));
                let free = a.pos.join(" ");
                if short_stem(&free) {
                    bail!("--{k}: wildcard stem under 4 characters is unsupported by PubMed; use 4+ characters or --grep");
                }
                if p == Part::Ab && !free.trim().is_empty() {
                    let Some(w) = simple_words(&free) else {
                        bail!("--ab needs plain words (no boolean/field syntax)");
                    };
                    self.add(Kind::Terms { p, needles: needles(fold_all(w)), any: false, not: false }, None, false, post);
                }
            }
        }
        Ok(())
    }

    fn mesh_family(&mut self, a: &Args) -> Result<()> {
        if ["mesh", "major", "tag"].iter().any(|k| !flat(a, k).is_empty()) {
            self.server_only.push("filter: --mesh/--major explode descendants server-side; use search");
        }
        for (k, tag, major) in [("mesh", "mh", false), ("major", "majr", true)] {
            for t in flat(a, k) {
                if short_stem(&t) {
                    bail!("--{k}: short wildcard {} needs >=4 chars before * under [{tag}]", short_word(&t));
                }
                self.add(Kind::Mesh { names: vec![fuzzy::fold(&t)], major }, Some(qtag(&t, tag)), false, None);
            }
        }
        for t in flat(a, "tag") {
            let s = if short_stem(&t) { DROP.to_string() } else { format!("({} OR {})", qtag(&t, "mh"), qtag(&t, "ot")) };
            self.add(Kind::Tag(glob_needle(&t)), Some(s), false, None);
        }
        for t in flat(a, "kw") {
            self.add(Kind::Kw(glob_needle(&t)), Some(qtag(&t, "ot")), false, None);
        }
        Ok(())
    }

    fn people(&mut self, a: &Args) -> Result<()> {
        let fz = a.flag("fuzzy");
        let post = if fz { Some("fuzzy") } else { None };
        for (key, which, tag) in
            [("author", Which::Any, "au"), ("first-author", Which::First, "1au"), ("last-author", Which::Last, "lastau")]
        {
            for raw in a.all(key) {
                for p in parse_author(raw) {
                    let ini = if p.ini.is_empty() { String::new() } else { format!(" {}", p.ini) };
                    let s = if fz { format!("{}{ini}*[{tag}]", p.last) } else { format!("{}{ini}[{tag}]", p.last) };
                    self.add_soft(Kind::Author { p, which, fuzzy: fz }, s, fz, post);
                }
            }
        }
        for g in groups(a, "aff") {
            let alts: Vec<Vec<String>> = g
                .iter()
                .map(|v| {
                    let mut t = fuzzy::tokens(v);
                    if t.len() > 1 {
                        t.retain(|x| !matches!(x.as_str(), "of" | "the" | "and" | "de" | "for" | "in"));
                    }
                    t
                })
                .collect();
            let server = or(alts
                .iter()
                .map(|t| {
                    let w: Vec<String> =
                        t.iter().map(|x| if fz && x.len() >= 4 { format!("{x}*[ad]") } else { format!("{x}[ad]") }).collect();
                    if w.len() > 1 { format!("({})", w.join(" AND ")) } else { w.join("") }
                })
                .collect());
            self.add_soft(Kind::Aff { alts, fuzzy: fz }, server, fz, post);
        }
        for g in groups_plain(a, "country") {
            let names: Vec<String> = g.iter().flat_map(|c| country_names(c)).collect();
            let server = or(names.iter().map(|n| tagged(n, "ad")).collect());
            self.add(Kind::Country(names.iter().map(|n| format!(" {n} ")).collect()), Some(server), false, None);
        }
        Ok(())
    }

    fn journals(&mut self, a: &Args) {
        let fz = a.flag("fuzzy");
        for g in groups_plain(a, "journal") {
            let server = or(g
                .iter()
                .map(|j| if fz { format!("{}*[ta]", fuzzy::fold(strip_paren(j))) } else { qtag(strip_paren(j), "ta") })
                .collect());
            self.add_soft(Kind::Journal { names: g, fuzzy: fz }, server, fz, fz.then_some("fuzzy"));
        }
        for g in groups_plain(a, "issn") {
            let server = or(g.iter().map(|i| bare(i, "issn")).collect());
            self.add(Kind::Issn(g), Some(server), false, None);
        }
    }

    fn tighten_from(&mut self, d: Ymd) {
        if self.from.is_none_or(|f| d.lo() > f.lo()) {
            self.from = Some(d);
        }
    }

    fn tighten_to(&mut self, d: Ymd) {
        if self.to.is_none_or(|t| d.hi() < t.hi()) {
            self.to = Some(d);
        }
    }

    fn dates(&mut self, a: &Args) -> Result<()> {
        if let Some(y) = a.get("year") {
            let (f, t) = year_range(y.trim())?;
            self.tighten_from(f);
            self.tighten_to(t);
        }
        if let Some(v) = a.get("from").or_else(|| a.get("since")) {
            self.tighten_from(Ymd::parse(v)?);
        }
        if let Some(v) = a.get("to").or_else(|| a.get("until")) {
            self.tighten_to(Ymd::parse(v)?);
        }
        let mut dt = None;
        let explicit = a.get("datetype").is_some();
        for (k, entry) in [("last", false), ("within", false), ("new", true)] {
            if let Some(v) = a.get(k) {
                let (from, _) = parse_last(v, today()).map_err(|e| match e {
                    "too large" => anyhow::anyhow!("--{k}: too large"),
                    _ => anyhow::anyhow!("--{k}: bad '{v}' (30d 6w 6m 2y)"),
                })?;
                if entry && !explicit {
                    self.new_from = Some(from);
                    self.server_only.push("filter: entry-date filters are server-side; use search");
                } else {
                    self.tighten_from(from);
                }
            }
        }
        if let Some(v) = a.get("datetype") {
            dt = Some(match v.to_ascii_lowercase().as_str() {
                "pdat" | "dp" | "pub" => "pdat",
                "edat" | "entrez" => "edat",
                "mdat" | "modified" => "mdat",
                "crdt" | "create" | "created" => "crdt",
                _ => bail!("--datetype: {v}? pdat|edat|mdat|crdt"),
            });
        }
        if let (Some(f), Some(t)) = (self.from, self.to)
            && f.lo() > t.hi()
        {
            bail!("date range reversed: {} > {}", f.esearch(), t.esearch());
        }
        if self.from.is_some() || self.to.is_some() {
            let dt = dt.unwrap_or("pdat");
            if dt != "pdat" && (a.get("new").is_some() || a.get("datetype").is_some()) {
                self.server_only.push("filter: entry-date filters are server-side; use search");
            }
            self.datetype = Some(dt.to_string());
            if dt != "mdat" {
                self.checks.push(Check::new(Kind::Date { from: self.from, to: self.to, epub: dt != "pdat" }, false));
            }
        }
        Ok(())
    }

    fn types(&mut self, a: &Args) {
        let mut inc: Vec<String> = ["type", "cat"].iter().flat_map(|k| flat_plain(a, k)).collect();
        for (flag, v) in [
            ("review", "review"),
            ("trial", "trial"),
            ("rct", "rct"),
            ("meta", "meta"),
            ("case", "case"),
            ("guideline", "guideline"),
            ("sys", "sr"),
        ] {
            if a.flag(flag) {
                inc.push(v.to_string());
            }
        }
        for (list, not) in [(inc, false), (flat_plain(a, "not-type"), true)] {
            if list.is_empty() {
                continue;
            }
            let sels: Vec<Sel> = list.iter().map(|s| type_sel(s)).collect();
            if let Some((_, f)) = sels.iter().find_map(server_sb) {
                self.server_only.push(f);
            }
            let s = or(sels.iter().map(Sel::server).collect());
            self.add(Kind::Types { sels, not }, Some(s), not, None);
        }
    }

    fn status(&mut self, a: &Args) -> Result<()> {
        let mut st = vec![];
        if let Some(v) = a.get("status") {
            st.push(match v.to_ascii_lowercase().as_str() {
                "published" => Some(Status::Published),
                "aop" | "ahead" => Some(Status::Aop),
                "unpublished" => Some(Status::Unpublished),
                "preprint" => Some(Status::Preprint),
                "retracted" => Some(Status::Retracted),
                "any" | "all" => None,
                _ => bail!("--status: {v}? published|aop|preprint|retracted|any"),
            });
        }
        for (flag, s) in [
            ("published", Status::Published),
            ("unpublished", Status::Unpublished),
            ("preprint", Status::Preprint),
            ("retracted", Status::Retracted),
            ("no-retracted", Status::NoRetracted),
        ] {
            if a.flag(flag) {
                st.push(Some(s));
            }
        }
        const AOP: &str = "\"ahead of print\"[sb]";
        for s in st.into_iter().flatten() {
            let (server, neg) = match s {
                Status::Aop => (AOP.to_string(), false),
                Status::Preprint => ("preprint[pt]".to_string(), false),
                Status::Unpublished => (format!("({AOP} OR preprint[pt])"), false),
                Status::Published => (format!("({AOP} OR preprint[pt])"), true),
                Status::Retracted => (qtag("Retracted Publication", "pt"), false),
                Status::NoRetracted => (qtag("Retracted Publication", "pt"), true),
            };
            self.add(Kind::Status(s), Some(server), neg, None);
        }
        Ok(())
    }

    fn population(&mut self, a: &Args) {
        for g in groups_plain(a, "lang") {
            let (names, codes): (Vec<String>, Vec<Vec<String>>) = g.iter().map(|l| lang_names(l)).unzip();
            let server = or(names.iter().map(|n| bare(n, "la")).collect());
            self.add(Kind::Lang(codes.into_iter().flatten().collect()), Some(server), false, None);
        }
        let mut sp = flat_plain(a, "species");
        if a.flag("human") {
            sp.push("human".into());
        }
        if a.flag("animal") {
            sp.push("animal".into());
        }
        for s in sp {
            match fuzzy::fold(&s).as_str() {
                "human" | "humans" => self.add(Kind::Species(true), Some("humans[mh]".into()), false, None),
                "animal" | "animals" => {
                    self.server_only.push("filter: --species uses MeSH explosion (server-side); use search");
                    self.add(Kind::Species(false), Some("(animals[mh] NOT humans[mh])".into()), false, None)
                }
                _ => {
                    self.server_only.push("filter: --species uses MeSH explosion (server-side); use search");
                    let m = species_mesh(&s);
                    self.add(Kind::MeshAny(vec![m.clone()]), Some(qtag(&m, "mh")), false, None);
                }
            }
        }
        for g in groups_plain(a, "sex") {
            let names: Vec<String> = g
                .iter()
                .map(|s| match fuzzy::fold(s).as_str() {
                    "f" | "female" | "women" | "woman" => "Female".to_string(),
                    "m" | "male" | "men" | "man" => "Male".to_string(),
                    _ => {
                        self.server_only.push("filter: --sex value uses MeSH explosion (server-side); use search");
                        s.clone()
                    }
                })
                .collect();
            let server = or(names.iter().map(|n| qtag(n, "mh")).collect());
            self.add(Kind::MeshAny(names), Some(server), false, None);
        }
        for g in groups_plain(a, "age") {
            self.server_only.push("filter: --age uses MeSH explosion (server-side); use search");
            let names: Vec<String> = g.iter().map(|s| age_mesh(s)).collect();
            let server = or(names.iter().map(|n| qtag(n, "mh")).collect());
            self.add(Kind::MeshAny(names), Some(server), false, None);
        }
    }

    fn access(&mut self, a: &Args) {
        if a.flag("free") {
            self.server_only.push("filter: --free is server-side; use search");
            self.add(Kind::Flag(Flag::Free), Some("free full text[sb]".into()), false, None);
        }
        if a.flag("fulltext") {
            self.server_only.push("filter: --fulltext is server-side; use search");
            self.add(Kind::Flag(Flag::Fulltext), Some("full text[sb]".into()), false, None);
        }
        if a.flag("oa") {
            self.server_only.push("filter: --oa is server-side; use search");
            self.add(Kind::Flag(Flag::Free), Some("\"pubmed pmc open access\"[sb]".into()), false, None);
        }
        if a.flag("abstract") {
            self.add(Kind::Flag(Flag::Abstract), Some("hasabstract".into()), false, None);
        }
        if a.flag("data") {
            self.server_only.push("filter: --data is server-side; use search");
            self.add(Kind::Flag(Flag::Data), Some("data[filter]".into()), false, None);
        }
        if a.has("nct") {
            let v = a.get("nct").unwrap_or("");
            if v.to_ascii_uppercase().starts_with("NCT") {
                self.add(Kind::Nct(Some(v.to_string())), Some(bare(v, "si")), false, None);
            } else {
                self.stolen = v.to_string();
                self.add(Kind::Nct(None), Some("clinicaltrials.gov[si]".into()), false, None);
            }
        }
        for g in flat_plain(a, "grant") {
            self.add(Kind::Grant(fuzzy::fold(&g)), Some(bare(&g, "gr")), false, None);
        }
        if a.flag("has-comments") {
            self.add(Kind::Flag(Flag::HasComments), Some("(hascommentin OR hascommenton)".into()), false, None);
        }
        if a.flag("has-problems") {
            self.add(Kind::Flag(Flag::HasProblems), Some(PROBLEMS_QUERY.into()), false, None);
        }
    }

    fn ranks(&mut self, a: &Args) -> Result<()> {
        let rank = Need { rank: true, ..Need::default() };
        let metrics = Need { metrics: true, ..Need::default() };
        let qs = flat_plain(a, "quartile");
        if !qs.is_empty() {
            let mut v = vec![];
            for q in qs {
                match q.trim_start_matches(['q', 'Q']).parse::<u8>() {
                    Ok(n) if (1..=4).contains(&n) => v.push(n),
                    _ => bail!("--quartile: bad '{q}' (1-4)"),
                }
            }
            self.add(Kind::Quartile(v), None, false, Some("quartile"));
            self.need = self.need.or(rank);
        }
        if let Some(v) = a.num::<f32>("sjr-min")? {
            self.add(Kind::SjrMin(v), None, false, Some("sjr"));
            self.need = self.need.or(rank);
        }
        if let Some(v) = a.num::<u32>("hindex-min")? {
            self.add(Kind::HMin(v), None, false, Some("hindex"));
            self.need = self.need.or(rank);
        }
        for ar in a.all("area") {
            self.add(Kind::Area(ar.to_string()), None, false, Some("area"));
            self.need = self.need.or(rank);
        }
        if let Some(v) = a.num::<u32>("cites-min")? {
            self.add(Kind::CitesMin(v), None, false, Some("cites"));
            self.need = self.need.or(metrics);
        }
        if let Some(v) = a.num::<f32>("rcr-min")? {
            self.add(Kind::RcrMin(v), None, false, Some("rcr"));
            self.need = self.need.or(metrics);
        }
        if a.flag("clinical") {
            self.add(Kind::Clinical, None, false, Some("clinical"));
            self.need = self.need.or(metrics);
        }
        Ok(())
    }

    pub fn query(&self, free: &str) -> String {
        let free = format!("{} {free}", self.stolen);
        let free = free.trim();
        let scoped = self.scope.filter(|_| !free.is_empty() && !free.contains('[')).map(|(tag, _)| scope_free(free, tag));
        let free = scoped.as_deref().unwrap_or(free);
        let mut pos: Vec<String> = Vec::with_capacity(self.pos.len() + 3);
        if !free.is_empty() {
            let wrap = !self.pos.is_empty() && free.contains(char::is_whitespace) && !(free.starts_with('(') && free.ends_with(')'));
            pos.push(if wrap { format!("({free})") } else { free.to_string() });
        }
        pos.extend(self.pos.iter().map(|p| group(p, false)));
        if let Some(n) = self.new_from {
            pos.extend(self.date_term());
            pos.push(format!("{}:3000[crdt]", n.esearch()));
        } else if pos.is_empty() {
            pos.extend(self.date_term());
        }
        if pos.is_empty() {
            pos.extend(self.soft.iter().map(|p| group(p, false)));
        }
        if pos.is_empty() {
            if self.neg.is_empty() {
                return String::new();
            }
            pos.push("all[sb]".into());
        }
        let mut q = pos.join(" AND ");
        for n in &self.neg {
            q.push_str(" NOT ");
            q.push_str(&group(n, true));
        }
        q
    }

    pub fn date_query(&self) -> Option<String> {
        let mut parts: Vec<String> = self.date_term().into_iter().collect();
        if let Some(n) = self.new_from {
            parts.push(format!("{}:3000[crdt]", n.esearch()));
        }
        (!parts.is_empty()).then(|| parts.join(" AND "))
    }

    fn date_term(&self) -> Option<String> {
        if self.from.is_none() && self.to.is_none() {
            return None;
        }
        let tag = match self.datetype.as_deref().unwrap_or("pdat") {
            "pdat" => "dp",
            other => other,
        };
        let lo = self.from.map_or("1800".to_string(), Ymd::esearch);
        let hi = self.to.map_or("3000".to_string(), Ymd::esearch);
        Some(format!("{lo}:{hi}[{tag}]"))
    }

    pub fn search_opts(&self) -> SearchOpts {
        let mut o = SearchOpts { sort: Some(self.sort.unwrap_or(SortKey::Relevance).server().to_string()), ..Default::default() };
        if (self.from.is_some() || self.to.is_some()) && self.new_from.is_none() {
            o.datetype = self.datetype.clone();
            o.mindate = Some(self.from.map_or("1800".to_string(), Ymd::esearch));
            o.maxdate = Some(self.to.map_or("3000".to_string(), Ymd::esearch));
        }
        o
    }

    pub fn year_opts(&self, y: i32) -> SearchOpts {
        let edge = |d: Option<Ymd>| d.filter(|d| d.y == y).map_or_else(|| format!("{y:04}"), Ymd::esearch);
        SearchOpts {
            datetype: Some(self.datetype.clone().unwrap_or_else(|| "pdat".into())),
            mindate: Some(edge(self.from)),
            maxdate: Some(edge(self.to)),
            ..Default::default()
        }
    }

    pub fn server_only(&self) -> Option<&'static str> {
        self.server_only.first().copied()
    }

    pub fn reject_server_only(&self) -> Result<()> {
        match self.server_only() {
            Some(m) => bail!("{m}"),
            None => Ok(()),
        }
    }

    pub fn year_span(&self) -> Option<(i32, i32)> {
        match (self.from, self.to) {
            (None, None) => None,
            (f, t) => {
                let to = t.map_or_else(|| today().y, |d| d.y);
                Some((f.map_or(to - 9, |d| d.y), to))
            }
        }
    }

    pub fn local_sort(&self) -> Option<&'static str> {
        self.sort.and_then(SortKey::local)
    }

    pub fn asc(&self) -> Option<bool> {
        self.asc
    }

    pub fn need(&self) -> Need {
        self.need
    }

    pub fn needs_post(&self) -> bool {
        self.checks.iter().any(|c| c.post)
    }

    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }

    pub fn describe_dates(&self) -> Option<String> {
        let primary = self
            .datetype
            .as_deref()
            .map(|dt| format!("{dt} {}..{}", self.from.map_or("*".into(), Ymd::esearch), self.to.map_or("*".into(), Ymd::esearch)));
        let entry = self.new_from.map(|n| format!("crdt {}..*", n.esearch()));
        match (primary, entry) {
            (None, None) => None,
            (p, e) => Some([p, e].into_iter().flatten().collect::<Vec<_>>().join("; ")),
        }
    }

    pub fn basic(&self, a: &Article, post_only: bool) -> bool {
        let h = Hay::new(a);
        self.checks.iter().all(|c| c.enr || (post_only && !c.post) || c.eval(&h))
    }

    pub fn enriched(&self, a: &Article, post_only: bool) -> bool {
        let h = Hay::new(a);
        self.checks.iter().all(|c| !c.enr || (post_only && !c.post) || c.eval(&h))
    }

    pub fn empty_query_err(&self) -> &'static str {
        if self.rx { "regex needs a base query: add terms or filters" } else { "empty query: give terms or filters" }
    }

    pub fn describe_local(&self) -> Option<String> {
        (!self.desc.is_empty()).then(|| self.desc.join("; "))
    }

    #[cfg(test)]
    pub fn matches(&self, a: &Article) -> bool {
        self.basic(a, false) && self.enriched(a, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(line: &str) -> Filters {
        let toks = crate::args::split_words(line).unwrap();
        Filters::from_args(&Args::parse(&toks).unwrap()).unwrap()
    }

    fn q(line: &str) -> String {
        let toks = crate::args::split_words(line).unwrap();
        let a = Args::parse(&toks).unwrap();
        Filters::from_args(&a).unwrap().query(&a.pos.join(" "))
    }

    fn err(line: &str) -> String {
        let toks = crate::args::split_words(line).unwrap();
        Filters::from_args(&Args::parse(&toks).unwrap()).unwrap_err().to_string()
    }

    #[test]
    fn spaced_parens_scope_without_empty_terms() {
        assert_eq!(q("'( heart OR lung )' --ti"), "(heart[ti] OR lung[ti])");
        assert_eq!(q("'( heart ) sepsis' --ti"), "(heart[ti]) AND sepsis[ti]");
        assert!(!q("'( heart OR lung )' --ti").contains("([ti]"));
    }

    #[test]
    fn quoted_commas_are_not_separators() {
        let t = q("--all '\"Clinical Trial, Phase III\"[pt]'");
        assert!(t.contains("\"Clinical Trial, Phase III\"[pt]"), "{t}");
        assert!(!t.contains("[tiab]"), "{t}");
        assert_eq!(split_list("\"a, b\"[ti],c"), ["\"a, b\"[ti]", "c"]);
        assert_eq!(split_list("\"a,b\"[ti];c"), ["\"a,b\"[ti]", "c"]);
        assert_eq!(split_plain("\"Clinical Trial, Phase III\", review"), ["\"Clinical Trial, Phase III\"", "review"]);
        assert_eq!(split_unquoted("\"x, y\"[ti], z;w"), ["\"x, y\"[ti]", " z", "w"]);
        let hit = Article { title: "Sepsis, septic shock outcomes".into(), ..Default::default() };
        let miss = Article { title: "Sepsis outcomes. Septic shock".into(), ..Default::default() };
        let fl = f("--ti '\"sepsis, septic shock\"[ti]'");
        assert!(fl.matches(&hit));
        assert!(!fl.matches(&miss));
    }

    #[test]
    fn plain_lists_trim_spaces() {
        assert_eq!(q("--type 'rct, review'"), q("--type rct,review"));
        assert_eq!(q("--lang 'en, fr'"), q("--lang en,fr"));
        assert_eq!(q("--not-type 'review, case'"), q("--not-type review,case"));
        assert!(f("--quartile '1, 2'").need().rank);
        assert!(q("--type 'Clinical Trial, Phase III'").contains("Clinical Trial, Phase III"));
        assert_eq!(split_plain("rct, review ;meta"), ["rct", "review", "meta"]);
        assert_eq!(split_plain("é, Éphase x"), ["é", "Éphase x"]);
    }

    #[test]
    fn where_in_lists_trim() {
        let a = Article { types: vec!["Review".into()], ..Default::default() };
        assert!(f("--where 'type in (rct,  review )'").matches(&a));
        assert!(f("--where 'type in rct, review'").matches(&a));
    }

    #[test]
    fn medline_is_server_side() {
        assert_eq!(f("--cat medline").server_only(), Some("filter: medline is server-side; use search"));
        assert_eq!(f("--not-type medline").server_only(), Some("filter: medline is server-side; use search"));
        assert_eq!(err("--where 'type in (medline)'"), "where: medline is server-side; use search @9");
        assert_eq!(q("--cat medline"), "\"medline\"[sb]");
    }

    #[test]
    fn mesh_family_is_server_only_in_pipes() {
        let msg = Some("filter: --mesh/--major explode descendants server-side; use search");
        assert_eq!(f("--mesh Neoplasms").server_only(), msg);
        assert_eq!(f("--major Neoplasms").server_only(), msg);
        assert_eq!(f("--tag Neoplasms").server_only(), msg);
        assert!(f("--kw crispr").server_only().is_none());
        assert!(f("--where 'mesh~neoplasms'").server_only().is_none());
    }

    #[test]
    fn first_and_last_author_skip_collectives() {
        let a = Article {
            authors: vec![
                Author { name: "Study Group".into(), coll: true, ..Default::default() },
                Author { name: "Guan X".into(), ..Default::default() },
                Author { name: "Lee K".into(), ..Default::default() },
                Author { name: "TESTS study collaborator group".into(), coll: true, ..Default::default() },
            ],
            ..Default::default()
        };
        assert!(f("--first-author 'Guan X'").matches(&a));
        assert!(f("--last-author 'Lee K'").matches(&a));
        assert!(!f("--last-author 'Guan X'").matches(&a));
        assert!(f("--where 'last~lee'").matches(&a));
    }

    #[test]
    fn near_rejects_wildcards() {
        assert_eq!(err("--near 'heart fail*:0'"), "--near: wildcards are not supported");
    }

    #[test]
    fn near_distance_is_capped() {
        let a = Article { title: "CRISPR Gene editing".into(), ..Default::default() };
        let v = f("--near 'CRISPR Gene:18446744073709551615'");
        assert!(v.matches(&a));
        assert!(v.query("").contains("[tiab:~1000]"));
    }

    #[test]
    fn free_text_passthrough() {
        assert_eq!(q("breast cancer"), "breast cancer");
        assert_eq!(q(r#"\"breast cancer\"[ti] AND tamoxifen[tiab]"#), "\"breast cancer\"[ti] AND tamoxifen[tiab]");
        assert_eq!(q(""), "");
    }

    #[test]
    fn term_families() {
        assert_eq!(
            q("--all asthma,child --any \"ICS,LABA\" --not review"),
            "asthma[tiab] AND child[tiab] AND (ICS[tiab] OR LABA[tiab]) NOT review[tiab]"
        );
        assert_eq!(q("--ti \"heart failure\" --tiab covid-19"), "\"heart failure\"[ti] AND \"covid-19\"[tiab]");
        assert_eq!(q("--all \"statin*\""), "statin*[tiab]");
        assert_eq!(q("--not x"), "all[sb] NOT x[tiab]");
        assert_eq!(q("cancer immunotherapy --ti pd-1"), "(cancer immunotherapy) AND \"pd-1\"[ti]");
    }

    #[test]
    fn mesh_family() {
        assert_eq!(q("--mesh \"Hemoglobin A, Glycosylated\" --major asthma"), "\"Hemoglobin A, Glycosylated\"[mh] AND \"asthma\"[majr]");
        assert_eq!(q("--tag copd --kw frailty"), "(\"copd\"[mh] OR \"copd\"[ot]) AND \"frailty\"[ot]");
        assert_eq!(q("--mesh a,b"), "\"a\"[mh] AND \"b\"[mh]");
    }

    #[test]
    fn authors() {
        assert_eq!(q("-a \"Smith JA\""), "smith ja[au]");
        assert_eq!(q("-a \"Müller K\""), "muller k[au]");
        assert_eq!(q("-a \"John Smith\""), "smith j[au]");
        assert_eq!(q("-a \"Smith, John\""), "smith j[au]");
        assert_eq!(q("-a \"Smith J, Doe A\""), "smith j[au] AND doe a[au]");
        assert_eq!(q("-a \"Smith J\" --fuzzy"), "smith j*[au]");
        assert_eq!(q("--first-author \"Lee K\" --last-author \"Park S\""), "lee k[1au] AND park s[lastau]");
        assert_eq!(q("-a Smith"), "smith[au]");
        assert!(!f("-a Smith").needs_post());
        assert!(f("-a Smith --fuzzy").needs_post());
    }

    #[test]
    fn affiliations() {
        assert_eq!(q("--aff \"Harvard Medical School\""), "(harvard[ad] AND medical[ad] AND school[ad])");
        assert_eq!(q("-i \"Univ of Toronto\" --fuzzy"), "(univ*[ad] AND toronto*[ad])");
        assert_eq!(q("--country germany"), "germany[ad]");
        assert_eq!(q("--country uk"), "(uk[ad] OR \"united kingdom\"[ad] OR england[ad])");
    }

    #[test]
    fn journals() {
        assert_eq!(q("-j \"J Clin Oncol,Lancet\""), "(\"J Clin Oncol\"[ta] OR \"Lancet\"[ta])");
        assert_eq!(q("-j \"Lancet (London, England)\""), "\"Lancet\"[ta]");
        assert_eq!(q("-j \"Clin Oncol\" --fuzzy"), "clin oncol*[ta]");
        assert_eq!(q("--issn 0028-0836"), "0028-0836[issn]");
    }

    #[test]
    fn types_and_status() {
        assert_eq!(q("-t rct,review"), "(\"Randomized Controlled Trial\"[pt] OR \"Review\"[pt])");
        assert_eq!(q("-t sr"), "\"systematic\"[sb]");
        assert_eq!(q("--meta --not-type editorial,letter"), "\"Meta-Analysis\"[pt] NOT (\"Editorial\"[pt] OR \"Letter\"[pt])");
        assert_eq!(q("--cat guideline"), "(\"Practice Guideline\"[pt] OR \"Guideline\"[pt])");
        assert_eq!(q("--sys --rct"), "(\"Randomized Controlled Trial\"[pt] OR \"systematic\"[sb])");
        assert_eq!(q("--status aop"), "\"ahead of print\"[sb]");
        assert_eq!(
            q("--published --no-retracted"),
            "all[sb] NOT (\"ahead of print\"[sb] OR preprint[pt]) NOT \"Retracted Publication\"[pt]"
        );
        assert_eq!(q("x --preprint"), "x AND preprint[pt]");
        assert_eq!(q("--retracted"), "\"Retracted Publication\"[pt]");
    }

    #[test]
    fn publication_type_descendants_match_locally() {
        let typed = |t: &[&str]| Article { types: t.iter().map(|x| x.to_string()).collect(), ..Default::default() };
        let rct = typed(&["Randomized Controlled Trial", "Journal Article"]);
        let phase3 = typed(&["Clinical Trial, Phase III"]);
        let sr = typed(&["Systematic Review"]);
        let protocol = typed(&["Clinical Trial Protocol"]);
        let netma = typed(&["Network Meta-Analysis"]);
        assert!(f("-t trial").matches(&rct));
        assert!(f("-t trial").matches(&phase3));
        assert!(!f("-t trial").matches(&protocol));
        assert!(!f("--not-type trial").matches(&rct));
        assert!(f("--not-type trial").matches(&protocol));
        assert!(f("--cat 'Controlled Clinical Trial'").matches(&rct));
        assert!(!f("-t rct").matches(&phase3));
        assert!(f("-t review").matches(&sr));
        assert!(f("-t meta").matches(&netma));
        assert!(f("--where 'type=trial'").matches(&rct));
        assert!(f("--where 'type in (review, trial)'").matches(&phase3));
        assert!(!f("--where 'type in (review, rct)'").matches(&phase3));
        assert!(f("--where 'type=rct'").matches(&rct));
    }

    #[test]
    fn year_span_uses_the_given_bound() {
        let now = today().y;
        assert_eq!(f("--to 2010").year_span(), Some((2001, 2010)));
        assert_eq!(f("--from 2020").year_span(), Some((2020, now)));
        assert_eq!(f("--year 2018-2022").year_span(), Some((2018, 2022)));
        assert_eq!(f("sepsis").year_span(), None);
    }

    #[test]
    fn population_and_access() {
        assert_eq!(q("--lang en,fr"), "(english[la] OR french[la])");
        assert_eq!(q("--species human --sex f --age elderly"), "humans[mh] AND \"Female\"[mh] AND \"Aged\"[mh]");
        assert_eq!(q("--animal"), "(animals[mh] NOT humans[mh])");
        assert_eq!(
            q("--free --fulltext --oa --abstract --data"),
            "free full text[sb] AND full text[sb] AND \"pubmed pmc open access\"[sb] AND hasabstract AND data[filter]"
        );
        assert_eq!(q("--nct NCT01234567 --grant R01"), "NCT01234567[si] AND R01[gr]");
        assert_eq!(q("--nct --has-comments"), "clinicaltrials.gov[si] AND (hascommentin OR hascommenton)");
        assert_eq!(q("--nct asthma"), "asthma AND clinicaltrials.gov[si]");
        assert_eq!(q("--has-problems"), PROBLEMS_QUERY);
    }

    #[test]
    fn local_only_filters() {
        let x = f("--quartile 1,2 --sjr-min 2 --cites-min 5");
        assert!(x.needs_post());
        assert_eq!(x.need(), Need { rank: true, metrics: true, expand: false });
        assert_eq!(x.query("a"), "a");
        assert_eq!(f("--where \"sjr>1\"").need(), Need { rank: true, metrics: false, expand: false });
        let s = f("--sort cites");
        assert!(!s.needs_post());
        assert_eq!(s.local_sort(), Some("cites"));
        assert!(s.need().metrics);
        assert!(!f("-t rct --year 2020").needs_post());
        assert_eq!(err("--quartile 7"), "--quartile: bad '7' (1-4)");
    }

    #[test]
    fn dates() {
        let t = Ymd { y: 2026, m: 10, d: 30 };
        assert_eq!(parse_last("30d", t).unwrap().0, Ymd { y: 2026, m: 9, d: 30 });
        assert_eq!(parse_last("6m", t).unwrap().0, Ymd { y: 2026, m: 4, d: 30 });
        assert_eq!(parse_last("2y", t).unwrap().0, Ymd { y: 2024, m: 10, d: 30 });
        assert_eq!(parse_last("1w", t).unwrap().0, Ymd { y: 2026, m: 10, d: 23 });
        assert_eq!(parse_last("8m", Ymd { y: 2026, m: 3, d: 31 }).unwrap().0, Ymd { y: 2025, m: 7, d: 31 });
        assert_eq!(parse_last("1m", Ymd { y: 2026, m: 3, d: 31 }).unwrap().0, Ymd { y: 2026, m: 2, d: 28 });
        assert_eq!(parse_last("1y", Ymd { y: 2024, m: 2, d: 29 }).unwrap().0, Ymd { y: 2023, m: 2, d: 28 });
        assert_eq!(parse_last("45d", Ymd { y: 2026, m: 1, d: 10 }).unwrap().0, Ymd { y: 2025, m: 11, d: 26 });
        assert!(parse_last("abc", t).is_err());
        assert!(parse_last("5", t).is_err());
        assert!(parse_last("-5d", t).is_err());
        assert_eq!(civil_from_days(0), Ymd { y: 1970, m: 1, d: 1 });
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(Ymd::parse("2020-3-5").unwrap().esearch(), "2020/03/05");
        assert_eq!(Ymd::parse("2020/03").unwrap().hi(), (2020, 3, 31));
        assert!(Ymd::parse("2020-13").is_err());
        assert!(Ymd::parse("20").is_err());
        let o = f("--year 2018-2022").search_opts();
        assert_eq!((o.datetype.as_deref(), o.mindate.as_deref(), o.maxdate.as_deref()), (Some("pdat"), Some("2018"), Some("2022")));
        let o = f("--from 2020-03 --datetype edat").search_opts();
        assert_eq!((o.datetype.as_deref(), o.mindate.as_deref(), o.maxdate.as_deref()), (Some("edat"), Some("2020/03"), Some("3000")));
        let o = f("--to 2010").search_opts();
        assert_eq!(o.mindate.as_deref(), Some("1800"));
        assert_eq!(f("--last 7d").search_opts().datetype.as_deref(), Some("pdat"));
        assert_eq!(f("--last 6m").search_opts().datetype.as_deref(), Some("pdat"));
        assert_eq!(f("--year 2020").year_span(), Some((2020, 2020)));
        assert!(err("--from 2022 --to 2020").starts_with("date range reversed"));
        assert_eq!(f("x").search_opts().sort.as_deref(), Some("relevance"));
        assert_eq!(f("--sort pubdate").search_opts().sort.as_deref(), Some("pub_date"));
    }

    fn art() -> Article {
        Article {
            pmid: 1,
            title: "Statins in Heart Failure: a Randomized Trial".into(),
            abs: vec![("".into(), "We studied atorvastatin in 300 patients.".into())],
            authors: vec![
                Author { name: "Müller KL".into(), ..Default::default() },
                Author { name: "Smith JA".into(), ..Default::default() },
                Author { name: "Doe A".into(), ..Default::default() },
            ],
            affs: vec!["Department of Medicine, Harvard Medical School, Boston, MA, USA".into(), "Univ of Toronto, Canada".into()],
            journal: JournalInfo {
                title: "Lancet (London, England)".into(),
                iso: "Lancet".into(),
                issn: "0140-6736".into(),
                country: "England".into(),
                ..Default::default()
            },
            year: 2021,
            date: "2021-03-15".into(),
            types: vec!["Journal Article".into(), "Randomized Controlled Trial".into()],
            mesh: vec![
                Mesh { name: "Heart Failure".into(), major: true, quals: vec![] },
                Mesh { name: "Humans".into(), ..Default::default() },
                Mesh { name: "Aged, 80 and over".into(), ..Default::default() },
            ],
            kw: vec!["statin".into()],
            langs: vec!["eng".into()],
            pmc: "PMC1".into(),
            banks: vec![("ClinicalTrials.gov".into(), vec!["NCT01234567".into()])],
            nrefs: 40,
            jr: Some(JournalRank { q: 1, sjr: 12.5, h: 600, area: "Medicine (miscellaneous)".into() }),
            m: Some(Metrics { cites: 25, rcr: Some(2.1), clin: true, ..Default::default() }),
            ..Default::default()
        }
    }

    fn ok(line: &str) -> bool {
        f(line).matches(&art())
    }

    #[test]
    fn raw_list_items_are_grouped() {
        assert_eq!(q("--all \"asthma[ti],cancer[ti] OR diabetes[ti]\""), "asthma[ti] AND (cancer[ti] OR diabetes[ti])");
        assert_eq!(q("--any \"a[ti] AND b[ti],c[ti]\""), "((a[ti] AND b[ti]) OR c[ti])");
    }

    fn crispr_article() -> Article {
        Article {
            title: "CRISPR screens in sepsis".into(),
            abs: vec![(String::new(), "We study asthma and lung injury.".into())],
            mesh: vec![Mesh { name: "Sepsis".into(), major: true, quals: vec![] }, Mesh { name: "Asthma".into(), major: false, quals: vec![] }],
            authors: vec![Author { name: "de Azevedo WF".into(), ..Default::default() }],
            ..Default::default()
        }
    }

    #[test]
    fn where_unparenthesised_in_list_ends_at_operator() {
        let a = Article { langs: vec!["eng".into()], year: 2024, ..Default::default() };
        assert!(!f("--where 'lang in eng,fre & year<1900'").matches(&a));
        assert!(f("--where 'lang in eng,fre & year>1900'").matches(&a));
        assert!(f("--where 'lang in eng,fre || year<1900'").matches(&a));
        assert!(f("--where '(lang in eng,fre) & year>1900'").matches(&a));
        assert!(f("--where 'lang in (eng,fre) && year>1900'").matches(&a));
    }

    #[test]
    fn parenthesised_and_composed_abstract_tags() {
        let title_only = Article { title: "Placebo trial".into(), abs: vec![("".into(), "Nothing relevant.".into())], ..Default::default() };
        let x = f("--not '(placebo[ab])'");
        assert_eq!(x.query("asthma"), "asthma");
        assert!(x.local.contains(&"ab"));
        assert!(x.matches(&title_only));
        let y = f("--all '(placebo[ab])'");
        assert_eq!(y.query(""), "(placebo[tiab])");
        assert!(!y.matches(&title_only));
        let msg = "[ab] works only on single term[ab] items (comma-separate them; no boolean expressions)";
        assert_eq!(err("--not 'a[ab] OR b[ab]'"), format!("--not: {msg}"));
        assert_eq!(err("--all '(a[ab] AND b[pt])'"), format!("--all: {msg}"));
        assert_eq!(err("--ti '(a[ab] OR b[ab])'"), format!("--ti: {msg}"));
    }

    #[test]
    fn abstract_tag_is_sent_as_tiab_and_enforced_locally() {
        let x = f("--all 'placebo[ab]'");
        assert_eq!(x.query(""), "placebo[tiab]");
        assert!(x.needs_post());
        assert!(x.local.contains(&"ab"));
        assert_eq!(q("--any 'placebo[abstract],sepsis'"), "(placebo[tiab] OR sepsis[tiab])");
        assert!(f("--any 'placebo[abstract],sepsis'").local.contains(&"ab"));
        let title_only = Article { title: "Placebo trial".into(), abs: vec![("".into(), "Nothing relevant.".into())], ..Default::default() };
        assert!(!f("--all 'placebo[ab]'").matches(&title_only));
        assert!(f("--all 'placebo[tiab]'").matches(&title_only));
        let n = f("--not 'placebo[ab],sepsis'");
        assert_eq!(n.query("asthma"), "asthma NOT sepsis[tiab]");
        assert!(n.local.contains(&"ab"));
        assert!(n.matches(&title_only));
        assert_eq!(f("--not 'placebo[ab]'").query("asthma"), "asthma");
        assert!(!f("--all 'asthma[tiab]'").needs_post());
    }

    #[test]
    fn positional_ab_tag_is_rejected() {
        let err = |l: &str| Filters::from_args(&Args::parse(&crate::args::split_words(l).unwrap()).unwrap()).unwrap_err().to_string();
        assert_eq!(err("placebo[ab]"), "[ab] is not a PubMed tag; use --ab 'x' or [tiab]");
        assert_eq!(err("sepsis AND (placebo[Abstract] OR x[ti])"), "[ab] is not a PubMed tag; use --ab 'x' or [tiab]");
        assert!(Filters::from_args(&Args::parse(&crate::args::split_words("placebo[tiab] x[ti]").unwrap()).unwrap()).is_ok());
    }

    #[test]
    fn concl_where_uses_interpretation_not_funding() {
        let a = Article {
            abs: vec![("Interpretation".into(), "Drug works.".into()), ("Funding".into(), "US National Institutes of Health.".into())],
            ..Default::default()
        };
        let f = |w: &str| Filters::from_args(&Args::parse(&["--where".to_string(), w.to_string()]).unwrap()).unwrap().matches(&a);
        assert!(f("concl~works"));
        assert!(!f("concl~health"));
    }

    #[test]
    fn ab_tag_enforced_under_any_enclosing_flag() {
        let title_only = Article { title: "Placebo trial".into(), abs: vec![("".into(), "Nothing relevant.".into())], ..Default::default() };
        for flag in ["--tiab", "--ti", "--all", "--any"] {
            let x = f(&format!("{flag} 'placebo[ab]'"));
            assert!(x.local.contains(&"ab"), "{flag}");
            assert!(x.needs_post(), "{flag}");
            assert!(!x.matches(&title_only), "{flag}");
        }
        assert!(!f("--tiab 'asthma'").local.contains(&"ab"));
    }

    #[test]
    fn other_abstract_counts_for_text_predicates() {
        let a = Article {
            title: "Plain".into(),
            abs: vec![("".into(), "Main text.".into())],
            other_abs: "complex words live here".into(),
            ..Default::default()
        };
        assert!(f("--phrase 'complex words'").matches(&a));
        assert!(f("--ab 'complex'").matches(&a));
        assert!(f("--grep-ab 'live here'").matches(&a));
        assert!(!f("--phrase 'absent phrase'").matches(&a));
    }

    #[test]
    fn date_query_intersects_flags() {
        let dq = |s: &str| f(s).date_query();
        assert_eq!(dq("--year 2024 --from 2020").as_deref(), Some("2024:2024[dp]"));
        assert_eq!(dq("--from 2020 --to 2022").as_deref(), Some("2020:2022[dp]"));
        assert!(dq("--aff Harvard").is_none());
    }

    #[test]
    fn or_groups_with_server_only_items_are_not_filtered_locally() {
        let rev = Article {
            title: "Sepsis overview".into(),
            abs: vec![("".into(), "Nothing relevant.".into())],
            types: vec!["Review".into()],
            ..Default::default()
        };
        assert_eq!(
            err("--any 'placebo[ab],review[pt]'"),
            "--any: [ab] cannot be mixed with other tags in one OR group; use separate queries"
        );
        assert!(Filters::from_args(&Args::parse(&crate::args::split_words("--any 'placebo[ab],sepsis'").unwrap()).unwrap()).is_ok());
        let x = f("--any 'sepsis[ti],review[pt]'");
        assert_eq!(x.query(""), "(sepsis[ti] OR review[pt])");
        assert!(x.matches(&rev));
        assert!(x.server_only().is_some());
        assert!(f("--any 'placebo[ab],sepsis[ti]'").matches(&rev));
        assert!(!f("--any 'placebo[ab],zzz[ti]'").matches(&rev));
        assert!(!f("--all 'placebo[ab],review[pt]'").matches(&rev));
        assert!(!f("--not 'sepsis[ti],review[pt]'").matches(&rev));
    }

    #[test]
    fn wildcard_terms_keep_word_boundaries() {
        assert_eq!(fold_all(vec!["random* controlled".into()]), ["random* controlled"]);
        assert_eq!(fold_all(vec!["stat*in".into()]), ["stat*in"]);
        assert_eq!(fold_all(vec!["*foo bar*".into()]), ["*foo bar*"]);
        let t = Article { title: "A randomized controlled trial of statins".into(), ..Default::default() };
        for line in ["--ti 'random* controlled'", "--all 'random* controlled'", "--any 'random* controlled'", "--tiab 'random* controlled'"] {
            assert!(f(line).matches(&t), "{line}");
        }
        assert!(!f("--ti 'random* trial'").matches(&t));
        assert!(!f("--not 'random* controlled'").matches(&t));
    }

    #[test]
    fn publication_type_wildcards_match_locally() {
        let typed = |t: &[&str]| Article { types: t.iter().map(|x| x.to_string()).collect(), ..Default::default() };
        let p3 = typed(&["Clinical Trial, Phase III", "Journal Article"]);
        let rev = typed(&["Review"]);
        assert!(f("-t 'Clinical Trial*'").matches(&p3));
        assert!(!f("-t 'Clinical Trial*'").matches(&rev));
        assert!(!f("--not-type 'Clinical Trial*'").matches(&p3));
        assert!(f("--not-type 'Clinical Trial*'").matches(&rev));
        assert!(f("--where 'type=\"clinical trial*\"'").matches(&p3));
        assert!(f("-t '*trial*'").matches(&p3));
    }

    #[test]
    fn tagged_clauses_match_locally() {
        let a = crispr_article();
        assert!(!f("--not 'CRISPR[ti]'").matches(&a));
        assert!(f("--not 'CRISPR[ab]'").matches(&a));
        assert!(f("--all 'CRISPR[ti]'").matches(&a));
        assert!(!f("--all 'CRISPR[ab]'").matches(&a));
        assert!(f("--all 'asthma[ab],sepsis[ti]'").matches(&a));
        assert!(f("--all 'asthma[tiab],sepsis'").matches(&a));
        assert!(f("--any 'zzz[ti],asthma[ab]'").matches(&a));
        let mesh = Some("filter: [mh]/[majr] explode descendants server-side; use search");
        for line in ["--all 'sepsis[majr]'", "--all 'asthma[mh]'", "--not 'x[MeSH Terms]'", "--any 'a[ti],asthma[mh]'"] {
            assert_eq!(f(line).server_only(), mesh, "{line}");
        }
        assert!(f("--ti 'CRISPR[ti]'").matches(&a));
        assert!(f("--all '\"lung injury\"[tiab]'").matches(&a));
        assert!(f("--all 'CRISPR[ti]'").server_only().is_none());
    }

    #[test]
    fn quoted_tagged_phrases_keep_their_quotes() {
        assert_eq!(q("sepsis --all '\"Smith J\"[au]'"), "sepsis AND \"Smith J\"[au]");
        assert_eq!(tagged("\"Smith J\"[au]", "tiab"), "\"Smith J\"[au]");
        assert_eq!(tagged("Smith J[au]", "tiab"), "Smith J[au]");
        assert_eq!(tagged("\"heart failure\"", "tiab"), "\"heart failure\"[tiab]");
    }

    #[test]
    fn text_matching_never_crosses_title_abstract() {
        let a = Article {
            title: "Sepsis management implications".into(),
            abs: vec![(String::new(), "Myocardial injury followed. See abstract text here.".into())],
            ..Default::default()
        };
        for line in [
            "--all 'implications myocardial'",
            "--any 'implications myocardial'",
            "--tiab 'implications myocardial'",
            "--phrase 'implications myocardial'",
            "--near 'implications myocardial:0'",
            "--grep 'implications\\s+myocardial'",
            "--where 'text~\"implications myocardial\"'",
            "--where 'text~~\"implications\\s+myocardial\"'",
        ] {
            assert!(!f(line).matches(&a), "{line}");
        }
        assert!(f("--not 'implications myocardial'").matches(&a));
        assert!(f("--no-grep 'implications\\s+myocardial'").matches(&a));
        assert!(f("--all 'management implications'").matches(&a));
        assert!(f("--all 'myocardial injury'").matches(&a));
        assert!(f("--all 'sepsis,injury'").matches(&a));
        assert!(f("--grep sepsis --grep injury").matches(&a));
        assert!(f("--where 'text~sepsis & text~injury'").matches(&a));
    }

    #[test]
    fn phrase_wildcard_in_any_position() {
        let a = Article { title: "A randomized trial of heart failure".into(), ..Default::default() };
        assert!(f("--phrase 'random* trial'").matches(&a));
        assert!(f("--phrase 'random* trial of heart fail*'").matches(&a));
        assert!(!f("--phrase 'random* heart'").matches(&a));
        assert!(!f("--phrase 'random trial'").matches(&a));
    }

    #[test]
    fn mesh_explosion_type_aliases_are_server_side() {
        assert_eq!(f("-t cohort").server_only(), Some("filter: cohort uses MeSH explosion (server-side); use search"));
        assert_eq!(f("--not-type 'case control'").server_only(), Some("filter: case control uses MeSH explosion (server-side); use search"));
        assert_eq!(err("--where 'type=cohort'"), "where: cohort uses MeSH explosion (server-side); use search @6");
        assert!(err("--where 'type in (rct,cohort)'").contains("cohort uses MeSH explosion"));
        assert!(f("-t rct,review").server_only().is_none());
        assert_eq!(f("-t cohort").query("x"), "x AND \"Cohort Studies\"[mh]");
    }

    #[test]
    fn phrase_wildcard_is_prefix_locally() {
        let a = Article { title: "Heart failure outcomes".into(), ..Default::default() };
        assert!(f("--phrase 'heart fail*'").matches(&a));
        assert!(f("--phrase 'heart failure'").matches(&a));
        assert!(!f("--phrase 'heart fail'").matches(&a));
        assert!(!f("--phrase 'heart fail*'").matches(&Article { title: "Heart rate failing".into(), ..Default::default() }));
    }

    #[test]
    fn systematic_subset_is_server_only() {
        let sys = Some("filter: systematic[sb] is server-side; use search");
        for line in ["-t sr", "--cat systematic", "--sys", "--not-type sr", "-t rct,sr"] {
            assert_eq!(f(line).server_only(), sys, "{line}");
        }
        assert_eq!(f("-t meta").server_only(), None);
        assert_eq!(f("-t review").server_only(), None);
        assert_eq!(q("-t sr"), "\"systematic\"[sb]");
        let e = err("--where 'type=sr'");
        assert!(e.contains("systematic[sb] is server-side"), "{e}");
        let e = err("--where 'type in (rct, sr)'");
        assert!(e.contains("systematic[sb] is server-side"), "{e}");
    }

    #[test]
    fn unsupported_tags_are_server_only() {
        assert_eq!(f("--all 'smith[au]'").server_only(), Some("filter: [tag] clauses are server-side; use search"));
        assert_eq!(f("--not 'lancet[ta]'").server_only(), None);
        assert_eq!(f("--all 'statin[ot]'").server_only(), None);
        assert_eq!(f("--all 'cancer[ti] OR diabetes[ti]'").server_only(), Some("filter: [tag] clauses are server-side; use search"));
        for tag in ["all", "tw", "all fields", "text word"] {
            let line = format!("--all 'Panknin[{tag}]'");
            assert_eq!(f(&line).server_only(), Some("filter: [tag] clauses are server-side; use search"), "{tag}");
            assert_eq!(q(&line), format!("Panknin[{tag}]"));
        }
        assert_eq!(q("--all 'smith[au]'"), "smith[au]");
        assert!(f("--all cancer").server_only().is_none());
    }

    #[test]
    fn comma_author_with_given_names_is_one_author() {
        assert_eq!(parse_author("de Azevedo, Walter Filgueira").len(), 1);
        assert_eq!(parse_author("Smith, John").len(), 1);
        assert_eq!(parse_author("Smith J, Jones K").len(), 2);
        assert_eq!(parse_author("Smith JA; Jones KT").len(), 2);
        assert_eq!(parse_author("de Azevedo, W F").len(), 1);
        assert!(f("--author 'de Azevedo, Walter Filgueira'").matches(&crispr_article()));
        assert!(f("--author 'de Azevedo, WF'").matches(&crispr_article()));
    }

    #[test]
    fn last_values_are_bounded() {
        let t = Ymd { y: 2026, m: 10, d: 30 };
        assert_eq!(parse_last("9223372036854775807y", t), Err("too large"));
        assert_eq!(parse_last("99999999999999999999d", t), Err("too large"));
        assert_eq!(parse_last("10001y", t), Err("too large"));
        assert_eq!(parse_last("3650001d", t), Err("too large"));
        assert_eq!(parse_last("10000y", t).unwrap().0.y, 2026 - 10000);
        assert_eq!(parse_last("+5d", t), Err("bad"));
        assert_eq!(parse_last("d", t), Err("bad"));
        assert_eq!(err("--last 9223372036854775807y"), "--last: too large");
        assert_eq!(err("--within 99999999999m"), "--within: too large");
        assert_eq!(err("--new 5q"), "--new: bad '5q' (30d 6w 6m 2y)");
    }

    #[test]
    fn unicode_inputs_do_not_panic() {
        let a = Article { title: "Ünïcode ß sepsis İstanbul".into(), ..Default::default() };
        for line in ["--all 'ß[ti]'", "--ti 'İstanbul[ti]'", "--grep 'ünï'", "--phrase 'ß sepsis'", "--near 'ß sepsis:2'"] {
            let _ = f(line).matches(&a);
        }
        assert_eq!(parse_last("５d", Ymd { y: 2026, m: 10, d: 30 }), Err("bad"));
    }

    #[test]
    fn extra_query_initials_do_not_match_without_fuzzy() {
        assert!(ok("--author \"Smith JA\""));
        assert!(ok("--author \"Smith J\""));
        assert!(!ok("--author \"Doe AB\""));
        assert!(ok("--author \"Doe AB\" --fuzzy"));
    }

    #[test]
    fn year_matches_print_or_epub_date() {
        let mut a = art();
        a.date = "2021-11".into();
        a.epub = "2021-06-19".into();
        assert!(f("--year 2021-06").matches(&a));
        assert!(f("--year 2021-11").matches(&a));
        assert!(!f("--year 2021-08").matches(&a));
    }

    #[test]
    fn partial_record_dates_index_as_first_day() {
        let mut a = art();
        a.date = "2023-11".into();
        a.year = 2023;
        a.epub.clear();
        assert!(!f("--from 2023-11-15").matches(&a));
        assert!(f("--from 2023-11-01").matches(&a));
        assert!(f("--year 2023-11").matches(&a));
        assert!(f("--to 2023-11-15").matches(&a));
        a.date = "2023".into();
        assert!(!f("--from 2023-02").matches(&a));
        assert!(f("--year 2023").matches(&a));
    }

    #[test]
    fn epub_after_print_does_not_satisfy_publication_date() {
        let mut a = art();
        a.date = "2014-09".into();
        a.year = 2014;
        a.epub = "2014-10-03".into();
        assert!(!f("--year 2014-10").matches(&a));
        assert!(f("--year 2014-09").matches(&a));
        a.epub = "2014-08-03".into();
        assert!(f("--year 2014-08").matches(&a));
    }

    #[test]
    fn original_book_date_counts_for_chapters() {
        let mut a = art();
        a.date = "2002-03-04".into();
        a.year = 2002;
        a.epub.clear();
        a.book_date = "1993".into();
        assert!(f("--year 1993").matches(&a));
        assert!(f("--year 2002").matches(&a));
        assert!(!f("--year 1995").matches(&a));
    }

    #[test]
    fn phrase_and_near_stay_inside_one_field() {
        assert!(ok("--phrase \"randomized trial\""));
        assert!(ok("--phrase \"atorvastatin in 300\""));
        assert!(!ok("--phrase \"trial we\""));
        assert!(!ok("--near \"trial studied:0\""));
        assert!(ok("--near \"heart trial:3\""));
    }

    #[test]
    fn ab_scope_keeps_wildcards() {
        assert!(ok("atorvast* --ab"));
        assert!(!ok("atorvastx* --ab"));
        assert!(ok("atorvastatin patients --ab"));
    }

    #[test]
    fn two_letter_surnames_and_date_only_fuzzy() {
        assert_eq!(q("--author \"John Wu\""), "wu j[au]");
        assert_eq!(q("--author \"Wu J\""), "wu j[au]");
        let d = q("--year 2024 --author \"Smyth JA\" --fuzzy");
        assert!(d.contains("2024") && !d.contains("smyth"), "{d}");
    }

    #[test]
    fn local_mirror() {
        assert!(ok("--all statin*,failure --ti statins"));
        assert!(!ok("--ti atorvastatin"));
        assert!(ok("--ab atorvastatin"));
        assert!(ok("--not cancer"));
        assert!(!ok("--not heart"));
        assert!(ok("--mesh failure --major heart"));
        assert!(ok("--age aged --species human --lang en"));
        assert!(!ok("--sex f"));
        assert!(ok("-a \"smith j\" --first-author muller"));
        assert!(!ok("--last-author smith"));
        assert!(ok("-a \"Smyth JA\" --fuzzy"));
        assert!(!ok("-a \"Smyth JA\""));
        assert!(ok("--aff \"harvard medical\" --country usa"));
        assert!(ok("--aff \"univ toronto\""));
        assert!(!ok("--aff stanford"));
        assert!(ok("--aff \"Havard Med\" --fuzzy"));
        assert!(ok("-j lancet"));
        assert!(ok("-j \"The Lancet\" --fuzzy"));
        assert!(!ok("-j \"Lancet Oncol\" --fuzzy"));
        assert!(ok("--issn 01406736"));
        assert!(ok("--year 2020-2022"));
        assert!(ok("--from 2021-03 --to 2021-03"));
        assert!(!ok("--from 2021-04"));
        assert!(!ok("--year 2019"));
        assert!(ok("-t rct --not-type review"));
        assert!(!ok("-t review"));
        assert!(ok("--free --abstract --data --nct NCT01234567") && ok("--nct"));
        assert!(ok("--quartile 1,2 --sjr-min 10 --hindex-min 500 --area medicine"));
        assert!(!ok("--quartile 3"));
        assert!(ok("--cites-min 25 --rcr-min 2 --clinical"));
        assert!(!ok("--cites-min 26"));
        assert!(!ok("--has-comments"));
        assert!(ok("--published --no-retracted"));
        assert!(!ok("--preprint"));
    }

    #[test]
    fn where_partial_dates_compare_by_period() {
        let at = |d: &str| {
            let mut a = rich();
            a.date = d.into();
            a
        };
        let m = |d: &str, w: &str| f(&format!("-w \"{w}\"")).matches(&at(d));
        assert!(!m("2020-09", "date<2020-09-01"));
        assert!(m("2020-09", "date>=2020-09-01"));
        assert!(m("2020-09", "date=2020-09"));
        assert!(m("2020-09", "date=2020"));
        assert!(!m("2020-09", "date=2020-09-15"));
        assert!(!m("2020-09", "date>2020-09"));
        assert!(m("2020-09", "date<=2020-09"));
        assert!(m("2020-09", "date<2020-10"));
        assert!(!m("2020-09", "date>=2020-10"));
        assert!(m("2020-09", "date>2019/12/31"));
        assert!(m("2020-09-15", "date=2020-09") && m("2020-09-15", "date=2020-09-15") && !m("2020-09-15", "date=2020-09-16"));
        assert!(m("2020-09-15", "date>2020-09-14") && !m("2020-09-15", "date>2020-09-15") && m("2020-09-15", "date>=2020-09-15"));
        assert!(m("2020", "date>=2020-12") && !m("2020", "date>2020") && m("2020", "date<2021"));
        assert_eq!(err("-w \"date<2020-13-45x\""), "where: 'date<' needs a date (YYYY[-MM[-DD]]), got '2020-13-45x' @6");
    }

    #[test]
    fn where_date_lists_compare_by_period() {
        let at = |d: &str| {
            let mut a = rich();
            a.date = d.into();
            a
        };
        let m = |d: &str, w: &str| f(&format!("-w \"{w}\"")).matches(&at(d));
        assert!(m("2024-10-01", "date in ('2024-10','2024-11')"));
        assert!(!m("2024-10-01", "date not in ('2024-10','2024-11')"));
        assert!(!m("2024-12-01", "date in (2024-10,2024-11)"));
        assert!(m("2024-12-01", "date not in (2024-10,2024-11)"));
        assert!(m("2024-10-15", "date in (2024-10-15, 2023)"));
        assert!(m("2023-02", "date in 2024-10,2023"));
        assert_eq!(err("-w \"date in (2024-1x)\""), "where: 'date in' needs dates (YYYY[-MM[-DD]]), got '2024-1x' @9");
    }

    #[test]
    fn where_expr() {
        assert!(!ok("--where \"year>=2020 & q<=2 & cites>10 & aff~stanford\""));
        assert!(ok("--where \"year>=2020 & q<=2 & cites>10 & aff~harvard\""));
        assert!(ok("-w \"title~'heart failure' & authors~muller & type=\\\"Randomized Controlled Trial\\\"\""));
        assert!(ok("-w \"journal=lancet & jt~london & sjr>12.4 & h>=600 & rcr>2 & nrefs=40\""));
        assert!(ok("-w \"mesh!~cancer & kw=statin & lang=eng & nct=NCT01234567 & country~eng\""));
        assert!(ok("-w \"date>=2021-03 & date<2021-04 & pmid=1 & doi!=x\""));
        assert!(!ok("-w \"pct>1\""));
        assert!(ok("-w \"pct!=1\""));
        assert!(!ok("-w \"authors!~smith\""));
        assert!(ok("-w \"authors!=nobody\""));
        assert!(ok("-w \"year > 2020 && year < 2022\""));
        assert_eq!(f("-w \"cites>1 & title~x\"").need(), Need { rank: false, metrics: true, expand: false });
        assert!(f("-w \"year>1\"").needs_post());
        assert_eq!(err("-w \"foo>1\""), format!("where: unknown field 'foo' ({WHERE_FIELDS}) @1"));
        assert_eq!(err("-w \"year\""), "where: no operator after 'year' (= != > >= < <= ~ !~ ~~ !~~ % in) @5");
        assert_eq!(err("-w \"year>abc\""), "where: 'year>' needs a number, got 'abc' @6");
        assert_eq!(err("-w \"year>\""), "where: empty value in 'year>' @6");
        assert_eq!(err("-w \"&\""), "where: expected field, got '&' @1");
        assert_eq!(err("-w \" \""), "where: empty expression");
    }

    fn rich() -> Article {
        let mut a = art();
        a.abs = vec![
            ("BACKGROUND".into(), "Hepatitis B is common.".into()),
            ("RESULTS".into(), "Atorvastatin lowered LDL (p < 0.05) in 300 patients.".into()),
            ("CONCLUSIONS".into(), "Statins help. Further trials are needed.".into()),
        ];
        a.epub = "2021-02-01".into();
        a
    }

    fn okr(line: &str) -> bool {
        f(line).matches(&rich())
    }

    #[test]
    fn grep_family() {
        assert!(okr("--grep \"p\\s*<\\s*0\\.05\""));
        assert!(okr("--grep HEPATITIS"));
        assert!(!okr("--grep \"(?-i)HEPATITIS\""));
        assert!(okr("--grep hepatitis --grep atorva"));
        assert!(!okr("--grep hepatitis --grep nothing"));
        assert!(okr("--grep-ti \"^statins in\" --grep-ab \"ldl\""));
        assert!(!okr("--grep-ti atorvastatin"));
        assert!(!okr("--grep-ab \"randomized\""));
        assert!(okr("--no-grep cancer"));
        assert!(!okr("--no-grep \"heart|cancer\""));
        assert!(!okr("--no-grep cancer --no-grep ldl"));
        assert!(okr("--grep \"statin\" --no-grep cancer"));
        assert!(okr("--grep \"Müller|statin\""));
        let x = f("--grep a --grep b");
        assert!(x.needs_post());
        assert_eq!(x.local, ["grep"]);
        assert_eq!(x.query("x"), "x");
        assert_eq!(x.query(""), "");
        assert_eq!(x.empty_query_err(), "regex needs a base query: add terms or filters");
        assert_eq!(f("-t rct").empty_query_err(), "empty query: give terms or filters");
        assert_eq!(f("--where \"title~~x\"").empty_query_err(), "regex needs a base query: add terms or filters");
        assert_eq!(f("--grep a --no-grep b").describe_local().unwrap(), "grep a; no-grep b");
        let e = err("--grep \"(\"");
        assert!(e.starts_with("bad regex '(': ") && !e.contains('\n'), "{e}");
        assert!(err("--no-grep \"[\"").starts_with("bad regex '[': "));
    }

    #[test]
    fn fuzzy_terms() {
        assert!(okr("--fz hepatits"));
        assert!(okr("--fz atorvastain"));
        assert!(okr("--fz atorvastatin --fz hepatitis"));
        assert!(!okr("--fz atorvastatin --fz cirrhosis"));
        assert!(okr("--fz \"hepatitus b\""));
        assert!(okr("--fz statin*"));
        assert!(!okr("--fz cancr"));
        assert!(okr("--fz LDL"));
        assert!(!okr("--fz LDX"));
        assert_eq!(q("--fz hepatitis"), "");
        let x = f("--fz hepatitis");
        assert!(x.needs_post());
        assert_eq!(x.local, ["fz"]);
        assert_eq!(err("--fz \"*\""), "--fz: empty term");
    }

    #[test]
    fn phrase_and_near() {
        assert_eq!(q("--phrase \"heart failure\""), "\"heart failure\"[tiab]");
        assert_eq!(q("--near \"heart failure:2\""), "\"heart failure\"[tiab:~2]");
        assert_eq!(q("--near \"heart failure\""), "\"heart failure\"[tiab:~3]");
        assert_eq!(q("x --phrase a-b --near \"a b c:1\""), "x AND \"a-b\"[tiab] AND \"a b c\"[tiab:~1]");
        assert!(f("--phrase a --near \"a b\"").needs_post());
        assert!(!f("--near \"a b\"").needs_post());
        assert!(okr("--phrase \"heart failure\""));
        assert!(okr("--phrase \"HEART  failure\""));
        assert!(!okr("--phrase \"failure heart\""));
        assert!(!okr("--phrase \"statins heart\""));
        assert!(okr("--phrase \"p 0 05\""));
        assert!(okr("--near \"failure heart:0\""));
        assert!(okr("--near \"statins failure:2\""));
        assert!(!okr("--near \"statins failure:1\""));
        assert!(okr("--near \"statins randomized:4\""));
        assert!(!okr("--near \"statins randomized:3\""));
        assert!(okr("--near \"statins heart failure:1\""));
        assert!(!okr("--near \"statins heart failure:0\""));
        assert!(!okr("--near \"statins trial hepatitis:2\""));
        assert!(okr("--near \"atorvastatin patients:7\""));
        assert!(!okr("--near \"atorvastatin patients:6\""));
        assert!(err("--near solo").starts_with("--near: need 2-8"));
        assert!(err("--near \"a a\"").starts_with("--near: need 2-8"));
        assert_eq!(err("--phrase \"*\""), "--phrase: empty");
    }

    #[test]
    fn recency() {
        let o = f("--since 2020 --until 2021-06").search_opts();
        assert_eq!((o.mindate.as_deref(), o.maxdate.as_deref(), o.datetype.as_deref()), (Some("2020"), Some("2021/06"), Some("pdat")));
        let o = f("--within 6m").search_opts();
        assert_eq!(o.datetype.as_deref(), Some("pdat"));
        assert_eq!(f("--within 2w").search_opts().datetype.as_deref(), Some("pdat"));
        let both = f("sepsis --last 30d --new 7d");
        assert!(both.search_opts().datetype.is_none());
        let q = both.query("sepsis");
        assert!(q.contains("[dp]") && q.contains("[crdt]") && q.starts_with("sepsis AND "), "{q}");
        let o = f("--new 6m").search_opts();
        assert!(o.datetype.is_none() && o.mindate.is_none());
        assert!(f("--new 6m").query("").ends_with(":3000[crdt]"));
        assert_eq!(f("--new 7d --datetype pdat").search_opts().datetype.as_deref(), Some("pdat"));
        assert_eq!(f("--new 7d --datetype mdat").search_opts().datetype.as_deref(), Some("mdat"));
        assert_eq!(err("--new 5"), "--new: bad '5' (30d 6w 6m 2y)");
        assert_eq!(err("--within x"), "--within: bad 'x' (30d 6w 6m 2y)");
        assert_eq!(f("--sort recent").search_opts().sort.as_deref(), Some("most_recent"));
        assert!(ok("--since 2021 --until 2021-03-15"));
        assert!(!ok("--since 2021-03-16"));
        assert!(ok("--new 100y"));
        let mut a = art();
        a.epub = "2000-01-01".into();
        a.date = "1999-12".into();
        assert!(f("--new 100y --from 1999").matches(&a));
        assert!(!f("--datetype edat --from 2010").matches(&a));
        assert!(f("--datetype edat --from 1999 --to 2000").matches(&a));
        a.epub.clear();
        assert!(f("--datetype edat --from 1999 --to 1999-12").matches(&a));
        assert!(!f("--datetype edat --from 2001").matches(&a));
        assert!(f("--datetype mdat --from 2030").matches(&a));
        assert!(!f("--since 2021").needs_post());
    }

    #[test]
    fn where_precedence_and_parens() {
        assert!(ok("-w \"year=1999 || year=2021 & q=1\""));
        assert!(!ok("-w \"(year=1999 || year=2021) & q=2\""));
        assert!(ok("-w \"year=1999 || year=2021 & q=1\""));
        assert!(!ok("-w \"year=2021 & q=2 || year=1999\""));
        assert!(ok("-w \"year=2021 & q=2 || q=1\""));
        assert!(ok("-w \"!(year=1999)\""));
        assert!(!ok("-w \"!year=2021\""));
        assert!(ok("-w \"not year=1999 & not q=3\""));
        assert!(ok("-w \"not (year=1999 || q=3)\""));
        assert!(!ok("-w \"not(year=2021)\""));
        assert!(ok("-w \"!!year=2021\""));
        assert!(ok("-w \"((year>=2020) && (q<=1)) || cites>1000\""));
        assert!(ok("-w \"year=1999 || year=2000 || year=2021\""));
        assert!(ok("--where \"year=2021\" --where \"q=1\""));
        assert!(!ok("--where \"year=2021\" --where \"q=2\""));
        assert!(ok("-w \"year>=2020 & year<=2022 & cites>10 && q=1\""));
        assert!(ok("-w \"title~heart failure & authors~muller\""));
        assert!(ok("-w \"year = 2021 && ( q = 1 )\""));
        assert!(ok("-w \"type~'randomized' & not mesh~cancer\""));
        assert_eq!(f("-w \"cites>1 || q<3\"").need(), Need { rank: true, metrics: true, expand: false });
        assert_eq!(f("-w \"not cites>1\"").need(), Need { rank: false, metrics: true, expand: false });
    }

    #[test]
    fn where_ops() {
        assert!(okr("-w \"abs~~'p\\s*<\\s*0\\.05'\""));
        assert!(okr("-w \"text~~(heart|lung)\""));
        assert!(okr("-w \"(text~~lung|heart) & year=2021\""));
        assert!(okr("-w \"title~~^statins\""));
        assert!(!okr("-w \"title~~'(?-i)^STATINS'\""));
        assert!(okr("-w \"title !~~ lung\""));
        assert!(!okr("-w \"title !~~ heart\""));
        assert!(okr("-w \"abs % hepatits\""));
        assert!(okr("-w \"text % atorvastain & title !% cancr\""));
        assert!(!okr("-w \"title % atorvastatin\""));
        assert!(okr("-w \"title %statin*\""));
        assert!(okr("-w \"type in (rct, review)\""));
        assert!(!okr("-w \"type in (review, editorial)\""));
        assert!(okr("-w \"type not in (review, editorial)\""));
        assert!(okr("-w \"type = rct\""));
        assert!(okr("-w \"type != review\""));
        assert!(okr("-w \"q in (1,2)\""));
        assert!(!okr("-w \"q in (3, 4)\""));
        assert!(okr("-w \"q not in (3)\""));
        assert!(okr("-w \"year in(2020, 2021)\""));
        assert!(okr("-w \"pct not in (1, 2)\""));
        assert!(!okr("-w \"pct in (1)\""));
        assert!(okr("-w \"lang in (en, fr)\""));
        assert!(okr("-w \"lang=en\""));
        assert!(!okr("-w \"lang in (fr)\""));
        assert!(okr("-w \"mesh in ('Heart Failure', cancer)\""));
        assert!(okr("-w \"concl~'further trials' & concl !~ 'ldl'\""));
        assert!(okr("-w \"concl~~'^Statins help'\""));
        assert!(okr("-w \"abs~hepatitis & text~atorvastatin & title~heart\""));
        assert!(okr("-w \"nauthors=3 & ncites=25 & first~muller & last~doe\""));
        assert!(okr("-w \"first!=smith & last!~smith\""));
        assert!(!okr("-w \"first=smith\""));
        assert!(okr("-w \"epub>=2021-02 & epub<2021-03 & epub!=x\""));
        assert!(okr("-w \"age>=0 & age<100000\""));
        assert!(!okr("-w \"age<0\""));
        let mut a = rich();
        a.authors.clear();
        a.epub.clear();
        assert!(!f("-w \"first~x\"").matches(&a));
        assert!(f("-w \"first!~x & last!=x & epub!=2021\"").matches(&a));
        assert!(!f("-w \"epub<2030\"").matches(&a));
        assert!(f("-w \"nauthors=0\"").matches(&a));
        a.abs.clear();
        assert!(f("-w \"concl!~x\"").matches(&a));
        a.abs = vec![("".into(), "Old. First idea. Second idea. Final words here.".into())];
        assert!(f("-w \"concl~'second idea' & concl !~ 'first idea'\"").matches(&a));
    }

    #[test]
    fn where_errors() {
        assert_eq!(err("-w \"(year=1\""), "where: expected ')' @8");
        assert_eq!(err("-w \"year=1)\""), "where: unexpected ')' @7");
        assert_eq!(err("-w \"year=1 &\""), "where: expected condition @9");
        assert_eq!(err("-w \"year=1 || \""), "where: expected condition @11");
        assert_eq!(err("-w \"year=1 & ~x\""), "where: expected field, got '~' @10");
        assert_eq!(err("-w \"q in (1\""), "where: expected ',' or ')' @8");
        assert_eq!(err("-w \"q in (1,\""), "where: empty list item @9");
        assert_eq!(err("-w \"q in (1 2)\""), "where: 'q in' needs numbers, got '1 2' @6");
        assert_eq!(err("-w \"q in ('1' 2)\""), "where: expected ',' or ')' @11");
        assert_eq!(err("-w \"q in ()\""), "where: empty list item @7");
        assert_eq!(err("-w \"q in (a)\""), "where: 'q in' needs numbers, got 'a' @6");
        assert_eq!(err("-w \"q not in (a)\""), "where: 'q not in' needs numbers, got 'a' @10");
        assert_eq!(err("-w \"title~'abc\""), "where: unterminated quote @7");
        assert_eq!(err("-w \"title~'abc' x\""), "where: unexpected 'x' after value @13");
        assert!(err("-w \"title~~(\"").starts_with("where: bad regex '(': "));
        assert!(err("-w \"title~~(\"").ends_with(" @8"));
        assert_eq!(err("-w \"title%''\""), "where: empty value in 'title%' @7");
        assert_eq!(err("-w \"title%*\""), "where: empty typo term in 'title%' @7");
        assert_eq!(err("-w \"year>=\""), "where: empty value in 'year>=' @7");
        assert_eq!(err("-w \"age<x\""), "where: 'age<' needs a number, got 'x' @5");
        assert_eq!(err("-w \"year=1 & foo=2\""), format!("where: unknown field 'foo' ({WHERE_FIELDS}) @10"));
    }

    #[test]
    fn age_math() {
        let mut a = art();
        a.date = "2026-10-20".into();
        let now = days_from_civil(2026, 10, 30);
        assert_eq!(age_days(&a, now), Some(10));
        a.date = "2026-10".into();
        assert_eq!(age_days(&a, now), Some(29));
        a.date = "2026".into();
        assert_eq!(age_days(&a, now), Some(302));
        a.date = "2030-01-01".into();
        assert_eq!(age_days(&a, now), Some(0));
        a.date.clear();
        a.year = 0;
        assert_eq!(age_days(&a, now), None);
    }

    #[test]
    fn explain_and_local_names() {
        let x = f("--grep a --fz bb --where \"year>1\" --quartile 1");
        assert_eq!(x.local, ["grep", "fz", "quartile", "where"]);
        assert_eq!(x.describe_local().unwrap(), "grep a; fz bb; where year>1");
        assert!(f("x").describe_local().is_none());
        let p = f("--grep x -t rct");
        let mut a = art();
        assert!(!p.basic(&a, true));
        a.title = "x".into();
        assert!(p.basic(&a, true));
        assert!(!f("--phrase zzz").basic(&a, true));
    }

    #[test]
    fn post_split() {
        let x = f("--quartile 1 --year 2020 -t rct --fuzzy -a \"Smith J\"");
        let mut a = art();
        a.authors.clear();
        assert!(!x.basic(&a, true));
        assert!(x.enriched(&a, true));
        a.jr = None;
        assert!(!x.enriched(&a, true));
        assert!(f("-t review --quartile 1").basic(&art(), true));
        assert!(!f("-t review").basic(&art(), false));
    }
    #[test]
    fn raw_expressions_are_grouped() {
        assert_eq!(q("asthma --ti \"cancer[ti] OR diabetes[ti]\""), "asthma AND (cancer[ti] OR diabetes[ti])");
        assert_eq!(q("asthma --not \"a[ti] OR b[ti]\""), "asthma NOT (a[ti] OR b[ti])");
        assert_eq!(q("x --mesh Asthma"), "x AND \"Asthma\"[mh]");
        assert!(top_level_operator("a AND b", true));
        assert!(!top_level_operator("a AND b", false));
        assert!(!top_level_operator("(a OR b)", true));
        assert!(!top_level_operator("\"a OR b\"[tiab]", true));
        assert_eq!(group("(a OR b) AND c", true), "((a OR b) AND c)");
        assert_eq!(group("(a OR b) NOT c", false), "((a OR b) NOT c)");
    }

    #[test]
    fn fuzzy_server_terms_are_soft() {
        assert_eq!(q("asthma -a \"Smyth JA\" --fuzzy"), "asthma");
        assert_eq!(q("-a \"Smith JA\" --fuzzy"), "smith ja*[au]");
        assert_eq!(q("asthma -i Havard --fuzzy -j Lancet --fuzzy"), "asthma");
        assert!(f("asthma -a \"Smyth JA\" --fuzzy").needs_post());
        assert_eq!(q("asthma -a \"Smith JA\""), "asthma AND smith ja[au]");
    }

    #[test]
    fn bare_text_scope_flags() {
        assert_eq!(q("cancer --ti"), "cancer[ti]");
        assert_eq!(q("statin dementia --tiab"), "statin[tiab] AND dementia[tiab]");
        assert_eq!(q("heart failure OR sepsis --tiab"), "heart[tiab] AND failure[tiab] OR sepsis[tiab]");
        assert_eq!(q("(a OR b) AND c --ti"), "(a[ti] OR b[ti]) AND c[ti]");
        assert_eq!(q("cancer[ti] x --tiab"), "cancer[ti] x");
        let ab = f("placebo --ab");
        assert!(ab.needs_post());
        let mut a = art();
        a.title = "placebo in title".into();
        a.abs = vec![("".into(), "no mention".into())];
        assert!(!ab.basic(&a, true));
        a.abs = vec![("".into(), "a placebo arm".into())];
        assert!(ab.basic(&a, true));
    }

    #[test]
    fn collection_title_counts_for_title_matches() {
        let mut a = art();
        a.title = "Chapter on antibiotics".into();
        a.coll_title = "NICE Process and Methods Guides".into();
        assert!(f("--ti methods").basic(&a, false));
        assert!(f("--tiab methods").basic(&a, false));
        assert!(f("--phrase 'methods guides'").basic(&a, false));
        assert!(!f("--not methods").basic(&a, false));
        a.coll_title.clear();
        assert!(!f("--ti methods").basic(&a, false));
    }

    #[test]
    fn has_problems_mirrors_server_union() {
        for rel in [
            "hasretractionin",
            "hasretractionof",
            "haserratumin",
            "haserratumfor",
            "hasexpressionofconcernin",
            "hasexpressionofconcernfor",
            "\"Retracted Publication\"[pt]",
            "\"Retraction Notice\"[pt]",
            "\"Expression of Concern\"[pt]",
            "\"Published Erratum\"[pt]",
        ] {
            assert!(PROBLEMS_QUERY.contains(rel), "{rel}");
        }
        let x = f("--has-problems");
        let mut a = art();
        assert!(!x.matches(&a));
        for kind in ["RetractionIn", "RetractionOf", "ErratumIn", "ErratumFor", "ExpressionOfConcernIn", "ExpressionOfConcernFor"] {
            a.corrs = vec![Corr { kind: kind.into(), ..Default::default() }];
            assert!(x.matches(&a), "{kind}");
        }
        a.corrs = vec![Corr { kind: "PartialRetractionIn".into(), ..Default::default() }];
        assert!(!x.matches(&a));
        a.corrs.clear();
        for t in ["Retracted Publication", "Retraction Notice", "Expression of Concern", "Published Erratum"] {
            a.types = vec![t.into()];
            assert!(x.matches(&a), "{t}");
        }
    }

    #[test]
    fn has_comments_mirrors_server_union() {
        assert_eq!(q("--has-comments"), "(hascommentin OR hascommenton)");
        let x = f("--has-comments");
        let mut a = art();
        assert!(!x.matches(&a));
        for kind in ["CommentIn", "CommentOn"] {
            a.corrs = vec![Corr { kind: kind.into(), ..Default::default() }];
            assert!(x.matches(&a), "{kind}");
        }
        a.corrs = vec![Corr { kind: "ErratumIn".into(), ..Default::default() }];
        assert!(!x.matches(&a));
    }

    #[test]
    fn bare_ab_keeps_quoted_phrases() {
        let x = f("'\"heart failure\" trial' --ab");
        let mut a = art();
        a.title = "Heart failure".into();
        a.abs = vec![("".into(), "Heart disease can cause renal failure; trial".into())];
        assert!(!x.basic(&a, true));
        a.abs = vec![("".into(), "A heart failure trial".into())];
        assert!(x.basic(&a, true));
        let w = f("'\"heart fail*\"' --ab");
        assert!(w.basic(&a, true));
    }

    #[test]
    fn comma_space_separates_country_journal_and_grant_lists() {
        assert_eq!(q("--country \"US, UK\""), q("--country US,UK"));
        assert_eq!(q("-j \"Lancet, JAMA\""), q("-j Lancet,JAMA"));
        assert_eq!(q("--grant \"R01 CA1, U01 X\""), q("--grant \"R01 CA1,U01 X\""));
        assert!(q("--country \"US, UK\"").contains(" OR "));
    }

    #[test]
    fn valued_ab_checks_abstract_only() {
        let x = f("--ab placebo");
        assert!(x.needs_post());
        let mut a = art();
        a.title = "placebo trial".into();
        a.abs = vec![("".into(), "nothing here".into())];
        assert!(!x.basic(&a, true));
        a.abs = vec![("".into(), "placebo given".into())];
        assert!(x.basic(&a, true));
        assert!(!f("--ti placebo").needs_post());
    }

    #[test]
    fn where_limits() {
        let deep = format!("{}year=1{}", "(".repeat(1_500), ")".repeat(1_500));
        assert_eq!(err(&format!("-w \"{deep}\"")), "where: too deeply nested");
        let nots = format!("{}year=1", "!".repeat(500));
        assert_eq!(err(&format!("-w \"{nots}\"")), "where: too deeply nested");
        let ok = format!("{}year=1{}", "(".repeat(30), ")".repeat(30));
        assert!(Filters::from_args(&Args::parse(&["-w".into(), ok]).unwrap()).is_ok());
        let long = format!("year=1{}", " & year=1".repeat(600));
        assert_eq!(err(&format!("-w \"{long}\"")), "where: expression too long (max 4000 chars)");
    }

    #[test]
    fn ab_scope_needs_plain_words() {
        assert_eq!(err("\"sepsis OR shock\" --ab"), "--ab needs plain words (no boolean/field syntax)");
        assert_eq!(err("\"sepsis[ti]\" --ab"), "--ab needs plain words (no boolean/field syntax)");
        assert!(Filters::from_args(&Args::parse(&["sepsis".into(), "shock".into(), "--ab".into()]).unwrap()).is_ok());
    }

    #[test]
    fn comma_initials_in_queries() {
        assert_eq!(q("--author \"Cheng, KT\""), "cheng kt[au]");
        let by = |name: &str| Article { authors: vec![Author { name: name.into(), ..Default::default() }], ..Default::default() };
        assert!(!f("--author \"Cheng, KT\"").matches(&by("Cheng KK")));
        assert!(f("--author \"Cheng, KT\"").matches(&by("Cheng KT")));
    }

    #[test]
    fn server_only_filters_flagged() {
        assert_eq!(f("--new 7d").server_only(), Some("filter: entry-date filters are server-side; use search"));
        assert_eq!(f("--from 2020 --datetype crdt").server_only(), Some("filter: entry-date filters are server-side; use search"));
        assert_eq!(f("--free").server_only(), Some("filter: --free is server-side; use search"));
        assert_eq!(f("--last 7d").server_only(), None);
        assert_eq!(f("--year 2020").server_only(), None);
        assert_eq!(f("--fulltext").server_only(), Some("filter: --fulltext is server-side; use search"));
        assert_eq!(f("--oa").server_only(), Some("filter: --oa is server-side; use search"));
        assert_eq!(f("--data").server_only(), Some("filter: --data is server-side; use search"));
    }

    #[test]
    fn server_sort_direction() {
        assert_eq!(err("--sort pubdate --asc"), "--asc not supported for server sort pubdate (use search -n N :: sort pubdate --asc)");
        assert_eq!(err("--sort author --desc"), "--desc not supported for server sort author (use search -n N :: sort author --desc)");
        assert!(Filters::from_args(&Args::parse(&["--sort".into(), "author".into(), "--asc".into()]).unwrap()).is_ok());
        assert!(Filters::from_args(&Args::parse(&["--sort".into(), "cites".into(), "--asc".into()]).unwrap()).is_ok());
        assert!(Filters::from_args(&Args::parse(&["--sort".into(), "date".into(), "--desc".into()]).unwrap()).is_ok());
    }

    #[test]
    fn year_buckets_respect_partial_bounds() {
        let x = f("--from 2024-07 --to 2026-02-10");
        let o = x.year_opts(2024);
        assert_eq!((o.mindate.as_deref(), o.maxdate.as_deref()), (Some("2024/07"), Some("2024")));
        let o = x.year_opts(2025);
        assert_eq!((o.mindate.as_deref(), o.maxdate.as_deref()), (Some("2025"), Some("2025")));
        let o = x.year_opts(2026);
        assert_eq!((o.mindate.as_deref(), o.maxdate.as_deref()), (Some("2026"), Some("2026/02/10")));
        let o = f("--from 2024-07 --to 2024-07").year_opts(2024);
        assert_eq!((o.mindate.as_deref(), o.maxdate.as_deref()), (Some("2024/07"), Some("2024/07")));
    }
    #[test]
    fn symbolic_operators_are_grouped() {
        assert_eq!(group("cancer[ti] | asthma[ti]", false), "(cancer[ti] | asthma[ti])");
        assert_eq!(group("a or b", false), "(a or b)");
        assert_eq!(group("a & b", true), "(a & b)");
        assert_eq!(group("a & b", false), "a & b");
        assert_eq!(group("\"a | b\"[ti]", false), "\"a | b\"[ti]");
        assert_eq!(q("sepsis --ti 'cancer[ti] | asthma[ti]'"), "sepsis AND (cancer[ti] | asthma[ti])");
    }

    #[test]
    fn comma_initials_with_spaces_stay_one_author() {
        let p = parse_author("de Azevedo, W F");
        assert_eq!(p, vec![Person { last: "de azevedo".into(), ini: "wf".into() }]);
        assert_eq!(parse_author("Cheng, K T")[0].ini, "kt");
        assert_eq!(parse_author("Smith J, Lee K").len(), 2);
        assert_eq!(parse_author("Cheng, KT").len(), 1);
        assert_eq!(parse_author("A B; C D").len(), 2);
    }

    #[test]
    fn phrase_runs_locally() {
        let x = f("--phrase \"heart failure\"");
        assert!(x.needs_post());
        assert_eq!(x.local, ["phrase"]);
        let mut a = Article { title: "Failure of the heart".into(), ..Default::default() };
        assert!(!x.basic(&a, true));
        a.title = "Chronic heart failure care".into();
        assert!(x.basic(&a, true));
    }

    #[test]
    fn year_and_new_compose_without_reversal() {
        let x = f("--year 2025 --new 7d");
        let q = x.query("sepsis");
        assert!(q.starts_with("sepsis AND 2025:2025[dp] AND "), "{q}");
        assert!(q.ends_with(":3000[crdt]"), "{q}");
        assert!(x.search_opts().mindate.is_none());
        let d = x.describe_dates().unwrap();
        assert!(d.starts_with("pdat 2025..2025; crdt "), "{d}");
        assert!(x.server_only().is_some());
        let past = f("--year 2020 --new 7d");
        assert!(past.query("x").contains("2020:2020[dp]"));
    }

    #[test]
    fn keywords_count_as_tiab_but_not_ti_or_ab() {
        let a = Article {
            title: "Docking tutorial".into(),
            abs: vec![(String::new(), "A protein ligand program.".into())],
            kw: vec!["MolDock".into(), "Protein-ligand interactions".into()],
            ..Default::default()
        };
        assert!(f("--all MolDock").matches(&a));
        assert!(f("--any moldock,zzz").matches(&a));
        assert!(!f("--not MolDock").matches(&a));
        assert!(f("--tiab MolDock").matches(&a));
        assert!(f("--phrase 'protein ligand interactions'").matches(&a));
        assert!(f("--all 'MolDock[tiab]'").matches(&a));
        assert!(!f("--ti MolDock").matches(&a));
        assert!(!f("--all 'MolDock[ti]'").matches(&a));
        assert!(!f("--all 'MolDock[ab]'").matches(&a));
        assert!(!f("--all 'Dock'").matches(&a));
    }

    #[test]
    fn kw_matches_words_not_substrings() {
        let a = Article { kw: vec!["rate limiting".into(), "Gene editing".into()], ..Default::default() };
        assert!(!f("--kw rat").matches(&a));
        assert!(f("--kw rate").matches(&a));
        assert!(f("--kw 'gene editing'").matches(&a));
        assert!(f("--kw 'rat*'").matches(&a));
        assert!(f("--kw 'limit*'").matches(&a));
        assert!(!f("--tag ed").matches(&a));
        assert!(f("--tag editing").matches(&a));
    }

    #[test]
    fn date_only_query_uses_date_term() {
        assert_eq!(q("--year 2024"), "2024:2024[dp]");
        assert_eq!(q("--from 2020-03 --datetype edat"), "2020/03:3000[edat]");
        assert!(q("--last 7d").ends_with("[dp]"));
        assert!(q("--new 7d").ends_with("[crdt]"));
        assert_eq!(q("sepsis --year 2024"), "sepsis");
        assert_eq!(q("-t rct --year 2024"), "\"Randomized Controlled Trial\"[pt]");
    }

    #[test]
    fn server_only_rejected() {
        assert_eq!(f("--free").reject_server_only().unwrap_err().to_string(), "filter: --free is server-side; use search");
        assert_eq!(f("--oa").reject_server_only().unwrap_err().to_string(), "filter: --oa is server-side; use search");
    }

    #[test]
    fn wildcard_values_cannot_inject_boolean_syntax() {
        let evil = "--kw \"zzzz* OR all[sb] OR zzzz*\"";
        let out = q(evil);
        assert_eq!(out, "\"zzzz* OR allsb OR zzzz*\"[ot]");
        assert_eq!(q("--kw immuno*"), "immuno*[ot]");
        assert_eq!(q("--journal \"natu* OR x[sb]\""), "\"natu* OR xsb\"[ta]");
        assert_eq!(q("--mesh \"abcd* (b)\""), "\"abcd* b\"[mh]");
        assert_eq!(q("--kw \"heart fail*\""), "\"heart fail*\"[ot]");
        assert_eq!(q("--issn \"1234-5678 OR all[sb]\""), "\"1234-5678 OR allsb\"[issn]");
        assert_eq!(q("--lang \"en OR all[sb]\""), "\"en or allsb\"[la]");
        assert_eq!(q("--nct \"NCT01 OR all[sb]\""), "(NCT01 OR all[sb]) AND clinicaltrials.gov[si]");
        assert_eq!(q("--nct NCT01234567"), "NCT01234567[si]");
        assert_eq!(q("--grant \"R01* OR all[sb]\""), "\"R01* OR allsb\"[gr]");
        assert_eq!(q("--grant R01*"), "R01*[gr]");
        assert_eq!(q("--phrase \"a[sb] b\""), "\"asb b\"[tiab]");
    }

    #[test]
    fn short_wildcard_stems_stay_local() {
        let x = f("--tiab \"IL*\"");
        assert!(x.local.contains(&"wildcard"));
        assert!(x.needs_post());
        assert_eq!(x.query("cancer"), "cancer");
        assert!(f("--ab \"IL*\"").needs_post());
        assert_eq!(q("--kw \"IL*\" sepsis"), "sepsis");
        assert_eq!(q("--any \"IL*,sepsis\" asthma"), "asthma");
        assert_eq!(q("--all \"IL*,sepsis\" asthma"), "asthma AND sepsis[tiab]");
        assert_eq!(q("--not \"IL*,sepsis\" asthma"), "asthma NOT sepsis[tiab]");
        assert_eq!(q("--phrase \"IL* signalling\" asthma"), "asthma");
        assert_eq!(q("--ti \"immuno*\" asthma"), "asthma AND immuno*[ti]");
        assert_eq!(q("--phrase \"heart fail*\""), "\"heart fail*\"[tiab]");
        assert!(err("'IL* cytokine' --ab").contains("short wildcard IL*"));
    }

    #[test]
    fn explicit_tags_follow_the_short_wildcard_rule() {
        for line in ["--all \"IL*[ti]\" sepsis", "--any \"IL*[tiab]\" sepsis", "--ti \"IL*[ti]\" sepsis", "--all \"IL*[ab]\" sepsis"] {
            let x = f(line);
            assert!(x.needs_post(), "{line}");
            assert_eq!(x.query("sepsis"), "sepsis", "{line}");
        }
        assert_eq!(q("--all \"immuno*[ti]\" sepsis"), "sepsis AND immuno*[ti]");
        let mut a = art();
        a.title = "IL6 signalling in sepsis".into();
        assert!(f("--all \"IL*[ti]\" sepsis").matches(&a));
        a.title = "Cytokine signalling in sepsis".into();
        assert!(!f("--all \"IL*[ti]\" sepsis").matches(&a));
        assert!(f("--not \"IL*[ti]\" sepsis").matches(&a));
    }

    #[test]
    fn quoted_items_in_type_lists_lose_their_quotes() {
        let typed = |t: &[&str]| Article { types: t.iter().map(|x| x.to_string()).collect(), ..Default::default() };
        let p3 = typed(&["Clinical Trial, Phase III"]);
        let rev = typed(&["Review"]);
        assert_eq!(q("--type '\"Clinical Trial, Phase III\",Review'"), "(\"Clinical Trial, Phase III\"[pt] OR \"Review\"[pt])");
        assert!(f("--type '\"Clinical Trial, Phase III\",Review'").matches(&p3));
        assert!(f("--type '\"Clinical Trial, Phase III\",Review'").matches(&rev));
        assert!(!f("--not-type '\"Clinical Trial, Phase III\"'").matches(&p3));
        assert!(f("--not-type '\"Clinical Trial, Phase III\"'").matches(&rev));
        assert!(f("--cat '\"Clinical Trial, Phase III\"'").matches(&p3));
        assert!(f("-w 'type=\"Clinical Trial, Phase III\"'").matches(&p3));
    }

    #[test]
    fn revised_date_of_books_counts_for_local_year_filters() {
        let mut a = art();
        a.date = "2002-03-04".into();
        a.year = 2002;
        a.epub.clear();
        assert!(!f("--year 2026").matches(&a));
        a.revised = "2026-10-08".into();
        assert!(f("--year 2026").matches(&a));
        assert!(f("--from 2026-10 --to 2026-10").matches(&a));
        assert!(f("--year 2002").matches(&a));
        assert!(!f("--year 2010").matches(&a));
    }

    #[test]
    fn compound_clauses_with_short_wildcards_bail() {
        assert_eq!(
            err("--all '(IL6*[tiab] OR IL8*[tiab])' sepsis"),
            "--all: short wildcard IL6* needs >=4 chars before * (compound clause)"
        );
        assert!(err("--any '\"TNF*\" OR asthma[ti]' sepsis").contains("short wildcard TNF*"));
        assert!(err("--not 'a AND (b OR IL*[tiab])' sepsis").contains("short wildcard IL*"));
        assert_eq!(
            err("(IL6* OR IL8*) AND sepsis"),
            "short wildcard IL6* needs >=4 chars before * (PubMed ignores shorter stems)"
        );
        assert!(err("IL*").contains("short wildcard IL*"));
        assert!(Filters::from_args(&Args::parse(&crate::args::split_words("--all '(immuno*[tiab] OR cytokin*[tiab])' sepsis").unwrap()).unwrap()).is_ok());
        assert!(Filters::from_args(&Args::parse(&crate::args::split_words("immuno* sepsis").unwrap()).unwrap()).is_ok());
    }

    #[test]
    fn short_wildcard_in_or_group_with_server_only_tag_bails() {
        let m = "short wildcard cannot be mixed with server-only tags";
        assert!(err("--any \"TNF*,Review[pt]\" sepsis").contains(m));
        assert!(err("--any \"Review[pt],TNF*\" sepsis").contains(m));
        assert!(f("--any \"TNF*,sepsis\" asthma").needs_post());
        assert_eq!(q("--all \"TNF*,Review[pt]\" asthma"), "asthma AND Review[pt]");
        assert_eq!(q("--not \"TNF*,Review[pt]\" asthma"), "asthma NOT Review[pt]");
        assert!(f("--all \"TNF*,Review[pt]\" asthma").local.contains(&"wildcard"));
        assert!(f("--not \"TNF*,Review[pt]\" asthma").needs_post());
    }

    #[test]
    fn journal_tag_matches_issn_and_grant_matches_country() {
        assert!(ok("--all \"0140-6736[ta]\""));
        assert!(ok("--all \"01406736[ta]\""));
        assert!(!ok("--all \"1234-5678[ta]\""));
        let mut a = art();
        a.grants = vec![Grant { id: "R01 HL1".into(), agency: "NHLBI NIH HHS".into(), country: "United States".into() }];
        assert!(f("--grant \"United States\"").matches(&a));
        assert!(f("--grant nhlbi").matches(&a));
        assert!(!f("--grant canada").matches(&a));
        assert!(!f("--grant \"United\" --grant zzz").matches(&a));
        a.grants[0].country = "Russia".into();
        assert!(!f("--grant us").matches(&a));
    }

    #[test]
    fn short_wildcards_under_keyword_and_journal_tags_stay_local() {
        let x = f("--all \"sta*[ot]\" asthma");
        assert!(x.local.contains(&"wildcard") && x.needs_post());
        assert_eq!(x.query("asthma"), "asthma");
        assert!(ok("--all \"sta*[ot]\""));
        assert!(!ok("--all \"zz*[ot]\""));
        assert!(!ok("--not \"sta*[ot]\""));
        assert!(ok("--all \"Lan*[ta]\""));
        assert!(!ok("--all \"Cel*[ta]\""));
        assert!(ok("--any \"zz*[ot],sta*[ot]\""));
    }

    #[test]
    fn short_wildcards_under_unevaluable_tags_bail() {
        assert_eq!(err("--all \"Smi*[au]\" asthma"), "--all: short wildcard Smi* needs >=4 chars before * under [au]");
        assert_eq!(err("--any \"IL*[mh]\" asthma"), "--any: short wildcard IL* needs >=4 chars before * under [mh]");
        assert_eq!(err("--mesh \"IL*\" asthma"), "--mesh: short wildcard IL* needs >=4 chars before * under [mh]");
        assert!(!f("--all \"Smith*[au]\" asthma").needs_post());
    }

    #[test]
    fn boolean_words_inside_wildcard_phrases_are_literal() {
        assert_eq!(q("--phrase \"head and neck cancer*\""), "\"head and neck cancer*\"[tiab]");
        assert_eq!(q("--kw \"head and neck cancer*\""), "\"head and neck cancer*\"[ot]");
        assert_eq!(q("--journal \"journal of noti* medicine\""), "\"journal of noti* medicine\"[ta]");
        let a = Article { title: "Head and Neck Cancers in adults".into(), ..Default::default() };
        assert!(f("--phrase \"head and neck cancer*\"").matches(&a));
        assert!(!f("--phrase \"head or neck cancer*\"").matches(&a));
        assert_eq!(q("--phrase \"IL* and signalling\""), "");
    }

    #[test]
    fn glob_terms_work_in_every_text_predicate() {
        assert!(ok("--ab \"atorva*in\""));
        assert!(!ok("--ab \"z*statin\""));
        assert!(ok("--ti \"stat*s\""));
        assert!(ok("--all \"stat*s,rand*zed\""));
        assert!(ok("--any \"zzz,at*statin\""));
        assert!(!ok("--not \"t*l\""));
        assert!(ok("--not \"zz*zz\""));
        assert!(ok("--tiab \"heart fail*\""));
        assert!(ok("--tiab \"he*t fai*e\""));
        assert!(!ok("--tiab \"he*t fai*x\""));
    }

    #[test]
    fn keyword_wildcards_and_clinical_study_parent() {
        let a = Article {
            kw: vec!["CRISPR".into(), "gene editing".into()],
            types: vec!["Randomized Controlled Trial".into()],
            ..Default::default()
        };
        assert!(f("--kw 'CRIS*R'").matches(&a));
        assert!(f("--kw 'gene ed*'").matches(&a));
        assert!(!f("--kw 'CRIS*X'").matches(&a));
        assert!(f("--kw CRISPR").matches(&a));
        assert!(f("-t 'Clinical Study'").matches(&a));
        assert!(!f("--not-type 'Clinical Study'").matches(&a));
    }

    #[test]
    fn apostrophes_fold_like_the_haystack() {
        let a = Article { title: "A Clinician's Guide to sepsis".into(), ..Default::default() };
        assert!(f("--phrase \"Clinician's Guide\"").matches(&a));
        assert!(f("--phrase \"Clinicians Guide\"").matches(&a));
        assert!(f("--phrase \"Clinician\u{2019}s Guide\"").matches(&a));
        assert!(f("--tiab \"Clinician's Guide\"").matches(&a));
        assert!(!f("--phrase \"Clinician's Manual\"").matches(&a));
    }

    #[test]
    fn journal_wildcards_match_title_and_iso() {
        let a = Article {
            journal: JournalInfo { title: "Nature communications".into(), iso: "Nat Commun".into(), ..Default::default() },
            ..Default::default()
        };
        assert!(f("-j 'Nature*'").matches(&a));
        assert!(f("-j 'Nat Comm*'").matches(&a));
        assert!(f("-j '*communications'").matches(&a));
        assert!(!f("-j 'Science*'").matches(&a));
        assert!(!f("-j 'Nature'").matches(&a));
    }

    #[test]
    fn relative_and_explicit_dates_intersect() {
        let y = today().y;
        let o = f("--until 2024 --last 3y").search_opts();
        assert_eq!(o.maxdate.as_deref(), Some("2024"));
        assert!(o.mindate.is_some());
        let o = f("--from 2000 --last 3y").search_opts();
        assert_ne!(o.mindate.as_deref(), Some("2000"));
        let o = f(&format!("--from {} --last 3y", y - 1)).search_opts();
        assert_eq!(o.mindate.as_deref().map(|m| &m[..4]), Some(format!("{}", y - 1).as_str()));
        let o = f("--year 2020 --until 2022").search_opts();
        assert_eq!(o.maxdate.as_deref(), Some("2020"));
        let o = f("--year 2020-2023 --until 2021").search_opts();
        assert_eq!(o.mindate.as_deref(), Some("2020"));
        assert_eq!(o.maxdate.as_deref(), Some("2021"));
        let mut a = art();
        a.date = "2025".into();
        assert!(!f("--until 2024 --last 3y").matches(&a));
        assert_eq!(err("--year 2020 --from 2022"), "date range reversed: 2022 > 2020");
    }

    #[test]
    fn phrase_server_term_keeps_supported_wildcards() {
        assert_eq!(q("--phrase \"random* trial\""), "\"random* trial\"[tiab]");
        assert_eq!(q("--phrase \"heart fail*\""), "\"heart fail*\"[tiab]");
        assert_eq!(q("--phrase \"CRIS*R Gene\""), "\"CRIS*R Gene\"[tiab]");
        assert_eq!(q("--phrase \"xxxx* OR y[sb]\""), "\"xxxx* OR ysb\"[tiab]");
        assert_eq!(q("--ab \"atorva*in\""), "atorva*in[tiab]");
        assert_eq!(q("--ti \"organi*ation\""), "organi*ation[ti]");
    }

    #[test]
    fn text_lists_trim_space_after_commas() {
        assert_eq!(q("--any \"sepsis, asthma\""), q("--any sepsis,asthma"));
        assert_eq!(q("--all \"sepsis, asthma\""), q("--all sepsis,asthma"));
        assert_eq!(q("--not \"sepsis, asthma\""), q("--not sepsis,asthma"));
        assert_eq!(q("--tiab \"sepsis, asthma\""), q("--tiab sepsis,asthma"));
        assert_eq!(q("--any \"sepsis, asthma\""), "(sepsis[tiab] OR asthma[tiab])");
        assert!(ok("--any \"zzz, at*statin\""));
    }

    #[test]
    fn infix_wildcards_in_phrases() {
        assert!(ok("--phrase \"heart fail*\""));
        assert!(ok("--phrase \"stat*s in heart\""));
        assert!(ok("--phrase \"S*ns in H*t\""));
        assert!(!ok("--phrase \"stat*x in heart\""));
        assert!(glob_hit("crispr", "cris*r"));
        assert!(glob_hit("crispr", "c*i*r"));
        assert!(!glob_hit("crispr", "cris*s"));
        assert!(!glob_hit("cr", "cr*r"));
    }

    #[test]
    fn population_flags_with_explosion_are_server_only() {
        let msg = |l: &str| f(l).server_only();
        assert_eq!(msg("--age adult"), Some("filter: --age uses MeSH explosion (server-side); use search"));
        assert_eq!(msg("--animal"), Some("filter: --species uses MeSH explosion (server-side); use search"));
        assert_eq!(msg("--species mouse"), Some("filter: --species uses MeSH explosion (server-side); use search"));
        assert_eq!(msg("--sex weird"), Some("filter: --sex value uses MeSH explosion (server-side); use search"));
        assert!(msg("--human").is_none());
        assert!(msg("--sex f").is_none());
    }

    #[test]
    fn pt_hierarchy_covers_verified_descendants() {
        assert!(pt_is("Guideline", "Review"));
        assert!(pt_is("Practice Guideline", "Review"));
        assert!(pt_is("Consensus Statement", "Review"));
        assert!(pt_is("Consensus Development Conference, NIH", "Review"));
        assert!(pt_is("Scoping Review", "Review"));
        assert!(pt_is("Randomized Controlled Trial, Veterinary", "Clinical Study"));
        assert!(pt_is("Observational Study, Veterinary", "Clinical Study"));
        assert!(!pt_is("Clinical Trial, Veterinary", "Clinical Trial"));
        assert!(!pt_is("Meta-Analysis", "Review"));
        assert!(pt_is("Network Meta-Analysis", "Meta-Analysis"));
    }

    #[test]
    fn subset_aliases_are_server_only() {
        for (alias, label) in [
            ("free full text", "free full text"),
            ("full text", "full text"),
            ("pubmed pmc", "pubmed pmc"),
            ("pubmed pmc open access", "pubmed pmc open access"),
        ] {
            let want = format!("filter: {label} is server-side; use search");
            assert_eq!(f(&format!("--cat \"{alias}\"")).server_only().map(str::to_string), Some(want.clone()), "{alias}");
            assert!(err(&format!("--where 'type=\"{alias}\"'")).contains(&want[8..]), "{alias}");
            assert!(err(&format!("--where 'type in (\"{alias}\")'")).contains(&want[8..]), "{alias}");
        }
        assert!(f("--cat \"ahead of print\"").server_only().is_none());
    }

    #[test]
    fn full_tag_names_behave_like_abbreviations() {
        for (full, short) in [("Title/Abstract", "tiab"), ("title/abstract", "tiab"), ("Title", "ti"), ("Abstract", "ab"), ("Other Term", "ot"), ("Journal", "ta"), ("MeSH Terms", "mh"), ("MeSH Major Topic", "majr"), ("Text Word", "tw"), ("All Fields", "all"), ("Author", "au")] {
            assert_eq!(canon_tag(full), short, "{full}");
            assert_eq!(canon_tag(short), short, "{short}");
        }
        for tag in ["Title/Abstract", "Title", "Other Term", "tiab"] {
            let line = format!("--all 'IL*[{tag}]' sepsis");
            let x = f(&line);
            assert!(x.needs_post(), "{line}");
            assert_eq!(x.query("sepsis"), "sepsis", "{line}");
        }
        let mut a = art();
        a.title = "IL6 signalling in sepsis".into();
        assert!(f("--all 'IL*[Title/Abstract]' sepsis").matches(&a));
        assert!(f("--all 'IL*[Title]' sepsis").matches(&a));
        a.title = "Cytokine signalling in sepsis".into();
        assert!(!f("--all 'IL*[Title/Abstract]' sepsis").matches(&a));
        assert_eq!(q("--all 'placebo[Abstract]' sepsis"), "sepsis AND placebo[tiab]");
        assert!(f("--all 'placebo[Abstract]' sepsis").needs_post());
        assert_eq!(err("--any 'IL*[MeSH Terms]' asthma"), "--any: short wildcard IL* needs >=4 chars before * under [MeSH Terms]");
        let ab_only = Article { title: "Placebo trial".into(), abs: vec![("".into(), "Nothing relevant.".into())], ..Default::default() };
        assert!(!f("--all 'placebo[Abstract]' trial").matches(&ab_only));
    }
}
