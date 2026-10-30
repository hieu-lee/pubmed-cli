use crate::model::*;
use std::collections::HashMap;
use std::sync::OnceLock;

static DATA: &[u8] = include_bytes!("../data/sjr.bin");

#[derive(Clone, Copy)]
pub struct Entry {
    pub issns: &'static str,
    pub title: &'static str,
    pub sjr: f32,
    pub h: u32,
    cats: &'static str,
}

pub type Hit = (Entry, (&'static str, u8));

struct Db {
    n: usize,
    cats: Vec<&'static str>,
    offs: &'static [u8],
    pool: &'static [u8],
    issn: &'static [[u8; 8]],
    names: &'static [[u8; 12]],
    abbr: &'static [[u8; 4]],
}

const STOP: &[&str] = &["of", "the", "and", "for", "in", "on", "a", "an", "de", "la", "le", "et", "und", "der", "die", "das", "del", "y"];

fn le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn db() -> &'static Db {
    static DB: OnceLock<Db> = OnceLock::new();
    DB.get_or_init(|| {
        let h = |i: usize| le(DATA, i * 4) as usize;
        let (n, nissn, nnames, nabbr, lcats, lpool) = (h(1), h(2), h(3), h(4), h(5), h(6));
        let (cats, rest) = DATA[32..].split_at(lcats);
        let (offs, rest) = rest.split_at((n + 1) * 4);
        let (pool, rest) = rest.split_at(lpool);
        let (issn, rest) = rest.split_at(nissn * 8);
        let (names, rest) = rest.split_at(nnames * 12);
        let abbr = &rest[..nabbr * 4];
        Db {
            n,
            cats: std::str::from_utf8(cats).expect("sjr cats").split('|').collect(),
            offs,
            pool,
            issn: issn.as_chunks().0,
            names: names.as_chunks().0,
            abbr: abbr.as_chunks().0,
        }
    })
}

impl Db {
    fn line(&self, i: usize) -> &'static str {
        std::str::from_utf8(&self.pool[le(self.offs, i * 4) as usize..le(self.offs, i * 4 + 4) as usize - 1]).expect("sjr entry")
    }

    fn entry(&self, i: usize) -> Entry {
        let mut f = self.line(i).splitn(6, '\t');
        let mut next = || f.next().unwrap_or("");
        let issns = next();
        let title = next();
        next();
        let sjr = next().parse().unwrap_or(0.0);
        let h = next().parse().unwrap_or(0);
        Entry { issns, title, sjr, h, cats: next() }
    }

    fn by_issn(&self, s: &str) -> Option<usize> {
        let k = issn_key(s)?;
        let i = self.issn.binary_search_by_key(&k, |c| le(c, 0)).ok()?;
        Some(le(&self.issn[i], 4) as usize)
    }

    fn by_name(&self, s: &str) -> Option<usize> {
        let find = |s: &str| {
            let h = fnv(norm(s).as_bytes());
            let i = self.names.binary_search_by_key(&h, |c| u64::from_le_bytes(c[..8].try_into().unwrap())).ok()?;
            Some(le(&self.names[i], 8) as usize)
        };
        find(s).or_else(|| find(strip_paren(s)))
    }

    fn by_abbr(&self, iso: &str) -> Vec<usize> {
        let toks: Vec<&str> = tokens(iso).collect();
        let Some(f) = toks.first() else { return Vec::new() };
        if toks.len() < 2 || toks.len() > 255 {
            return Vec::new();
        }
        let key = (u32::from(f.as_bytes()[0].to_ascii_lowercase()) << 8 | toks.len() as u32) << 16;
        let val = |c: &[u8; 4]| u32::from_le_bytes(*c);
        let lo = self.abbr.partition_point(|c| val(c) < key);
        self.abbr[lo..]
            .iter()
            .map(val)
            .take_while(|v| v & 0xFFFF_0000 == key)
            .map(|v| (v & 0xFFFF) as usize)
            .filter(|&i| tokens(self.entry(i).title).zip(&toks).all(|(w, a)| prefix_ci(a, w)))
            .collect()
    }

    fn find(&self, a: &Article) -> Option<usize> {
        let j = &a.journal;
        [&j.issn, &j.eissn]
            .into_iter()
            .find_map(|s| self.by_issn(s))
            .or_else(|| [&j.title, &j.iso].into_iter().filter(|s| !s.is_empty()).find_map(|s| self.by_name(s)))
            .or_else(|| match self.by_abbr(&j.iso)[..] {
                [i] => Some(i),
                _ => None,
            })
    }
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &c| (h ^ u64::from(c)).wrapping_mul(0x0100_0000_01b3))
}

fn issn_key(s: &str) -> Option<u32> {
    let (mut n, mut digits) = (0u32, 0);
    let mut check = 0;
    for c in s.bytes().filter(u8::is_ascii_alphanumeric) {
        let d = match c.to_ascii_uppercase() {
            c @ b'0'..=b'9' => u32::from(c - b'0'),
            b'X' if digits == 7 => 10,
            _ => return None,
        };
        match digits {
            0..7 => n = n * 10 + d,
            7 => check = d,
            _ => return None,
        }
        digits += 1;
    }
    (digits == 8).then(|| n * 11 + check)
}

fn strip_paren(s: &str) -> &str {
    let t = s.trim_end();
    if t.ends_with(')')
        && let Some(i) = t.rfind('(')
        && i > 0
    {
        return t[..i].trim_end();
    }
    t
}

fn norm(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    norm_into(&mut o, s);
    o
}

fn norm_into(o: &mut String, s: &str) {
    o.clear();
    let s = s.trim();
    let s = if s.len() > 4 && s.is_char_boundary(4) && s[..4].eq_ignore_ascii_case("the ") { &s[4..] } else { s };
    for c in s.chars() {
        if c == '&' {
            o.push_str("and");
        } else if c.is_alphanumeric() {
            o.extend(c.to_lowercase());
        }
    }
}

fn tokens(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty() && !STOP.iter().any(|s| t.eq_ignore_ascii_case(s)))
}

fn prefix_ci(abbr: &str, word: &str) -> bool {
    let mut w = word.chars().flat_map(char::to_lowercase);
    abbr.chars().flat_map(char::to_lowercase).all(|a| w.next() == Some(a))
}

impl Entry {
    pub fn cats(&self) -> impl Iterator<Item = (&'static str, u8)> + use<> {
        let names = &db().cats;
        self.cats.split(',').filter_map(move |c| {
            let (i, q) = c.split_once(':')?;
            Some((*names.get(i.parse::<usize>().ok()?)?, q.parse().ok()?))
        })
    }

    pub fn best(&self) -> (&'static str, u8) {
        self.cats().min_by_key(|c| c.1).unwrap_or(("", 0))
    }

    pub fn print_issn(&self) -> &'static str {
        self.issns.split(',').next().unwrap_or("")
    }

    fn rank_of(&self, (area, q): (&str, u8)) -> JournalRank {
        JournalRank { q, sjr: self.sjr, h: self.h, area: area.to_string() }
    }

    pub fn rank(&self) -> JournalRank {
        self.rank_of(self.best())
    }

    fn in_area(&self, area: &Area) -> Option<(&'static str, u8)> {
        self.cats().filter(|(c, _)| area.matches(c)).min_by_key(|c| c.1)
    }
}

struct Area {
    want: String,
    exact: bool,
}

impl Area {
    fn new(area: &str) -> Area {
        let want = area.trim().to_lowercase();
        let exact = db().cats.iter().any(|c| c.eq_ignore_ascii_case(&want));
        Area { want, exact }
    }

    fn matches(&self, cat: &str) -> bool {
        if self.exact { cat.eq_ignore_ascii_case(&self.want) } else { cat.to_lowercase().contains(&self.want) }
    }
}

pub fn rank(a: &Article) -> Option<JournalRank> {
    let db = db();
    db.find(a).map(|i| db.entry(i).rank())
}

pub fn rank_in_area(a: &Article, area: &str) -> Option<JournalRank> {
    let db = db();
    let e = db.entry(db.find(a)?);
    e.in_area(&Area::new(area)).map(|c| e.rank_of(c))
}

pub fn areas() -> Vec<&'static str> {
    let db = db();
    let mut n = vec![0u32; db.cats.len()];
    for i in 0..db.n {
        for c in db.line(i).rsplit('\t').next().unwrap_or("").split(',') {
            if let Some(i) = c.split(':').next().and_then(|i| i.parse::<usize>().ok()) {
                n[i] += 1;
            }
        }
    }
    let mut v: Vec<usize> = (0..n.len()).collect();
    v.sort_by_key(|&i| std::cmp::Reverse(n[i]));
    v.into_iter().map(|i| db.cats[i]).collect()
}

fn candidates(query: &str) -> Vec<Entry> {
    let db = db();
    let q = query.trim();
    if q.is_empty() {
        return (0..db.n).map(|i| db.entry(i)).collect();
    }
    if issn_key(q).is_some() {
        return db.by_issn(q).map(|i| vec![db.entry(i)]).unwrap_or_default();
    }
    let nq = norm(q);
    if nq.is_empty() {
        return Vec::new();
    }
    let words: Vec<String> = q.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(norm).collect();
    let mut tier: HashMap<usize, u8> = HashMap::new();
    let mut put = |i: usize, t: u8| {
        let e = tier.entry(i).or_insert(t);
        *e = (*e).min(t);
    };
    let mut buf = String::new();
    let mut test = |i: usize, k: &str| {
        if k == nq {
            put(i, 0);
        } else if k.starts_with(&nq) {
            put(i, 2);
        } else if words.iter().all(|w| k.contains(w.as_str())) {
            put(i, 3);
        }
    };
    for i in 0..db.n {
        let mut f = db.line(i).splitn(4, '\t').skip(1);
        let title = f.next().unwrap_or("");
        norm_into(&mut buf, title);
        test(i, &buf);
        if title.ends_with(')') {
            norm_into(&mut buf, strip_paren(title));
            test(i, &buf);
        }
        for al in f.next().unwrap_or("").split('|').filter(|s| !s.is_empty()) {
            test(i, al);
        }
    }
    for i in db.by_abbr(q) {
        put(i, 1);
    }
    let mut v: Vec<(u8, usize)> = tier.into_iter().map(|(i, t)| (t, i)).collect();
    v.sort_unstable();
    v.into_iter().map(|(_, i)| db.entry(i)).collect()
}

pub fn search(query: &str, area: Option<&str>, quartiles: &[u8], limit: usize) -> Vec<Hit> {
    let area = area.map(Area::new);
    candidates(query)
        .into_iter()
        .filter_map(|e| Some((e, area.as_ref().map_or_else(|| Some(e.best()), |a| e.in_area(a))?)))
        .filter(|(_, (_, q))| quartiles.is_empty() || quartiles.contains(q))
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(query: &str, limit: usize) -> Vec<Entry> {
        search(query, None, &[], limit).into_iter().map(|h| h.0).collect()
    }

    fn art(issn: &str, eissn: &str, title: &str, iso: &str) -> Article {
        Article {
            journal: JournalInfo { issn: issn.into(), eissn: eissn.into(), title: title.into(), iso: iso.into(), ..Default::default() },
            ..Default::default()
        }
    }

    fn q(title: &str, iso: &str) -> Option<u8> {
        rank(&art("", "", title, iso)).map(|r| r.q)
    }

    #[test]
    fn issn_lookup_and_normalisation() {
        let r = rank(&art("0028-4793", "", "", "")).unwrap();
        assert_eq!((r.q, r.area.as_str()), (1, "Medicine (miscellaneous)"));
        assert!(r.sjr > 10.0 && r.h > 1000);
        assert_eq!(rank(&art("", "1533-4406", "", "")).unwrap().q, 1);
        assert_eq!(rank(&art("1474-547x", "", "", "")).unwrap().q, 1);
        assert!(rank(&art("0000-0000", "", "", "")).is_none());
    }

    #[test]
    fn well_known_journals() {
        let cases: &[(&str, &str, u8)] = &[
            ("The New England journal of medicine", "N Engl J Med", 1),
            ("Lancet (London, England)", "Lancet", 1),
            ("JAMA", "JAMA", 1),
            ("BMJ (Clinical research ed.)", "BMJ", 1),
            ("Nature", "Nature", 1),
            ("Science (New York, N.Y.)", "Science", 1),
            ("Cell", "Cell", 1),
            ("PloS one", "PLoS One", 1),
            ("Scientific reports", "Sci Rep", 1),
            ("Journal of clinical oncology : official journal of the American Society of Clinical Oncology", "J Clin Oncol", 1),
            ("Proceedings of the National Academy of Sciences of the United States of America", "Proc Natl Acad Sci U S A", 1),
            ("Nature communications", "Nat Commun", 1),
            ("Annals of internal medicine", "Ann Intern Med", 1),
            ("Journal of the American College of Cardiology", "J Am Coll Cardiol", 1),
            ("Blood", "Blood", 1),
            ("Gastroenterology", "Gastroenterology", 1),
            ("Nucleic acids research", "Nucleic Acids Res", 1),
            ("BMJ open", "BMJ Open", 1),
            ("The Journal of biological chemistry", "J Biol Chem", 1),
            ("Frontiers in immunology", "Front Immunol", 1),
            ("International journal of molecular sciences", "Int J Mol Sci", 1),
            ("Journal of medical Internet research", "J Med Internet Res", 1),
            ("Medicine", "Medicine (Baltimore)", 2),
            ("Cochrane Database of Systematic Reviews", "Cochrane Database Syst Rev", 1),
            ("Clinical infectious diseases", "Clin Infect Dis", 1),
            ("Circulation", "Circulation", 1),
            ("Journal of neurosurgery", "J Neurosurg", 1),
            ("Pediatrics", "Pediatrics", 1),
            ("Annals of the New York Academy of Sciences", "Ann N Y Acad Sci", 1),
            ("Zhonghua yi xue za zhi", "Zhonghua Yi Xue Za Zhi", 3),
        ];
        let bad: Vec<_> = cases.iter().filter(|(t, i, want)| q(t, i) != Some(*want)).map(|(t, i, w)| (t, i, w, q(t, i))).collect();
        assert!(bad.is_empty(), "{bad:?}");
    }

    #[test]
    fn iso_abbreviation_matcher() {
        let hit = |iso: &str| db().by_abbr(iso).into_iter().map(|i| db().entry(i).title).collect::<Vec<_>>();
        assert_eq!(hit("N Engl J Med"), ["New England Journal of Medicine"]);
        assert_eq!(hit("J Am Coll Cardiol"), ["Journal of the American College of Cardiology"]);
        assert!(hit("Zzz Qqq").is_empty());
        assert!(hit("J").is_empty());
    }

    #[test]
    fn ampersand_and_parenthetical() {
        assert_eq!(norm("The Lancet"), "lancet");
        assert_eq!(norm("Obstetrics & Gynecology"), norm("Obstetrics and Gynecology"));
        assert_eq!(strip_paren("Lancet (London, England)"), "Lancet");
        assert_eq!(strip_paren("Plain"), "Plain");
        assert_eq!(strip_paren("(odd)"), "(odd)");
    }

    #[test]
    fn area_filter() {
        let a = art("0028-4793", "", "", "");
        assert_eq!(rank_in_area(&a, "medicine (misc").unwrap().q, 1);
        assert!(rank_in_area(&a, "Oncology").is_none());
        let a = art("0732-183X", "", "", "");
        let r = rank_in_area(&a, "oncology").unwrap();
        assert_eq!((r.q, r.area.as_str()), (1, "Oncology"));
        assert!(areas().contains(&"Oncology"));
    }

    #[test]
    fn fuzzy_lookup() {
        assert_eq!(lookup("0028-4793", 5).len(), 1);
        assert_eq!(lookup("new england journal of medicine", 3)[0].title, "New England Journal of Medicine");
        assert_eq!(lookup("N Engl J Med", 3)[0].title, "New England Journal of Medicine");
        assert!(lookup("lancet", 5).iter().any(|e| e.title == "The Lancet"));
        assert!(lookup("zzzzqqqq", 5).is_empty());
        let top = search("", Some("Oncology"), &[1], 3);
        assert_eq!(top.len(), 3);
        assert!(top.windows(2).all(|w| w[0].0.sjr >= w[1].0.sjr));
    }

    #[test]
    fn embedded_tables_agree_with_runtime_normalisation() {
        let db = db();
        let (mut miss_name, mut miss_issn) = (0, 0);
        for i in 0..db.n {
            let e = db.entry(i);
            if db.by_name(e.title).is_none() {
                miss_name += 1;
            }
            if !e.issns.is_empty() && db.by_issn(e.print_issn()).is_none_or(|j| !db.entry(j).issns.contains(e.print_issn())) {
                miss_issn += 1;
            }
        }
        assert_eq!((miss_name, miss_issn), (0, 0));
        assert!(db.n > 30_000);
    }
}
