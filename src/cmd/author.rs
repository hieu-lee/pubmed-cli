use crate::args::Args;
use crate::eutils::{self, SearchOpts};
use crate::filter::Filters;
use crate::fuzzy;
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::fmt::Write;

const SCAN_DEFAULT: usize = 200;
const SCAN_MAX: usize = 2000;
const CHECKTAGS: &[&str] = &[
    "Humans", "Animals", "Male", "Female", "Adult", "Middle Aged", "Aged", "Aged, 80 and over", "Adolescent", "Child",
    "Young Adult", "Infant", "Child, Preschool", "Mice", "Rats", "Retrospective Studies", "Prospective Studies",
];
const NOISE_TYPES: &[&str] = &["Journal Article"];

#[derive(Debug, PartialEq)]
struct Name {
    last: String,
    ini: String,
}

pub async fn run(ctx: &Ctx, a: &Args, _input: Option<Set>) -> Result<Set> {
    let raw = a.first_pos_or_all();
    let who = parse_name(&raw);
    if who.last.is_empty() {
        bail!("usage: author \"Last I\" [--aff X] [--scan 200] [--list]");
    }
    let list = a.flag("list");
    let fuzzy = a.flag("fuzzy");
    let exact = a.flag("exact");
    let limit = a.num::<usize>("limit")?;
    let scan = a
        .num::<usize>("scan")?
        .or(if list { None } else { limit })
        .unwrap_or(SCAN_DEFAULT)
        .clamp(1, SCAN_MAX);

    let display = raw.trim().trim_end_matches(',').to_string();
    let q = if who.ini.is_empty() { who.last.clone() } else { format!("{} {}", who.last, who.ini) };
    let mut term = match (exact, fuzzy, who.ini.chars().next()) {
        (true, _, _) => format!("\"{q}\"[au]"),
        (false, true, Some(i)) => format!("{} {i}*[au]", who.last),
        _ => format!("{q}[au]"),
    };
    if let Some(aff) = a.get("aff") {
        let _ = write!(term, " AND \"{}\"[ad]", aff.replace('"', ""));
    }
    if let Some(d) = Filters::from_args(a)?.date_query() {
        let _ = write!(term, " AND {d}");
    }
    let o = SearchOpts { sort: Some("pubdate".into()), retmax: scan, ..Default::default() };
    let res = eutils::esearch(ctx, &term, &o).await?;
    if res.ids.is_empty() {
        bail!("no articles for {term}");
    }
    let want = a.get("aff").map(aff_words).unwrap_or_default();
    let mut set = Set::from_ids(res.ids);
    eutils::load(ctx, &mut set).await?;
    set.total = Some(res.count);

    let matched: Vec<(usize, usize)> = set
        .arts
        .iter()
        .enumerate()
        .filter_map(|(i, art)| {
            (0..art.authors.len())
                .find(|&j| same_person(&art.authors[j], &who, fuzzy, exact) && aff_ok(art, j, &want))
                .map(|j| (i, j))
        })
        .collect();

    if list {
        let skip = a.num::<usize>("offset")?.or(a.num("skip")?).unwrap_or(0);
        let keep: Vec<Article> = matched
            .iter()
            .skip(skip)
            .take(limit.unwrap_or(10))
            .map(|(i, _)| set.arts[*i].clone())
            .collect();
        let mut out = Set::from_arts(keep);
        out.total = Some(res.count);
        out.notes = set.notes;
        return Ok(out);
    }

    let txt = profile(&display, res.count, set.arts.len(), &set.arts, &matched, &who);
    let mut out = Set::text(txt);
    out.total = Some(res.count);
    Ok(out)
}

fn fold(s: &str) -> String {
    s.chars()
        .flat_map(|c| -> Vec<char> {
            let r: &str = match c {
                'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' => "a",
                'ç' | 'ć' | 'č' | 'ĉ' | 'Ç' | 'Ć' | 'Č' | 'Ĉ' => "c",
                'ď' | 'đ' | 'Ď' | 'Đ' => "d",
                'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => "e",
                'ĝ' | 'ğ' | 'Ĝ' | 'Ğ' => "g",
                'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' | 'ı' | 'İ' | 'Ì' | 'Í' | 'Î' | 'Ï' | 'Ī' => "i",
                'ł' | 'ľ' | 'ĺ' | 'Ł' | 'Ľ' | 'Ĺ' => "l",
                'ñ' | 'ń' | 'ň' | 'Ñ' | 'Ń' | 'Ň' => "n",
                'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' | 'Ő' => "o",
                'ŕ' | 'ř' | 'Ŕ' | 'Ř' => "r",
                'ś' | 'š' | 'ş' | 'ș' | 'Ś' | 'Š' | 'Ş' | 'Ș' => "s",
                'ť' | 'ţ' | 'ț' | 'Ť' | 'Ţ' | 'Ț' => "t",
                'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' | 'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ū' | 'Ů' | 'Ű' => "u",
                'ý' | 'ÿ' | 'Ý' | 'Ÿ' => "y",
                'ź' | 'ż' | 'ž' | 'Ź' | 'Ż' | 'Ž' => "z",
                'ß' => "ss",
                'æ' | 'Æ' => "ae",
                'œ' | 'Œ' => "oe",
                _ => return c.to_lowercase().collect(),
            };
            r.chars().collect()
        })
        .collect()
}

fn initials(fore: &str) -> String {
    fore.split(|c: char| c.is_whitespace() || c == '-' || c == '.')
        .filter_map(|w| w.chars().find(|c| c.is_alphabetic()))
        .flat_map(char::to_uppercase)
        .collect()
}

fn parse_name(s: &str) -> Name {
    let p = fuzzy::parse_person(s.trim().trim_end_matches(','));
    Name { last: p.last, ini: p.ini.to_uppercase() }
}

fn author_name(au: &Author) -> Name {
    let mut n = parse_name(&au.name);
    if n.ini.is_empty() {
        n.ini = initials(&au.fore);
    }
    n
}

fn squash(s: &str) -> String {
    fold(s).chars().filter(|c| c.is_alphanumeric()).collect()
}

fn same_person(au: &Author, who: &Name, fuzzy: bool, exact: bool) -> bool {
    let n = author_name(au);
    let (l, wl) = (squash(&n.last), squash(&who.last));
    let last_ok = l == wl || (fuzzy && (l.ends_with(&wl) || wl.ends_with(&l)) && !l.is_empty() && !wl.is_empty());
    if !last_ok {
        return false;
    }
    let (ni, wi) = (fold(&n.ini), fold(&who.ini));
    if wi.is_empty() || ni.is_empty() {
        return wi.is_empty() || fuzzy;
    }
    if fuzzy {
        ni.chars().next() == wi.chars().next()
    } else if exact {
        ni == wi
    } else {
        ni.starts_with(&wi)
    }
}

fn aff_words(s: &str) -> Vec<String> {
    fold(s).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

fn aff_ok(a: &Article, j: usize, want: &[String]) -> bool {
    if want.is_empty() {
        return true;
    }
    let has = |raw: &String| {
        let f = fold(raw);
        want.iter().all(|w| f.contains(w.as_str()))
    };
    let own: Vec<&String> = a.authors[j].aff.iter().filter_map(|i| a.affs.get(*i as usize)).collect();
    match (own.is_empty(), a.affs.len()) {
        (false, _) => own.into_iter().any(has),
        (true, 1) => has(&a.affs[0]),
        (true, _) => a.authors.iter().all(|x| x.aff.is_empty()) && a.affs.iter().any(has),
    }
}

#[cfg(test)]
fn find_author(a: &Article, who: &Name, fuzzy: bool) -> Option<usize> {
    a.authors.iter().position(|au| same_person(au, who, fuzzy, false))
}

const INST: &[&str] = &[
    "univ", "instit", "hospital", "hôpital", "klinik", "clinic", "college", "school", "center", "centre", "laborator",
    "academy", "polytech", "ecole", "école", "foundation", "company", "inc", "corp", "ltd", "google", "microsoft", "nih",
    "national", "medical", "research",
];

fn norm_aff(raw: &str) -> String {
    let segs: Vec<&str> = raw.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return String::new();
    }
    let pick = segs
        .iter()
        .position(|s| {
            let l = s.to_lowercase();
            INST.iter().any(|k| l.contains(k))
        })
        .unwrap_or(0);
    let mut out = segs[pick].trim_start_matches(|c: char| !c.is_alphabetic()).trim_end_matches('.').to_string();
    if segs.len() > 1 && pick != segs.len() - 1 {
        let c = segs[segs.len() - 1].trim_end_matches('.');
        if c.len() < 25 && !c.contains('@') && !c.chars().any(|c| c.is_ascii_digit()) {
            out.push_str(", ");
            out.push_str(c);
        }
    }
    out
}

fn aff_of(a: &Article, j: usize) -> Option<String> {
    let idx = a.authors[j].aff.first().map(|i| *i as usize);
    let raw = match idx {
        Some(i) => a.affs.get(i)?,
        None if a.affs.len() == 1 => &a.affs[0],
        None => return None,
    };
    Some(norm_aff(raw)).filter(|s| !s.is_empty())
}

#[derive(Default)]
struct Tally {
    n: u32,
    first: u16,
    last: u16,
    name: String,
}

fn tally(map: &mut HashMap<String, Tally>, key: String, name: impl FnOnce() -> String, year: u16) {
    let t = map.entry(key).or_insert_with(|| Tally { name: name(), first: year, last: year, ..Default::default() });
    t.n += 1;
    if year != 0 {
        t.first = if t.first == 0 { year } else { t.first.min(year) };
        t.last = t.last.max(year);
    }
}

fn top(map: HashMap<String, Tally>, k: usize) -> Vec<Tally> {
    let mut v: Vec<Tally> = map.into_values().collect();
    v.sort_by(|a, b| b.n.cmp(&a.n).then_with(|| a.name.cmp(&b.name)));
    v.truncate(k);
    v
}

fn join_top(v: &[Tally], span: bool) -> String {
    v.iter()
        .map(|t| {
            if span && t.first != 0 {
                if t.first == t.last { format!("{} {} {}", t.name, t.n, t.first) } else { format!("{} {} {}-{}", t.name, t.n, t.first, t.last) }
            } else {
                format!("{} {}", t.name, t.n)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn profile(display: &str, total: u64, scanned: usize, arts: &[Article], matched: &[(usize, usize)], who: &Name) -> String {
    let mut years: HashMap<u16, u32> = HashMap::new();
    let mut journals = HashMap::new();
    let mut affs = HashMap::new();
    let mut co = HashMap::new();
    let mut mesh = HashMap::new();
    let mut types = HashMap::new();
    let mut orcids: HashMap<String, Tally> = HashMap::new();
    let (mut firsts, mut lasts) = (0, 0);

    for (i, j) in matched {
        let a = &arts[*i];
        if a.year != 0 {
            *years.entry(a.year).or_default() += 1;
        }
        let jn = if a.journal.iso.is_empty() { &a.journal.title } else { &a.journal.iso };
        if !jn.is_empty() {
            tally(&mut journals, jn.to_lowercase(), || jn.clone(), a.year);
        }
        if let Some(af) = aff_of(a, *j) {
            let key = fold(af.split(", ").next().unwrap_or(&af));
            tally(&mut affs, key, || af, a.year);
        }
        for (k, au) in a.authors.iter().enumerate() {
            if k == *j {
                if !au.orcid.is_empty() {
                    tally(&mut orcids, au.orcid.clone(), || au.orcid.clone(), a.year);
                }
                continue;
            }
            let n = author_name(au);
            let key = format!("{} {}", squash(&n.last), fold(&n.ini));
            let shown = || if n.ini.is_empty() || !parse_name(&au.name).ini.is_empty() { au.name.clone() } else { format!("{} {}", au.name, n.ini) };
            tally(&mut co, key, shown, a.year);
        }
        for m in a.mesh.iter().filter(|m| !CHECKTAGS.contains(&m.name.as_str())) {
            tally(&mut mesh, m.name.to_lowercase(), || m.name.clone(), a.year);
        }
        for t in a.types.iter().filter(|t| !NOISE_TYPES.contains(&t.as_str()) && !t.starts_with("Research Support")) {
            tally(&mut types, t.to_lowercase(), || t.clone(), a.year);
        }
        firsts += (a.authors.iter().position(|x| !x.coll).unwrap_or(0) == *j) as u32;
        lasts += (a.authors.iter().filter(|x| !x.coll).count() > 1 && a.authors.iter().rposition(|x| !x.coll) == Some(*j)) as u32;
    }

    let mut o = String::new();
    let (lo, hi) = (years.keys().min().copied().unwrap_or(0), years.keys().max().copied().unwrap_or(0));
    let _ = write!(o, "{display} pubmed={total} scanned={scanned} matched={}", matched.len());
    if matched.is_empty() {
        let _ = write!(o, "\nno article author matched '{}' (try --fuzzy)", who.last);
        return o;
    }
    let _ = writeln!(o, " years={lo}-{hi} first={firsts} last={lasts}");
    let mut ys: Vec<(u16, u32)> = years.into_iter().collect();
    ys.sort_by_key(|y| std::cmp::Reverse(y.0));
    let recent: Vec<String> = ys.iter().take(10).map(|(y, n)| format!("{y}:{n}")).collect();
    let older: u32 = ys.iter().skip(10).map(|(_, n)| n).sum();
    let _ = write!(o, "yr {}", recent.join(" "));
    if older > 0 {
        let _ = write!(o, " earlier:{older}");
    }
    o.push('\n');
    let multi = affs.len() > 1;
    let aff_top = top(affs, 5);
    let sections: [(&str, Vec<Tally>, bool); 5] = [
        ("j", top(journals, 5), false),
        ("aff", aff_top, true),
        ("co", top(co, 8), false),
        ("mesh", top(mesh, 8), false),
        ("type", top(types, 5), false),
    ];
    for (label, v, span) in &sections {
        if !v.is_empty() {
            let _ = writeln!(o, "{label}: {}", join_top(v, *span));
        }
    }
    let ids = top(orcids, 3);
    if !ids.is_empty() {
        let _ = writeln!(o, "orcid: {}", ids.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(" "));
    }
    let aff = &sections[1].1;
    if multi && aff.len() > 1 && (aff[0].n as usize) * 10 < matched.len() * 7 {
        o.push_str("hint: several affiliations, possibly different people; narrow with --aff X");
    }
    o.truncate(o.trim_end().len());
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn au(name: &str, fore: &str, aff: &[u16], orcid: &str) -> Author {
        Author { name: name.into(), fore: fore.into(), aff: aff.to_vec(), orcid: orcid.into(), ..Default::default() }
    }

    #[test]
    fn name_parsing() {
        assert_eq!(parse_name("Hinton G"), Name { last: "hinton".into(), ini: "G".into() });
        assert_eq!(parse_name("van der Berg JA"), Name { last: "van der berg".into(), ini: "JA".into() });
        assert_eq!(parse_name("Hinton"), Name { last: "hinton".into(), ini: String::new() });
        assert_eq!(parse_name("Smith, J."), Name { last: "smith".into(), ini: "J".into() });
        assert_eq!(parse_name("de Azevedo, W F"), Name { last: "de azevedo".into(), ini: "WF".into() });
        assert_eq!(parse_name("Cheng, K.T."), Name { last: "cheng".into(), ini: "KT".into() });
    }

    #[test]
    fn exact_requires_equal_initials() {
        let who = parse_name("Wang X");
        assert!(same_person(&au("Wang XY", "", &[], ""), &who, false, false));
        assert!(!same_person(&au("Wang XY", "", &[], ""), &who, false, true));
        assert!(same_person(&au("Wang X", "", &[], ""), &who, false, true));
        assert!(!same_person(&au("Wang Y", "", &[], ""), &who, false, true));
    }

    #[test]
    fn matching() {
        let who = parse_name("Müller J");
        assert!(same_person(&au("Muller", "Jean-Pierre", &[], ""), &who, false, false));
        assert!(same_person(&au("Müller JP", "", &[], ""), &who, false, false));
        assert!(!same_person(&au("Muller K", "Klaus", &[], ""), &who, false, false));
        assert!(!same_person(&au("Miller J", "", &[], ""), &who, false, false));
        let exactish = parse_name("Smith JA");
        assert!(!same_person(&au("Smith J", "John", &[], ""), &exactish, false, false));
        assert!(same_person(&au("Smith J", "John", &[], ""), &exactish, true, false));
        assert!(same_person(&au("Garcia-Lopez M", "", &[], ""), &parse_name("Lopez M"), true, false));
        assert!(!same_person(&au("Garcia-Lopez M", "", &[], ""), &parse_name("Lopez M"), false, false));
    }

    #[test]
    fn aff_filter_checks_the_matching_author() {
        let art = Article {
            authors: vec![au("Smith J", "John", &[0], ""), au("Brown K", "Kim", &[1], "")],
            affs: vec!["Mayo Clinic, Rochester, USA".into(), "Harvard Medical School, Boston, USA".into()],
            ..Default::default()
        };
        let want = aff_words("Harvard");
        assert!(!aff_ok(&art, 0, &want));
        assert!(aff_ok(&art, 1, &want));
        assert!(aff_ok(&art, 0, &aff_words("mayo clinic")));
        assert!(aff_ok(&art, 0, &[]));
        let bare = Article { authors: vec![au("Lee K", "", &[], "")], affs: vec!["Dept X, Harvard".into(), "Other".into()], ..Default::default() };
        assert!(aff_ok(&bare, 0, &want));
        let mixed = Article {
            authors: vec![au("Lee K", "", &[], ""), au("Brown K", "", &[0], "")],
            affs: vec!["Harvard".into(), "Other".into()],
            ..Default::default()
        };
        assert!(!aff_ok(&mixed, 0, &want));
    }

    #[test]
    fn aff_normalisation() {
        assert_eq!(
            norm_aff("Department of Computer Science, University of Toronto, Toronto, ON, Canada."),
            "University of Toronto, Canada"
        );
        assert_eq!(norm_aff("Google Brain."), "Google Brain");
        assert_eq!(norm_aff("Dept of X, Y Univ, 12345 Town"), "Y Univ");
    }

    #[test]
    fn profile_counts() {
        let mk = |pmid, year, authors: Vec<Author>| Article {
            pmid,
            year,
            journal: JournalInfo { iso: "Nature".into(), ..Default::default() },
            affs: vec!["Dept X, University of Toronto, Canada".into()],
            authors,
            types: vec!["Journal Article".into(), "Review".into()],
            mesh: vec![Mesh { name: "Humans".into(), ..Default::default() }, Mesh { name: "Neural Networks".into(), ..Default::default() }],
            ..Default::default()
        };
        let arts = vec![
            mk(1, 2020, vec![au("Hinton G", "Geoffrey", &[0], "0000-0001"), au("Lee K", "Kim", &[], "")]),
            mk(2, 2021, vec![au("Lee K", "Kim", &[], ""), au("Hinton GE", "Geoffrey E", &[0], "")]),
        ];
        let who = parse_name("Hinton G");
        let m: Vec<(usize, usize)> = arts.iter().enumerate().filter_map(|(i, a)| find_author(a, &who, false).map(|j| (i, j))).collect();
        assert_eq!(m, vec![(0, 0), (1, 1)]);
        let p = profile("Hinton G", 5, 2, &arts, &m, &who);
        let mut consortium = mk(3, 2022, vec![au("Smith J", "John", &[], ""), au("Lee K", "Kim", &[], "")]);
        consortium.authors.push(Author { name: "EMPEROR Investigators".into(), coll: true, ..Default::default() });
        let carts = vec![consortium];
        let lee = parse_name("Lee K");
        let cm: Vec<(usize, usize)> = carts.iter().enumerate().filter_map(|(i, a)| find_author(a, &lee, false).map(|j| (i, j))).collect();
        assert_eq!(cm, vec![(0, 1)]);
        assert!(profile("Lee K", 1, 1, &carts, &cm, &lee).contains("first=0 last=1"));
        assert_eq!(
            p,
            "Hinton G pubmed=5 scanned=2 matched=2 years=2020-2021 first=1 last=1\nyr 2021:1 2020:1\nj: Nature 2\naff: University of Toronto, Canada 2 2020-2021\nco: Lee K 2\nmesh: Neural Networks 2\ntype: Review 2\norcid: 0000-0001"
        );
    }
}
