use crate::args::Args;
use crate::enrich;
use crate::eutils;
use crate::filter::{Filters, need_for_key};
use crate::fuzzy;
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, anyhow, bail};
use std::cmp::Ordering;
use std::collections::HashMap;

pub const SORT_KEYS: &str = "year|date|cites|sjr|rcr|title|journal|first|pmid|score";

pub fn limit_skip(a: &Args) -> Result<(usize, usize)> {
    let num = |k: &str, flag: &str| -> Result<Option<usize>> {
        match a.get(k) {
            None => Ok(None),
            Some(v) => v.parse::<usize>().map(Some).map_err(|_| anyhow!("{flag}: bad number '{v}'")),
        }
    };
    let skip = match num("skip", "--skip")? {
        Some(s) => Some(s),
        None => num("offset", "--offset")?,
    };
    Ok((num("limit", "-n")?.unwrap_or(10), skip.unwrap_or(0)))
}

fn ids_only(a: &Args) -> bool {
    a.get("fmt") == Some("ids") || crate::fields::is_pmid_only(&a.csv("fields"))
}

fn trunc(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

pub async fn search(ctx: &Ctx, a: &Args) -> Result<Set> {
    let f = Filters::from_args(a)?;
    let term = f.query(&a.pos.join(" "));
    if term.is_empty() {
        bail!("{}", f.empty_query_err());
    }
    let (limit, skip) = limit_skip(a)?;
    let explain = a.flag("explain");
    let mut so = f.search_opts();
    let local_sort = f.local_sort();
    let mut notes = Vec::new();
    if explain {
        notes.push(format!("q={term}"));
    }
    if !f.needs_post() && local_sort.is_none() {
        so.retmax = limit.min(10_000);
        so.retstart = skip;
        let r = eutils::esearch(ctx, &term, &so).await?;
        if explain {
            notes.push(format!("translation={}", r.translation));
        } else if r.count == 0 {
            notes.push(format!("0 hits; pubmed read: {}", trunc(&r.translation, 200)));
        }
        let mut set = Set::from_ids(r.ids);
        set.total = Some(r.count);
        set.notes = notes;
        if !ids_only(a) {
            eutils::load(ctx, &mut set).await?;
        }
        return Ok(set);
    }

    let wanted = limit.saturating_add(skip);
    let want = if local_sort.is_some() { usize::MAX } else { wanted };
    let cap = if f.needs_post() { f.scan } else { f.scan.max(wanted) };
    so.retmax = cap.clamp(1, 10_000);
    if f.needs_post() && so.retmax < wanted {
        notes.push(format!("scan cap {} < wanted {wanted}; raise --scan", so.retmax));
    }
    let r = eutils::esearch(ctx, &term, &so).await?;
    if explain {
        notes.push(format!("translation={}", r.translation));
    } else if r.count == 0 {
        notes.push(format!("0 hits; pubmed read: {}", trunc(&r.translation, 200)));
    }
    let need = f.need();
    let mut kept: Vec<Article> = Vec::new();
    let mut i = 0;
    let mut batch = if want == usize::MAX { r.ids.len() } else { want.saturating_mul(3).clamp(50, 200) };
    while i < r.ids.len() && kept.len() < want {
        let end = (i + batch).min(r.ids.len());
        let mut s = Set::from_ids(r.ids[i..end].to_vec());
        eutils::load(ctx, &mut s).await?;
        let mut arts = s.arts;
        arts.retain(|x| f.basic(x, true));
        if need != Need::default() && !arts.is_empty() {
            enrich::apply(ctx, &mut arts, need).await?;
        }
        arts.retain(|x| f.enriched(x, true));
        kept.extend(arts);
        i = end;
        let rate = (kept.len() as f32 + 1.0) / (i as f32 + 2.0);
        batch = (((want.saturating_sub(kept.len())) as f32 / rate * 1.3) as usize).clamp(50, 400);
    }
    let exhausted = i >= r.ids.len() && r.ids.len() as u64 >= r.count;
    let matched = kept.len();
    let mut set = Set::from_arts(kept);
    if let Some(key) = local_sort {
        sort_set(&mut set, key, f.asc())?;
    }
    let from = skip.min(matched);
    let to = from.saturating_add(limit).min(matched);
    set.ids.truncate(to);
    set.ids.drain(..from);
    set.arts.truncate(to);
    set.arts.drain(..from);
    if f.needs_post() {
        set.total = exhausted.then_some(matched as u64);
        notes.push(format!("scanned={i}/{}", r.count));
    } else {
        set.total = Some(r.count);
        notes.push(format!("sorted {}/{}", i, r.count));
    }
    set.notes = notes;
    Ok(set)
}

pub async fn ensure_loaded(ctx: &Ctx, set: &mut Set) -> Result<()> {
    if set.arts.len() != set.ids.len() {
        set.arts.clear();
        eutils::load(ctx, set).await?;
    }
    Ok(())
}

pub async fn apply_filters(ctx: &Ctx, mut set: Set, f: &Filters) -> Result<Set> {
    f.reject_server_only()?;
    ensure_loaded(ctx, &mut set).await?;
    let mut arts = std::mem::take(&mut set.arts);
    arts.retain(|x| f.basic(x, false));
    if f.need() != Need::default() && !arts.is_empty() {
        enrich::apply(ctx, &mut arts, f.need()).await?;
    }
    arts.retain(|x| f.enriched(x, false));
    set.ids = arts.iter().map(|x| x.pmid).collect();
    set.scores.retain(|k, _| set.ids.contains(k));
    set.total = Some(arts.len() as u64);
    set.arts = arts;
    Ok(set)
}

pub async fn input_set(ctx: &Ctx, toks: &[String], input: Option<Set>) -> Result<Set> {
    if !toks.is_empty() {
        return Ok(Set::from_ids(eutils::resolve(ctx, toks).await?));
    }
    let set = input.ok_or_else(|| anyhow!("no input: pipe a search or give PMIDs"))?;
    if set.ids.is_empty() && (set.table.is_some() || set.text.is_some()) {
        bail!("input has no PMIDs");
    }
    Ok(set)
}

pub fn norm_key(k: &str) -> Result<&'static str> {
    Ok(match k.to_ascii_lowercase().as_str() {
        "year" => "year",
        "date" | "pubdate" | "recent" => "date",
        "cites" | "citations" | "cited" => "cites",
        "sjr" => "sjr",
        "rcr" => "rcr",
        "title" => "title",
        "journal" | "jt" => "journal",
        "first" | "author" => "first",
        "pmid" | "id" => "pmid",
        "score" | "relevance" => "score",
        _ => bail!("sort key '{k}'? {SORT_KEYS}"),
    })
}

pub fn key_need(key: &str) -> Need {
    need_for_key(key)
}

enum K {
    N(f64),
    S(String),
}

fn key_of(a: Option<&Article>, id: u32, scores: &HashMap<u32, f32>, key: &str) -> Result<Option<K>> {
    match key {
        "pmid" => return Ok(Some(K::N(id as f64))),
        "score" => return Ok(scores.get(&id).map(|s| K::N(*s as f64))),
        _ => {}
    }
    let a = a.ok_or_else(|| anyhow!("sort {key}: records not loaded"))?;
    Ok(match key {
        "year" => (a.year > 0).then_some(K::N(a.year as f64)),
        "date" => (!a.date.is_empty()).then(|| K::S(full_date(&a.date))),
        "cites" => a.m.as_ref().map(|m| K::N(m.cites as f64)),
        "rcr" => a.m.as_ref().and_then(|m| m.rcr).map(|v| K::N(v as f64)),
        "sjr" => a.jr.as_ref().map(|j| K::N(j.sjr as f64)),
        "title" => Some(K::S(fuzzy::fold(&a.title))),
        "journal" => Some(K::S(fuzzy::fold(if a.journal.iso.is_empty() { &a.journal.title } else { &a.journal.iso }))),
        "first" => crate::fields::first_author(a).map(|x| K::S(fuzzy::fold(&x.name))),
        _ => bail!("sort key '{key}'? {SORT_KEYS}"),
    })
}

fn full_date(d: &str) -> String {
    let mut parts = d.splitn(3, '-');
    let y = parts.next().unwrap_or("");
    let m = parts.next().filter(|m| !m.is_empty()).unwrap_or("01");
    let day = parts.next().filter(|x| !x.is_empty()).unwrap_or("01");
    format!("{y}-{m}-{day}")
}

fn cmp_key(a: &Option<K>, b: &Option<K>, desc: bool) -> Ordering {
    let o = match (a, b) {
        (None, None) => return Ordering::Equal,
        (None, _) => return Ordering::Greater,
        (_, None) => return Ordering::Less,
        (Some(K::N(x)), Some(K::N(y))) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Some(K::S(x)), Some(K::S(y))) => x.cmp(y),
        _ => Ordering::Equal,
    };
    if desc { o.reverse() } else { o }
}

pub fn sort_set(set: &mut Set, key: &str, asc: Option<bool>) -> Result<()> {
    let key = norm_key(key)?;
    let desc = match asc {
        Some(a) => !a,
        None => !matches!(key, "title" | "journal" | "first"),
    };
    let have = set.arts.len() == set.ids.len();
    let keys = (0..set.ids.len()).map(|i| key_of(have.then(|| &set.arts[i]), set.ids[i], &set.scores, key)).collect::<Result<Vec<_>>>()?;
    let mut idx: Vec<usize> = (0..keys.len()).collect();
    idx.sort_by(|&i, &j| cmp_key(&keys[i], &keys[j], desc));
    set.ids = idx.iter().map(|&i| set.ids[i]).collect();
    if have {
        let mut old: Vec<Option<Article>> = std::mem::take(&mut set.arts).into_iter().map(Some).collect();
        set.arts = idx.iter().filter_map(|&i| old[i].take()).collect();
    }
    Ok(())
}

pub async fn sort_loaded(ctx: &Ctx, set: &mut Set, key: &str, asc: Option<bool>) -> Result<()> {
    let key = norm_key(key)?;
    if !matches!(key, "pmid" | "score") {
        ensure_loaded(ctx, set).await?;
        let need = key_need(key);
        if need != Need::default() && !set.arts.is_empty() {
            enrich::apply(ctx, &mut set.arts, need).await?;
        }
    }
    sort_set(set, key, asc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn art(pmid: u32, year: u16, cites: Option<u32>, title: &str) -> Article {
        Article { pmid, year, title: title.into(), m: cites.map(|c| Metrics { cites: c, ..Default::default() }), ..Default::default() }
    }

    fn set() -> Set {
        Set::from_arts(vec![art(1, 2020, Some(5), "b"), art(2, 2022, None, "A"), art(3, 2021, Some(50), "c"), art(4, 2022, Some(7), "d")])
    }

    #[test]
    fn sorts() {
        let mut s = set();
        sort_set(&mut s, "cites", None).unwrap();
        assert_eq!(s.ids, [3, 4, 1, 2]);
        assert_eq!(s.arts.iter().map(|a| a.pmid).collect::<Vec<_>>(), [3, 4, 1, 2]);
        sort_set(&mut s, "cites", Some(true)).unwrap();
        assert_eq!(s.ids, [1, 4, 3, 2]);
        sort_set(&mut s, "title", None).unwrap();
        assert_eq!(s.ids, [2, 1, 3, 4]);
        sort_set(&mut s, "year", None).unwrap();
        assert_eq!(s.ids, [2, 4, 3, 1]);
        sort_set(&mut s, "pmid", None).unwrap();
        assert_eq!(s.ids, [4, 3, 2, 1]);
        assert!(sort_set(&mut s, "bogus", None).unwrap_err().to_string().starts_with("sort key 'bogus'?"));
    }

    #[test]
    fn pubdate_sorts_by_full_date() {
        let mk = |pmid: u32, date: &str| Article { pmid, date: date.into(), year: date[..4].parse().unwrap(), ..Default::default() };
        let mut s = Set::from_arts(vec![mk(1, "2025-01-01"), mk(2, "2025-10-22"), mk(3, "2025"), mk(4, "2025-03")]);
        sort_set(&mut s, "pubdate", None).unwrap();
        assert_eq!(s.ids, [2, 4, 1, 3]);
        sort_set(&mut s, "pubdate", Some(true)).unwrap();
        assert_eq!(s.ids, [1, 3, 4, 2]);
        sort_set(&mut s, "date", None).unwrap();
        assert_eq!(s.ids, [2, 4, 1, 3]);
    }

    #[test]
    fn author_sort_skips_collective_authors() {
        let au = |name: &str, coll: bool| Author { name: name.into(), coll, ..Default::default() };
        let a = Article { pmid: 1, authors: vec![au("Z Study Group", true), au("Adams A", false)], ..Default::default() };
        let b = Article { pmid: 2, authors: vec![au("Brown B", false)], ..Default::default() };
        let mut s = Set::from_arts(vec![b, a]);
        sort_set(&mut s, "first", None).unwrap();
        assert_eq!(s.ids, [1, 2]);
        sort_set(&mut s, "author", Some(false)).unwrap();
        assert_eq!(s.ids, [2, 1]);
    }

    #[test]
    fn ids_only_honours_field_aliases() {
        let p = |l: &str| Args::parse(&l.split_whitespace().map(String::from).collect::<Vec<_>>()).unwrap();
        assert!(ids_only(&p("-f pmid")));
        assert!(ids_only(&p("-f id")));
        assert!(ids_only(&p("--fmt ids")));
        assert!(!ids_only(&p("-f id,title")));
        assert!(!ids_only(&p("")));
    }

    #[test]
    fn sort_ids_only() {
        let mut s = Set::from_ids(vec![5, 9, 7]);
        s.scores.insert(5, 0.1);
        s.scores.insert(7, 0.9);
        sort_set(&mut s, "score", None).unwrap();
        assert_eq!(s.ids, [7, 5, 9]);
        assert!(sort_set(&mut s, "year", None).is_err());
    }

    #[test]
    fn limits() {
        let p = |l: &str| Args::parse(&crate::args::split_words(l).unwrap()).unwrap();
        assert_eq!(limit_skip(&p("x")).unwrap(), (10, 0));
        assert_eq!(limit_skip(&p("x -n 5 --skip 20")).unwrap(), (5, 20));
        assert_eq!(limit_skip(&p("x -n 5 -o 3")).unwrap(), (5, 3));
    }
}
