use crate::http::{Ctx, form};
use crate::model::*;
use crate::xml;
use anyhow::{Result, anyhow, bail};
use futures::StreamExt;
use futures::future::{join_all, try_join_all};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

const ART_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const SRCH_TTL: Duration = Duration::from_secs(3600);
const CHUNK: usize = 200;
const PARALLEL: usize = 3;

#[derive(Debug, Clone, Default)]
pub struct SearchOpts {
    pub sort: Option<String>,
    pub datetype: Option<String>,
    pub mindate: Option<String>,
    pub maxdate: Option<String>,
    pub retmax: usize,
    pub retstart: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub count: u64,
    pub ids: Vec<u32>,
    pub translation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Related,
    CitedBy,
    Refs,
}

#[derive(Deserialize)]
struct EsRoot {
    esearchresult: Option<EsRes>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct EsRes {
    count: Option<String>,
    #[serde(default)]
    idlist: Vec<String>,
    querytranslation: Option<String>,
    errorlist: Option<serde_json::Value>,
    #[serde(rename = "ERROR")]
    error: Option<String>,
}

fn short(msg: &str) -> String {
    let m = msg.rsplit("Details: ").next().unwrap_or(msg).trim();
    m.chars().take(120).collect()
}

fn parse_search(body: &[u8]) -> Result<SearchResult> {
    let root: EsRoot = serde_json::from_slice(body).map_err(|_| anyhow!("bad esearch response"))?;
    let Some(r) = root.esearchresult else {
        bail!("esearch: {}", short(root.error.as_deref().unwrap_or("bad response")));
    };
    if let Some(e) = r.error {
        bail!("bad query: {}", short(&e));
    }
    let mut translation = r.querytranslation.unwrap_or_default();
    let mut missing = Vec::new();
    for k in ["phrasesnotfound", "fieldsnotfound"] {
        if let Some(a) = r.errorlist.as_ref().and_then(|e| e.get(k)).and_then(|v| v.as_array()) {
            missing.extend(a.iter().filter_map(|v| v.as_str()));
        }
    }
    if !missing.is_empty() {
        translation.push_str(" [not found: ");
        translation.push_str(&missing.join(", "));
        translation.push(']');
    }
    Ok(SearchResult {
        count: r.count.and_then(|c| c.parse().ok()).unwrap_or(0),
        ids: r.idlist.iter().filter_map(|i| i.parse().ok()).collect(),
        translation,
    })
}

fn sort_param(s: &str) -> &str {
    match s {
        "date" | "recent" => "most_recent",
        "pubdate" => "pub_date",
        "author" => "Author",
        "journal" => "JournalName",
        s => s,
    }
}

pub async fn esearch(ctx: &Ctx, term: &str, o: &SearchOpts) -> Result<SearchResult> {
    let retmax = o.retmax.to_string();
    let retstart = o.retstart.to_string();
    let mut p: Vec<(&str, &str)> = vec![("db", "pubmed"), ("term", term), ("retmode", "json"), ("retmax", &retmax)];
    if o.retstart > 0 {
        p.push(("retstart", &retstart));
    }
    if let Some(s) = &o.sort {
        p.push(("sort", sort_param(s)));
    }
    if o.mindate.is_some() || o.maxdate.is_some() {
        p.push(("datetype", o.datetype.as_deref().unwrap_or("pdat")));
        p.push(("mindate", o.mindate.as_deref().unwrap_or("1000")));
        p.push(("maxdate", o.maxdate.as_deref().unwrap_or("3000")));
    }
    let key = form(&p);
    if let Some(b) = ctx.http.cache_get("srch", &key, SRCH_TTL).await
        && let Ok(r) = parse_search(&b) {
            return Ok(r);
        }
    let body = ctx.http.eutils("esearch.fcgi", &p).await?;
    let r = parse_search(body.as_bytes())?;
    ctx.http.cache_put("srch", &key, body.as_bytes()).await;
    Ok(r)
}

pub async fn count(ctx: &Ctx, term: &str, o: &SearchOpts) -> Result<u64> {
    let o = SearchOpts { retmax: 0, retstart: 0, sort: None, ..o.clone() };
    Ok(esearch(ctx, term, &o).await?.count)
}

fn dedup(ids: &[u32]) -> Vec<u32> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter().copied().filter(|i| seen.insert(*i)).collect()
}

async fn fetch_chunk(ctx: &Ctx, ids: &[u32]) -> Result<Vec<Article>> {
    let list = ids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let body = ctx
        .http
        .eutils("efetch.fcgi", &[("db", "pubmed"), ("id", &list), ("retmode", "xml"), ("rettype", "abstract")])
        .await?;
    let arts = xml::parse_articles(&body)?;
    join_all(arts.iter().filter_map(|a| {
        let v = serde_json::to_vec(a).ok()?;
        Some(async move { ctx.http.cache_put("art6", &a.pmid.to_string(), &v).await })
    }))
    .await;
    Ok(arts)
}

pub async fn fetch(ctx: &Ctx, ids: &[u32]) -> Result<Vec<Article>> {
    let ids = dedup(ids);
    let hits = join_all(ids.iter().map(|&id| async move {
        let b = ctx.http.cache_get("art6", &id.to_string(), ART_TTL).await?;
        serde_json::from_slice::<Article>(&b).ok().filter(|a| a.pmid == id)
    }))
    .await;
    let mut got: HashMap<u32, Article> = HashMap::with_capacity(ids.len());
    let mut missing = Vec::new();
    for (id, h) in ids.iter().zip(hits) {
        match h {
            Some(a) => {
                got.insert(*id, a);
            }
            None => missing.push(*id),
        }
    }
    let mut fetched = futures::stream::iter(missing.chunks(CHUNK))
        .map(|c| fetch_chunk(ctx, c))
        .buffer_unordered(PARALLEL);
    while let Some(r) = fetched.next().await {
        for a in r? {
            got.insert(a.pmid, a);
        }
    }
    Ok(ids.iter().filter_map(|id| got.remove(id)).collect())
}

pub async fn load(ctx: &Ctx, set: &mut Set) -> Result<()> {
    if set.arts.len() == set.ids.len() && set.arts.iter().zip(&set.ids).all(|(a, i)| a.pmid == *i) {
        return Ok(());
    }
    let arts = fetch(ctx, &set.ids).await?;
    set.ids = arts.iter().map(|a| a.pmid).collect();
    set.arts = arts;
    Ok(())
}

#[derive(Deserialize)]
struct ElRoot {
    #[serde(default)]
    linksets: Vec<ElSet>,
    #[serde(alias = "ERROR", alias = "Error")]
    error: Option<String>,
}

#[derive(Deserialize)]
struct ElSet {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    linksetdbs: Vec<ElDb>,
    #[serde(default, alias = "ERROR", alias = "Error")]
    error: Option<String>,
}

#[derive(Deserialize)]
struct ElDb {
    #[serde(default)]
    linkname: String,
    #[serde(default)]
    links: Vec<ElLink>,
    #[serde(default, alias = "ERROR", alias = "Error")]
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ElLink {
    Scored { id: String, score: Option<f32> },
    Plain(String),
}

fn parse_elink(body: &[u8], linkname: &str) -> Result<Links> {
    let root: ElRoot = serde_json::from_slice(body).map_err(|_| anyhow!("bad elink response"))?;
    let nested = root.linksets.iter().flat_map(|s| s.error.iter().chain(s.linksetdbs.iter().filter_map(|d| d.error.as_ref())));
    if let Some(e) = root.error.as_ref().or_else(|| nested.into_iter().next()) {
        bail!("elink: {}", crate::http::redact(&e.chars().take(80).collect::<String>()));
    }
    let mut out = Vec::with_capacity(root.linksets.len());
    for s in root.linksets {
        let Some(src) = s.ids.first().and_then(|i| i.parse::<u32>().ok()) else { continue };
        let links = s
            .linksetdbs
            .into_iter()
            .find(|d| d.linkname == linkname)
            .map(|d| {
                d.links
                    .into_iter()
                    .filter_map(|l| match l {
                        ElLink::Scored { id, score } => Some((id.parse::<u32>().ok()?, score)),
                        ElLink::Plain(id) => Some((id.parse::<u32>().ok()?, None)),
                    })
                    .filter(|(id, _)| *id != src)
                    .collect()
            })
            .unwrap_or_default();
        out.push((src, links));
    }
    Ok(out)
}

pub type Links = Vec<(u32, Vec<(u32, Option<f32>)>)>;

pub async fn elink(ctx: &Ctx, kind: Link, ids: &[u32]) -> Result<Links> {
    let (cmd, linkname) = match kind {
        Link::Related => ("neighbor_score", "pubmed_pubmed"),
        Link::CitedBy => ("neighbor", "pubmed_pubmed_citedin"),
        Link::Refs => ("neighbor", "pubmed_pubmed_refs"),
    };
    let ids = dedup(ids);
    let strs: Vec<String> = ids.iter().map(u32::to_string).collect();
    let bodies = try_join_all(strs.chunks(CHUNK).map(|c| {
        let mut p: Vec<(&str, &str)> =
            vec![("dbfrom", "pubmed"), ("db", "pubmed"), ("cmd", cmd), ("linkname", linkname), ("retmode", "json")];
        p.extend(c.iter().map(|i| ("id", i.as_str())));
        async move { ctx.http.eutils("elink.fcgi", &p).await }
    }))
    .await?;
    let mut got: HashMap<u32, Vec<(u32, Option<f32>)>> = HashMap::with_capacity(ids.len());
    for b in bodies {
        got.extend(parse_elink(b.as_bytes(), linkname)?);
    }
    Ok(ids.iter().map(|id| (*id, got.remove(id).unwrap_or_default())).collect())
}

fn bare_doi(s: &str) -> &str {
    for p in ["https://doi.org/", "http://doi.org/", "https://dx.doi.org/", "http://dx.doi.org/"] {
        if let Some(r) = s.strip_prefix(p) {
            return r;
        }
    }
    s
}

fn strip_prefix_ci<'a>(s: &'a str, p: &str) -> Option<&'a str> {
    s.get(..p.len()).filter(|h| h.eq_ignore_ascii_case(p)).map(|_| &s[p.len()..])
}

enum Tok {
    Pmid(u32),
    Aid(String),
}

fn classify(raw: &str) -> Result<Tok> {
    let t = raw.trim();
    let bad = || anyhow!("bad id '{t}'");
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let pmc = |s: &str| match strip_prefix_ci(s, "PMC") {
        Some(d) if digits(d) => Ok(Tok::Aid(format!("PMC{d}"))),
        _ if digits(s) => Ok(Tok::Aid(format!("PMC{s}"))),
        _ => Err(bad()),
    };
    if let Some(r) = strip_prefix_ci(t, "pmid:") {
        let r = r.trim();
        return r.parse().map(Tok::Pmid).map_err(|_| bad());
    }
    if let Some(r) = strip_prefix_ci(t, "pmc:") {
        return pmc(r.trim());
    }
    if let Some(r) = strip_prefix_ci(t, "doi:") {
        let r = bare_doi(r.trim());
        return if r.starts_with("10.") { Ok(Tok::Aid(r.to_string())) } else { Err(bad()) };
    }
    if digits(t) {
        return t.parse().map(Tok::Pmid).map_err(|_| bad());
    }
    if strip_prefix_ci(t, "PMC").is_some_and(digits) {
        return pmc(t);
    }
    let d = bare_doi(t);
    if d.starts_with("10.") && d.contains('/') {
        return Ok(Tok::Aid(d.to_string()));
    }
    Err(bad())
}

async fn lookup(ctx: &Ctx, tok: &str, aid: &str) -> Result<u32> {
    let term = format!("\"{}\"[AID]", aid.replace('"', ""));
    let o = SearchOpts { retmax: 1, ..Default::default() };
    match esearch(ctx, &term, &o).await?.ids.first() {
        Some(&id) => Ok(id),
        None => bail!("not found: {}", tok.trim()),
    }
}

pub async fn resolve(ctx: &Ctx, toks: &[String]) -> Result<Vec<u32>> {
    let parsed = toks.iter().map(|t| classify(t)).collect::<Result<Vec<_>>>()?;
    let ids = try_join_all(parsed.into_iter().zip(toks).map(|(p, t)| async move {
        match p {
            Tok::Pmid(id) => Ok(id),
            Tok::Aid(a) => lookup(ctx, t, &a).await,
        }
    }))
    .await?;
    Ok(dedup(&ids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Config;

    #[test]
    fn classify_tokens() {
        let aid = |s: &str| match classify(s).unwrap() {
            Tok::Aid(a) => a,
            Tok::Pmid(p) => format!("pmid {p}"),
        };
        assert_eq!(aid("123"), "pmid 123");
        assert_eq!(aid("PMID:456"), "pmid 456");
        assert_eq!(aid("pmc:PMC789"), "PMC789");
        assert_eq!(aid("pmc:789"), "PMC789");
        assert_eq!(aid("pmc123"), "PMC123");
        assert_eq!(aid("doi:10.1056/NEJMoa2001017"), "10.1056/NEJMoa2001017");
        assert_eq!(aid("10.1056/NEJMoa2001017"), "10.1056/NEJMoa2001017");
        assert_eq!(aid("https://doi.org/10.1/x"), "10.1/x");
        assert!(classify("hello").is_err());
        assert!(classify("pmid:abc").is_err());
        assert!(classify("doi:foo").is_err());
        assert_eq!(classify("hello").err().unwrap().to_string(), "bad id 'hello'");
    }

    #[test]
    fn search_parsing() {
        let ok = br#"{"esearchresult":{"count":"35","retmax":"2","idlist":["3","1"],"querytranslation":"x[All Fields]","errorlist":{"phrasesnotfound":["chapter"],"fieldsnotfound":["zz"]}}}"#;
        let r = parse_search(ok).unwrap();
        assert_eq!((r.count, r.ids.as_slice()), (35, &[3, 1][..]));
        assert_eq!(r.translation, "x[All Fields] [not found: chapter, zz]");
        let bad = br#"{"esearchresult":{"ERROR":"Search Backend failed: x. Details: Empty Term in the request"}}"#;
        assert_eq!(parse_search(bad).unwrap_err().to_string(), "bad query: Empty Term in the request");
        let rate = br#"{"error":"API rate limit exceeded","count":"4"}"#;
        assert_eq!(parse_search(rate).unwrap_err().to_string(), "esearch: API rate limit exceeded");
        assert_eq!(parse_search(b"<html>").unwrap_err().to_string(), "bad esearch response");
    }

    #[test]
    fn elink_parsing() {
        let scored = br#"{"linksets":[{"ids":["10"],"linksetdbs":[{"linkname":"pubmed_pubmed","links":[{"id":"10","score":99},{"id":"11","score":5}]}]},{"ids":["12"]}]}"#;
        let v = parse_elink(scored, "pubmed_pubmed").unwrap();
        assert_eq!(v, vec![(10, vec![(11, Some(5.0))]), (12, vec![])]);
        let plain = br#"{"linksets":[{"ids":["10"],"linksetdbs":[{"linkname":"pubmed_pubmed_citedin","links":["7","8"]}]}]}"#;
        assert_eq!(parse_elink(plain, "pubmed_pubmed_citedin").unwrap(), vec![(10, vec![(7, None), (8, None)])]);
        assert_eq!(parse_elink(plain, "pubmed_pubmed_refs").unwrap(), vec![(10, vec![])]);
        let err = br#"{"error":"backend unavailable"}"#;
        assert_eq!(parse_elink(err, "pubmed_pubmed").unwrap_err().to_string(), "elink: backend unavailable");
        let db_err = br#"{"linksets":[{"ids":["31978945"],"linksetdbs":[{"linkname":"pubmed_pubmed_refs","ERROR":"backend unavailable"}]}]}"#;
        assert_eq!(parse_elink(db_err, "pubmed_pubmed_refs").unwrap_err().to_string(), "elink: backend unavailable");
        let set_err = br#"{"linksets":[{"ids":["1"],"error":"set failed"}]}"#;
        assert_eq!(parse_elink(set_err, "pubmed_pubmed").unwrap_err().to_string(), "elink: set failed");
    }

    #[test]
    fn sort_names() {
        assert_eq!(sort_param("date"), "most_recent");
        assert_eq!(sort_param("pubdate"), "pub_date");
        assert_eq!(sort_param("relevance"), "relevance");
    }

    fn live() -> std::sync::Arc<Ctx> {
        let mut cfg = Config::from_env();
        cfg.no_cache = true;
        Ctx::new(cfg).unwrap()
    }

    #[tokio::test]
    #[ignore]
    async fn live_search_and_resolve() {
        let c = live();
        let r = esearch(&c, "retracted publication[pt]", &SearchOpts { retmax: 3, sort: Some("pubdate".into()), ..Default::default() })
            .await
            .unwrap();
        assert!(r.count > 1000 && r.ids.len() == 3, "{r:?}");
        let n = count(&c, "covid-19[ti] AND 2020[dp]", &SearchOpts::default()).await.unwrap();
        assert!(n > 10000);
        let r = esearch(&c, "chapter[pt] AND foo[xx]", &SearchOpts { retmax: 1, ..Default::default() }).await.unwrap();
        eprintln!("{}", r.translation);
        assert!(esearch(&c, "", &SearchOpts::default()).await.is_err());
        let ids = resolve(&c, &["31978945".into(), "doi:10.1056/NEJMoa2001017".into(), "PMC7092803".into(), "pmc:PMC7092803".into()])
            .await
            .unwrap();
        assert_eq!(ids, vec![31978945]);
        let e = resolve(&c, &["doi:10.9999/nope.nope".into()]).await.unwrap_err().to_string();
        assert_eq!(e, "not found: doi:10.9999/nope.nope");
    }

    #[tokio::test]
    #[ignore]
    async fn live_fetch_kinds() {
        let c = live();
        let ids = [31978945, 20301579, 26225414, 31978945, 999999999];
        let arts = fetch(&c, &ids).await.unwrap();
        assert_eq!(arts.iter().map(|a| a.pmid).collect::<Vec<_>>(), vec![31978945, 20301579, 26225414]);
        let a = &arts[0];
        eprintln!("{}", serde_json::to_string(a).unwrap().chars().take(3500).collect::<String>());
        eprintln!("{}", serde_json::to_string(&arts[1]).unwrap().chars().take(2500).collect::<String>());
        assert!(a.abs.len() == 1 && !a.corrs.is_empty() && a.doi == "10.1056/NEJMoa2001017" && a.pmc == "PMC7092803");
        assert!(a.authors.iter().any(|x| x.name.contains("Team")));
        let mut set = Set::from_ids(ids.to_vec());
        load(&c, &mut set).await.unwrap();
        assert_eq!(set.ids.len(), 3);
        assert_eq!(set.arts.len(), 3);
    }

    #[tokio::test]
    #[ignore]
    async fn live_variety() {
        let c = live();
        let mut ids = Vec::new();
        for q in ["retracted publication[pt]", "comment[pt] AND 2019[dp]", "erratum[pt] AND 2021[dp]", "randomized controlled trial[pt] AND 2023[dp] AND hasabstract", "1975[dp] AND journal article[pt]", "NCT[si] AND 2022[dp]"] {
            ids.extend(esearch(&c, q, &SearchOpts { retmax: 40, ..Default::default() }).await.unwrap().ids);
        }
        let arts = fetch(&c, &ids).await.unwrap();
        let n = |f: &dyn Fn(&Article) -> bool| arts.iter().filter(|a| f(a)).count();
        eprintln!(
            "n={} structured={} corrs={} problems={} retracted={} banks={} grants={} orcid={} coll={} eissn={} nodate={} noyear={} nodoi={} nopmc={} noabs={} mesh={} kw={}",
            arts.len(),
            n(&|a| a.abs.len() > 1),
            n(&|a| !a.corrs.is_empty()),
            n(&|a| a.has_problems()),
            n(&|a| a.is_retracted()),
            n(&|a| !a.banks.is_empty()),
            n(&|a| !a.grants.is_empty()),
            n(&|a| a.authors.iter().any(|x| !x.orcid.is_empty())),
            n(&|a| a.authors.iter().any(|x| x.fore.is_empty() && x.aff.is_empty() && x.name.split(' ').count() > 3)),
            n(&|a| !a.journal.eissn.is_empty()),
            n(&|a| a.date.is_empty()),
            n(&|a| a.year == 0),
            n(&|a| a.doi.is_empty()),
            n(&|a| a.pmc.is_empty()),
            n(&|a| a.abs.is_empty()),
            n(&|a| !a.mesh.is_empty()),
            n(&|a| !a.kw.is_empty()),
        );
        let odd: Vec<_> = arts.iter().filter(|a| a.date.len() != 4 && a.date.len() != 7 && a.date.len() != 10).map(|a| (a.pmid, a.date.clone())).collect();
        eprintln!("odd dates {odd:?}");
        let bad: Vec<_> = arts.iter().filter(|a| a.title.is_empty() || a.title.contains('<') || a.abs.iter().any(|(_, t)| t.contains('<'))).map(|a| a.pmid).collect();
        eprintln!("markup/empty titles {bad:?}");
        if let Some(a) = arts.iter().find(|a| a.is_retracted()) {
            eprintln!("{} {:?}", a.pmid, a.corrs);
        }
        if let Some(a) = arts.iter().find(|a| !a.banks.is_empty()) {
            eprintln!("{} {:?}", a.pmid, a.banks);
        }
        if let Some(a) = arts.iter().find(|a| a.abs.len() > 2) {
            eprintln!("{} {:?}", a.pmid, a.abs.iter().map(|(l, t)| (l.clone(), t.len())).collect::<Vec<_>>());
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_big_fetch_and_cache() {
        let dir = std::env::temp_dir().join(format!("pm-cache-test-{}", std::process::id()));
        let cfg = Config { cache_dir: dir.clone(), ..Config::from_env() };
        let c = Ctx::new(cfg).unwrap();
        let r = esearch(&c, "cancer[ti] AND 2022[dp]", &SearchOpts { retmax: 450, ..Default::default() }).await.unwrap();
        let t = std::time::Instant::now();
        let a1 = fetch(&c, &r.ids).await.unwrap();
        let cold = t.elapsed();
        let t = std::time::Instant::now();
        let a2 = fetch(&c, &r.ids).await.unwrap();
        eprintln!("cold {cold:?} warm {:?} n={}", t.elapsed(), a1.len());
        assert_eq!(a1.len(), a2.len());
        assert!(a1.len() > 400);
        let x = c.http.eutils("efetch.fcgi", &[("db", "pubmed"), ("id", &r.ids[..200].iter().map(u32::to_string).collect::<Vec<_>>().join(",")), ("retmode", "xml"), ("rettype", "abstract")]).await.unwrap();
        let t = std::time::Instant::now();
        let v = xml::parse_articles(&x).unwrap();
        eprintln!("parse 200: {:?} ({} records, {} bytes)", t.elapsed(), v.len(), x.len());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    #[ignore]
    async fn live_elink() {
        let c = live();
        let rel = elink(&c, Link::Related, &[31978945, 20301579, 1]).await.unwrap();
        assert_eq!(rel.len(), 3);
        assert!(rel[0].1.len() > 50 && rel[0].1[0].1.is_some() && !rel[0].1.iter().any(|(i, _)| *i == 31978945));
        let cb = elink(&c, Link::CitedBy, &[31978945, 1]).await.unwrap();
        assert!(cb[0].1.len() > 1000 && cb[0].1[0].1.is_none());
        let rf = elink(&c, Link::Refs, &[31978945]).await.unwrap();
        assert!(rf[0].1.len() > 5);
    }
}
