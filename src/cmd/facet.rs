use crate::args::Args;
use crate::enrich;
use crate::filter::Filters;
use crate::http::Ctx;
use crate::model::*;
use crate::ops;
use anyhow::{Result, bail};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

pub const HELP: &str = "\
facet FIELDS [-n 10] [--min N] [--other] [--asc|--desc]: group-by counts over the piped set (or PMIDs, or an inline query).
FIELDS: comma list of journal year month type q author first last mesh major kw country aff lang status grant cites rcr (or --by FIELDS).
Output: one field = `value<TAB>count` rows; several = blocks headed `# field`. Sorted by count desc, ties by value; --asc/--desc sort the shown rows by value instead (year trends).
-n top N per field (0=all); --min N drops counts below N; --other adds an (other) row = articles with any value not shown (place --other last or write --other=1).
Header: n=records scanned, missing=field:records with no value, total=server hits.
A piped `search` holds only its -n ids: to facet bigger sets write `pm 'search ... -n 500 | facet journal'`.
Inline: with no pipe and no PMIDs, other words and filters run as a search of --scan N (500) records: pm facet journal,year sepsis -t rct --last 5y
Piped/PMID input plus filter flags (-y, -t, --where ...) is filtered first.";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Journal,
    Year,
    Month,
    Type,
    Q,
    Author,
    First,
    Last,
    Mesh,
    Major,
    Kw,
    Country,
    Aff,
    Lang,
    Status,
    Grant,
    Cites,
    Rcr,
}

const FIELDS: &[(Field, &str, &[&str])] = &[
    (Field::Journal, "journal", &["j", "jt"]),
    (Field::Year, "year", &["y"]),
    (Field::Month, "month", &["date"]),
    (Field::Type, "type", &["types", "pt"]),
    (Field::Q, "q", &["quartile"]),
    (Field::Author, "author", &["authors", "au"]),
    (Field::First, "first", &["first-author"]),
    (Field::Last, "last", &["last-author"]),
    (Field::Mesh, "mesh", &[]),
    (Field::Major, "major", &[]),
    (Field::Kw, "kw", &["keyword", "keywords"]),
    (Field::Country, "country", &["cn"]),
    (Field::Aff, "aff", &[]),
    (Field::Lang, "lang", &[]),
    (Field::Status, "status", &[]),
    (Field::Grant, "grant", &["grants"]),
    (Field::Cites, "cites", &[]),
    (Field::Rcr, "rcr", &[]),
];

const CITE_BUCKETS: &[&str] = &["0", "1-9", "10-49", "50-99", "100+"];
const RCR_BUCKETS: &[&str] = &["<0.5", "0.5-1", "1-2", "2-5", "5+"];
const NONE: &str = "-";
const OTHER: &str = "(other)";
const SCAN_DEFAULT: usize = 500;

impl Field {
    fn parse(s: &str) -> Result<Field> {
        let s = s.trim().to_ascii_lowercase();
        match FIELDS.iter().find(|(_, n, al)| *n == s || al.contains(&s.as_str())) {
            Some((f, _, _)) => Ok(*f),
            None => {
                let names: Vec<&str> = FIELDS.iter().map(|f| f.1).collect();
                bail!("facet: unknown field '{s}' ({})", names.join("|"))
            }
        }
    }

    fn name(self) -> &'static str {
        FIELDS.iter().find(|f| f.0 == self).map_or("", |f| f.1)
    }

    fn need(self) -> Need {
        Need { rank: self == Field::Q, metrics: matches!(self, Field::Cites | Field::Rcr), expand: false }
    }

    fn buckets(self) -> Option<&'static [&'static str]> {
        match self {
            Field::Cites => Some(CITE_BUCKETS),
            Field::Rcr => Some(RCR_BUCKETS),
            _ => None,
        }
    }

    fn values(self, a: &Article, out: &mut Vec<String>) {
        match self {
            Field::Journal => {
                let j = if a.journal.iso.is_empty() { &a.journal.title } else { &a.journal.iso };
                push(out, j);
            }
            Field::Year => {
                if a.year > 0 {
                    out.push(a.year.to_string());
                }
            }
            Field::Month => {
                if let Some(m) = a.date.get(..7).filter(|m| m.as_bytes()[4] == b'-') {
                    out.push(m.to_string());
                }
            }
            Field::Type => a.types.iter().for_each(|t| push(out, t)),
            Field::Q => out.push(match &a.jr {
                Some(j) if (1..=4).contains(&j.q) => format!("Q{}", j.q),
                _ => NONE.to_string(),
            }),
            Field::Author => a.authors.iter().for_each(|x| push(out, &x.name)),
            Field::First => crate::fields::first_author(a).iter().for_each(|x| push(out, &x.name)),
            Field::Last => crate::fields::last_author(a).iter().for_each(|x| push(out, &x.name)),
            Field::Mesh => a.mesh.iter().for_each(|m| push(out, &m.name)),
            Field::Major => a.mesh.iter().filter(|m| m.major).for_each(|m| push(out, &m.name)),
            Field::Kw => a.kw.iter().for_each(|k| push(out, k)),
            Field::Country => {
                for aff in &a.affs {
                    for piece in aff.split(';') {
                        if let Some(c) = country(piece) {
                            out.push(c.to_string());
                        }
                    }
                }
            }
            Field::Aff => {
                let idx = a.authors.first().and_then(|u| u.aff.first()).map_or(0, |&i| i as usize);
                if let Some(aff) = a.affs.get(idx).or(a.affs.first()) {
                    push(out, &institution(aff));
                }
            }
            Field::Lang => a.langs.iter().for_each(|l| push(out, l)),
            Field::Status => push(out, &a.status),
            Field::Grant => a.grants.iter().for_each(|g| push(out, &g.agency)),
            Field::Cites => {
                if let Some(m) = &a.m {
                    out.push(cites_bucket(m.cites).to_string());
                }
            }
            Field::Rcr => out.push(rcr_bucket(a.m.as_ref().and_then(|m| m.rcr)).to_string()),
        }
    }

    fn cmp(self, a: &str, b: &str) -> Ordering {
        match self.buckets() {
            Some(bk) => {
                let pos = |v: &str| bk.iter().position(|x| *x == v).unwrap_or(bk.len());
                pos(a).cmp(&pos(b))
            }
            None => natural_cmp(a, b),
        }
    }
}

fn push(out: &mut Vec<String>, s: &str) {
    let s = s.trim();
    if !s.is_empty() {
        out.push(s.to_string());
    }
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let split = |s: &str| {
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        (s[..end].parse::<u64>().ok().filter(|_| end > 0), end)
    };
    match (split(a), split(b)) {
        ((Some(x), i), (Some(y), j)) => x.cmp(&y).then_with(|| a[i..].cmp(&b[j..])),
        _ => a.cmp(b),
    }
}

fn cites_bucket(c: u32) -> &'static str {
    match c {
        0 => CITE_BUCKETS[0],
        1..=9 => CITE_BUCKETS[1],
        10..=49 => CITE_BUCKETS[2],
        50..=99 => CITE_BUCKETS[3],
        _ => CITE_BUCKETS[4],
    }
}

fn rcr_bucket(r: Option<f32>) -> &'static str {
    match r {
        None => NONE,
        Some(r) if r < 0.5 => RCR_BUCKETS[0],
        Some(r) if r < 1.0 => RCR_BUCKETS[1],
        Some(r) if r < 2.0 => RCR_BUCKETS[2],
        Some(r) if r < 5.0 => RCR_BUCKETS[3],
        Some(_) => RCR_BUCKETS[4],
    }
}

const COUNTRIES: &[(&str, &[&str])] = &[
    ("USA", &["usa", "us", "united states", "united states of america", "america"]),
    ("UK", &["uk", "united kingdom", "england", "scotland", "wales", "northern ireland", "great britain", "britain"]),
    ("China", &["china", "prc", "mainland china"]),
    ("Hong Kong", &["hong kong"]),
    ("Taiwan", &["taiwan"]),
    ("Japan", &["japan"]),
    ("South Korea", &["south korea", "korea", "republic of korea"]),
    ("North Korea", &["north korea"]),
    ("India", &["india"]),
    ("Germany", &["germany", "deutschland"]),
    ("France", &["france"]),
    ("Italy", &["italy", "italia"]),
    ("Spain", &["spain", "espana"]),
    ("Portugal", &["portugal"]),
    ("Netherlands", &["netherlands", "the netherlands", "holland"]),
    ("Belgium", &["belgium"]),
    ("Switzerland", &["switzerland"]),
    ("Austria", &["austria"]),
    ("Sweden", &["sweden"]),
    ("Norway", &["norway"]),
    ("Denmark", &["denmark"]),
    ("Finland", &["finland"]),
    ("Iceland", &["iceland"]),
    ("Ireland", &["ireland"]),
    ("Poland", &["poland"]),
    ("Czechia", &["czech republic", "czechia"]),
    ("Slovakia", &["slovakia"]),
    ("Hungary", &["hungary"]),
    ("Romania", &["romania"]),
    ("Bulgaria", &["bulgaria"]),
    ("Greece", &["greece"]),
    ("Serbia", &["serbia"]),
    ("Croatia", &["croatia"]),
    ("Slovenia", &["slovenia"]),
    ("Bosnia", &["bosnia and herzegovina", "bosnia"]),
    ("Ukraine", &["ukraine"]),
    ("Russia", &["russia", "russian federation"]),
    ("Turkey", &["turkey", "turkiye", "tuerkiye"]),
    ("Israel", &["israel"]),
    ("Iran", &["iran"]),
    ("Iraq", &["iraq"]),
    ("Saudi Arabia", &["saudi arabia", "ksa"]),
    ("UAE", &["uae", "united arab emirates"]),
    ("Qatar", &["qatar"]),
    ("Kuwait", &["kuwait"]),
    ("Lebanon", &["lebanon"]),
    ("Jordan", &["jordan"]),
    ("Egypt", &["egypt"]),
    ("Pakistan", &["pakistan"]),
    ("Bangladesh", &["bangladesh"]),
    ("Sri Lanka", &["sri lanka"]),
    ("Nepal", &["nepal"]),
    ("Thailand", &["thailand"]),
    ("Vietnam", &["vietnam", "viet nam"]),
    ("Malaysia", &["malaysia"]),
    ("Singapore", &["singapore"]),
    ("Indonesia", &["indonesia"]),
    ("Philippines", &["philippines"]),
    ("Australia", &["australia"]),
    ("New Zealand", &["new zealand"]),
    ("Canada", &["canada"]),
    ("Mexico", &["mexico"]),
    ("Brazil", &["brazil", "brasil"]),
    ("Argentina", &["argentina"]),
    ("Chile", &["chile"]),
    ("Colombia", &["colombia"]),
    ("Peru", &["peru"]),
    ("Venezuela", &["venezuela"]),
    ("Cuba", &["cuba"]),
    ("Puerto Rico", &["puerto rico"]),
    ("South Africa", &["south africa"]),
    ("Nigeria", &["nigeria"]),
    ("Ghana", &["ghana"]),
    ("Kenya", &["kenya"]),
    ("Uganda", &["uganda"]),
    ("Tanzania", &["tanzania"]),
    ("Ethiopia", &["ethiopia"]),
    ("Morocco", &["morocco"]),
    ("Tunisia", &["tunisia"]),
    ("Algeria", &["algeria"]),
    ("Luxembourg", &["luxembourg"]),
    ("Estonia", &["estonia"]),
    ("Latvia", &["latvia"]),
    ("Lithuania", &["lithuania"]),
    ("Cyprus", &["cyprus"]),
    ("Malta", &["malta"]),
];

const US_STATES: &[&str] = &[
    "AL", "AK", "AZ", "AR", "CA", "CO", "CT", "DE", "FL", "GA", "HI", "ID", "IL", "IN", "IA", "KS", "KY", "LA", "ME",
    "MD", "MA", "MI", "MN", "MS", "MO", "MT", "NE", "NV", "NH", "NJ", "NM", "NY", "NC", "ND", "OH", "OK", "OR", "PA",
    "RI", "SC", "SD", "TN", "TX", "UT", "VT", "VA", "WA", "WV", "WI", "WY", "DC",
];

const INSTITUTION: &[&str] = &[
    "univ", "instit", "hospital", "college", "center", "centre", "academy", "clinic", "laborator", "foundation",
    "polytechnic", "klinikum", "ecole", "hôpital", "hopital", "charit", "nhs", "ministry", "school",
];

const AFF_MAX: usize = 60;

fn country_map() -> &'static HashMap<&'static str, &'static str> {
    static M: OnceLock<HashMap<&str, &str>> = OnceLock::new();
    M.get_or_init(|| COUNTRIES.iter().flat_map(|(c, al)| al.iter().map(move |a| (*a, *c))).collect())
}

fn strip_contact(aff: &str) -> &str {
    let lo = aff.to_ascii_lowercase();
    let cut = ["electronic address", "e-mail", "email"].iter().filter_map(|k| lo.find(k)).min();
    cut.map_or(aff, |i| &aff[..i])
}

fn segments(aff: &str) -> Vec<String> {
    strip_contact(aff)
        .split(',')
        .map(|s| {
            let toks: Vec<&str> = s.split_whitespace().filter(|t| !t.contains('@')).collect();
            toks.join(" ").trim_matches(|c: char| c == '.' || c == ';' || c.is_whitespace()).to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn has_inst(w: &str) -> bool {
    let w = w.to_lowercase();
    INSTITUTION.iter().any(|k| w.contains(k))
}

pub fn country(aff: &str) -> Option<&'static str> {
    let segs = segments(aff);
    let last = segs.last()?;
    let toks: Vec<&str> = last.split_whitespace().filter(|t| !t.chars().any(|c| c.is_ascii_digit())).collect();
    if let [t] = toks[..] {
        let t = t.trim_matches('.');
        if US_STATES.contains(&t) {
            return Some("USA");
        }
    }
    let words: Vec<String> = toks
        .iter()
        .map(|t| t.chars().filter(|c| *c != '.').collect::<String>().to_lowercase())
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let map = country_map();
    (1..=words.len().min(3)).rev().find_map(|k| {
        let (pre, suf) = words.split_at(words.len() - k);
        let c = map.get(suf.join(" ").as_str())?;
        (!pre.iter().any(|w| has_inst(w))).then_some(*c)
    })
}

pub fn institution(aff: &str) -> String {
    let segs = segments(aff);
    let pick = segs.iter().find(|s| has_inst(s) && !is_unit(s)).or(segs.first());
    let s = pick.map_or("", String::as_str);
    match s.char_indices().nth(AFF_MAX) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn is_unit(s: &str) -> bool {
    let l = s.to_lowercase();
    l.starts_with("school of") && !["univ", "hospital", "instit", "college"].iter().any(|k| l.contains(k))
}

struct Opts {
    top: usize,
    min: usize,
    other: bool,
    order: Option<bool>,
}

fn count_field(f: Field, per: &[Vec<String>], o: &Opts) -> (Vec<(String, usize)>, usize) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for v in per.iter().flatten() {
        *counts.entry(v).or_default() += 1;
    }
    let mut rows: Vec<(&str, usize)> = counts.into_iter().filter(|(_, n)| *n >= o.min.max(1)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| f.cmp(a.0, b.0)));
    if o.top > 0 {
        rows.truncate(o.top);
    }
    if let Some(asc) = o.order {
        rows.sort_by(|a, b| if asc { f.cmp(a.0, b.0) } else { f.cmp(b.0, a.0) });
    }
    let shown: HashSet<&str> = rows.iter().map(|r| r.0).collect();
    let rest = per.iter().filter(|vs| vs.iter().any(|v| !shown.contains(v.as_str()))).count();
    (rows.into_iter().map(|(v, n)| (v.to_string(), n)).collect(), rest)
}

fn build(arts: &[Article], fields: &[Field], o: &Opts) -> Set {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut missing = Vec::new();
    let mut buf = Vec::new();
    for &f in fields {
        let per: Vec<Vec<String>> = arts
            .iter()
            .map(|a| {
                buf.clear();
                f.values(a, &mut buf);
                buf.sort();
                buf.dedup();
                buf.clone()
            })
            .collect();
        let none = per.iter().filter(|v| v.is_empty()).count();
        if none > 0 {
            missing.push(format!("{}:{none}", f.name()));
        }
        let (counted, rest) = count_field(f, &per, o);
        if fields.len() > 1 {
            rows.push(vec![format!("# {}", f.name())]);
        }
        rows.extend(counted.into_iter().map(|(v, n)| vec![v, n.to_string()]));
        if o.other && rest > 0 {
            rows.push(vec![OTHER.to_string(), rest.to_string()]);
        }
    }
    let head = if let [f] = fields { f.name() } else { "value" };
    let mut set = Set::table(&[head, "n"], rows);
    set.notes.push(format!("n={}", arts.len()));
    if !missing.is_empty() {
        set.notes.push(format!("missing={}", missing.join(",")));
    }
    set
}

fn looks_like_id(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    t.bytes().all(|b| b.is_ascii_digit())
        || l.strip_prefix("doi:").is_some_and(|r| !r.is_empty())
        || l.strip_prefix("pmid:").is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
        || l.strip_prefix("pmc").is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
        || l.strip_prefix("pmc:").is_some_and(|r| !r.is_empty())
        || l.starts_with("10.") && l.contains('/')
        || l.starts_with("http") && l.contains("doi.org/")
}

fn parse_fields(a: &Args) -> Result<(Vec<Field>, &[String])> {
    let (raw, rest) = match a.get("by") {
        Some(b) => (b.to_string(), &a.pos[..]),
        None => match a.pos.split_first() {
            Some((f, rest)) => (f.clone(), rest),
            None => bail!("facet: need FIELDS, e.g. facet journal,year -n 10 (pm help facet)"),
        },
    };
    let mut fields: Vec<Field> = Vec::new();
    for p in raw.split(',').filter(|p| !p.trim().is_empty()) {
        let f = Field::parse(p)?;
        if !fields.contains(&f) {
            fields.push(f);
        }
    }
    if fields.is_empty() {
        bail!("facet: need FIELDS, e.g. facet journal,year -n 10 (pm help facet)");
    }
    Ok((fields, rest))
}

pub async fn run(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Set> {
    let (fields, rest) = parse_fields(a)?;
    let filters = Filters::from_args(a)?;
    let mut searched = false;
    let mut set = if !rest.is_empty() && rest.iter().all(|t| looks_like_id(t)) {
        ops::input_set(ctx, rest, None).await?
    } else if input.is_some() {
        if let Some(t) = rest.first() {
            bail!("facet: '{t}' is not an id; with piped input put the query in search");
        }
        ops::input_set(ctx, &[], input).await?
    } else if !rest.is_empty() || !filters.is_empty() || filters.server_only().is_some() {
        let mut b = a.clone();
        b.pos = rest.to_vec();
        b.opts.retain(|(k, _)| !matches!(k.as_str(), "limit" | "skip" | "offset"));
        b.opts.push(("limit".into(), a.num::<usize>("scan")?.unwrap_or(SCAN_DEFAULT).to_string()));
        searched = true;
        ops::search(ctx, &b).await?
    } else {
        bail!("facet: no input: pipe a search, give PMIDs, or add a query (pm help facet)");
    };
    if !searched {
        filters.reject_server_only()?;
        if !filters.is_empty() {
            set = ops::apply_filters(ctx, set, &filters).await?;
        }
    }
    ops::ensure_loaded(ctx, &mut set).await?;
    let need = fields.iter().fold(Need::default(), |n, f| n.or(f.need()));
    if need != Need::default() && !set.arts.is_empty() {
        enrich::apply(ctx, &mut set.arts, need).await?;
    }
    let order = if a.flag("asc") {
        Some(true)
    } else if a.flag("desc") {
        Some(false)
    } else {
        None
    };
    let o = Opts {
        top: a.num::<usize>("limit")?.unwrap_or(10),
        min: a.num::<usize>("min")?.unwrap_or(0),
        other: a.flag("other"),
        order,
    };
    let mut out = build(&set.arts, &fields, &o);
    out.total = set.total;
    out.notes.extend(set.notes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pmc_prefix_needs_digits_to_be_an_id() {
        for id in ["PMC123456", "pmc:PMC1", "pmid:123", "doi:10.1/x", "31452104", "10.1056/NEJMoa1"] {
            assert!(looks_like_id(id), "{id}");
        }
        for q in ["PMCH", "PMCA", "pmc", "pmid:abc", "doi:", "sepsis"] {
            assert!(!looks_like_id(q), "{q}");
        }
    }

    fn art(journal: &str, year: u16, cites: u32) -> Article {
        Article {
            journal: JournalInfo { iso: journal.into(), ..Default::default() },
            year,
            date: format!("{year}-03-01"),
            m: Some(Metrics { cites, ..Default::default() }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn server_only_filters_are_rejected_on_piped_input() {
        let ctx = crate::http::Ctx::new(crate::http::Config::default()).unwrap();
        for line in ["year --free", "year --since 2026 --datetype mdat", "year --oa"] {
            let toks: Vec<String> = line.split_whitespace().map(String::from).collect();
            let a = Args::parse(&toks).unwrap();
            let input = Some(Set::from_arts(vec![art("J", 2020, 1)]));
            let e = run(&ctx, &a, input).await.unwrap_err().to_string();
            assert!(e.contains("server-side"), "{line}: {e}");
        }
    }

    fn opts() -> Opts {
        Opts { top: 10, min: 0, other: false, order: None }
    }

    fn cells(s: &Set) -> Vec<String> {
        s.table.as_ref().unwrap().rows.iter().map(|r| r.join("\t")).collect()
    }

    #[test]
    fn countries() {
        assert_eq!(country("Dept of X, Harvard Univ, Boston, MA 02115, USA."), Some("USA"));
        assert_eq!(country("Univ of Oxford, Oxford, England"), Some("UK"));
        assert_eq!(country("Peking Univ, Beijing 100871, P.R. China. zhang@pku.edu.cn"), Some("China"));
        assert_eq!(country("Inst X, Berlin, Germany. Electronic address: a@b.de."), Some("Germany"));
        assert_eq!(country("Hosp Y, Seoul, Republic of Korea"), Some("South Korea"));
        assert_eq!(country("Mayo Clinic, Rochester, MN"), Some("USA"));
        assert_eq!(country("Dept Z, Univ of Ghana"), None);
        assert_eq!(country("Univ of Toronto, Toronto, ON M5S 1A1 Canada"), Some("Canada"));
        assert_eq!(country("Some Lab, Atlantis"), None);
        assert_eq!(country(""), None);
        assert_eq!(country("X, Belfast, Northern Ireland"), Some("UK"));
    }

    #[test]
    fn institutions() {
        assert_eq!(
            institution("Department of Medicine, Massachusetts General Hospital, Harvard Medical School, Boston, MA, USA"),
            "Massachusetts General Hospital"
        );
        assert_eq!(institution("Stanford University School of Medicine, Stanford, CA. a@b.edu"), "Stanford University School of Medicine");
        assert_eq!(institution("Acme Ltd, Paris, France"), "Acme Ltd");
        assert_eq!(institution(""), "");
    }

    #[test]
    fn buckets() {
        let b: Vec<&str> = [0, 1, 9, 10, 49, 50, 99, 100, 5000].iter().map(|&c| cites_bucket(c)).collect();
        assert_eq!(b, ["0", "1-9", "1-9", "10-49", "10-49", "50-99", "50-99", "100+", "100+"]);
        let r: Vec<&str> = [None, Some(0.2), Some(0.5), Some(1.9), Some(2.0), Some(9.0)].into_iter().map(rcr_bucket).collect();
        assert_eq!(r, ["-", "<0.5", "0.5-1", "1-2", "2-5", "5+"]);
    }

    #[test]
    fn ties_and_order() {
        let arts = vec![art("B", 2020, 5), art("A", 2021, 5), art("B", 2019, 5), art("A", 2020, 70), art("C", 2020, 0)];
        let s = build(&arts, &[Field::Journal], &opts());
        assert_eq!(cells(&s), ["A\t2", "B\t2", "C\t1"]);
        let s = build(&arts, &[Field::Cites], &opts());
        assert_eq!(cells(&s), ["1-9\t3", "0\t1", "50-99\t1"]);
        let s = build(&arts, &[Field::Year], &Opts { order: Some(true), ..opts() });
        assert_eq!(cells(&s), ["2019\t1", "2020\t3", "2021\t1"]);
        let s = build(&arts, &[Field::Year], &Opts { top: 1, order: Some(false), ..opts() });
        assert_eq!(cells(&s), ["2020\t3"]);
    }

    #[test]
    fn natural_numbers() {
        assert_eq!(natural_cmp("9", "10"), Ordering::Less);
        assert_eq!(natural_cmp("2020-02", "2020-10"), Ordering::Less);
        assert_eq!(natural_cmp("abc", "abd"), Ordering::Less);
    }

    #[test]
    fn layout_multi_other_min() {
        let mut a = art("Lancet", 2020, 1);
        a.authors = vec![Author { name: "Smith J".into(), ..Default::default() }, Author { name: "Lee K".into(), ..Default::default() }];
        let mut b = art("BMJ", 2020, 1);
        b.authors = vec![Author { name: "Smith J".into(), ..Default::default() }, Author { name: "Smith J".into(), ..Default::default() }];
        let c = art("Lancet", 2021, 1);
        let arts = [a, b, c];
        let s = build(&arts, &[Field::Journal, Field::Author], &opts());
        assert_eq!(cells(&s), ["# journal", "Lancet\t2", "BMJ\t1", "# author", "Smith J\t2", "Lee K\t1"]);
        assert_eq!(s.table.as_ref().unwrap().header, ["value", "n"]);
        assert_eq!(s.notes, ["n=3", "missing=author:1"]);
        let s = build(&arts, &[Field::Journal], &Opts { top: 1, other: true, ..opts() });
        assert_eq!(cells(&s), ["Lancet\t2", "(other)\t1"]);
        assert_eq!(s.table.as_ref().unwrap().header, ["journal", "n"]);
        let s = build(&arts, &[Field::Journal], &Opts { min: 2, other: true, ..opts() });
        assert_eq!(cells(&s), ["Lancet\t2", "(other)\t1"]);
        let s = build(&arts, &[Field::Author], &Opts { top: 0, ..opts() });
        assert_eq!(cells(&s), ["Smith J\t2", "Lee K\t1"]);
    }

    #[test]
    fn quartile_month_mesh() {
        let mut a = art("J", 2020, 0);
        a.jr = Some(JournalRank { q: 1, ..Default::default() });
        a.mesh = vec![
            Mesh { name: "Sepsis".into(), major: true, quals: vec![] },
            Mesh { name: "Humans".into(), major: false, quals: vec![] },
        ];
        let b = art("J", 2020, 0);
        let arts = [a, b];
        assert_eq!(cells(&build(&arts, &[Field::Q], &opts())), ["-\t1", "Q1\t1"]);
        assert_eq!(cells(&build(&arts, &[Field::Month], &opts())), ["2020-03\t2"]);
        assert_eq!(cells(&build(&arts, &[Field::Major], &opts())), ["Sepsis\t1"]);
        assert_eq!(cells(&build(&arts, &[Field::Mesh], &opts())), ["Humans\t1", "Sepsis\t1"]);
    }

    #[test]
    fn field_parsing() {
        assert_eq!(Field::parse("J").unwrap(), Field::Journal);
        assert_eq!(Field::parse("keywords").unwrap(), Field::Kw);
        assert!(Field::parse("zzz").unwrap_err().to_string().starts_with("facet: unknown field 'zzz'"));
        let a = Args::parse(&crate::args::split_words("journal,year,journal -n 3").unwrap()).unwrap();
        assert_eq!(parse_fields(&a).unwrap().0, [Field::Journal, Field::Year]);
        assert!(looks_like_id("31452104") && looks_like_id("doi:10.1/x") && looks_like_id("PMC12"));
        assert!(!looks_like_id("sepsis"));
    }
}
