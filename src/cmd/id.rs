use crate::args::Args;
use crate::eutils;
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, bail};
use std::collections::HashMap;

pub const IDCONV: &str = "https://pmc.ncbi.nlm.nih.gov/tools/idconv/api/v1/articles/";

#[derive(Debug, PartialEq)]
enum Tok {
    Pmid(u32),
    Pmc(String),
    Doi(String),
    Mid(String),
}

#[derive(Debug, Default, Clone)]
pub struct IdRec {
    pub pmid: Option<u32>,
    pub doi: String,
    pub pmc: String,
}

fn parse_tok(t: &str) -> Result<Tok> {
    let t = t.trim();
    let (pre, rest) = match t.split_once(':') {
        Some((p, r)) if matches!(p.to_ascii_lowercase().as_str(), "pmid" | "pmc" | "pmcid" | "doi" | "mid") => {
            (p.to_ascii_lowercase(), r.trim())
        }
        _ => (String::new(), t),
    };
    let digits = !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    let upper = rest.to_ascii_uppercase();
    Ok(match pre.as_str() {
        "doi" => Tok::Doi(rest.to_string()),
        "mid" => Tok::Mid(upper),
        "pmc" | "pmcid" if digits => Tok::Pmc(format!("PMC{rest}")),
        "pmc" | "pmcid" => Tok::Pmc(upper),
        _ if digits => Tok::Pmid(rest.parse()?),
        _ if upper.starts_with("PMC") && upper[3..].bytes().all(|b| b.is_ascii_digit()) && upper.len() > 3 => Tok::Pmc(upper),
        _ if upper.starts_with("NIHMS") => Tok::Mid(upper),
        _ if rest.starts_with("10.") => Tok::Doi(rest.to_string()),
        _ => bail!("id: bad id '{t}'"),
    })
}

fn records(v: &serde_json::Value) -> HashMap<String, IdRec> {
    let mut out = HashMap::new();
    for r in v["records"].as_array().into_iter().flatten() {
        let Some(req) = r["requested-id"].as_str() else { continue };
        if r["status"] == "error" {
            continue;
        }
        let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
        out.insert(req.to_ascii_uppercase(), IdRec { pmid: r["pmid"].as_u64().map(|n| n as u32), doi: s("doi"), pmc: s("pmcid") });
    }
    out
}

pub async fn idconv(ctx: &Ctx, idtype: &str, ids: &[String]) -> Result<HashMap<String, IdRec>> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(200) {
        let joined = chunk.join(",");
        let mut p = vec![("ids", joined.as_str()), ("idtype", idtype), ("format", "json"), ("tool", "pm-cli")];
        if let Some(e) = &ctx.cfg().email {
            p.push(("email", e));
        }
        let body = ctx.http.get(IDCONV, &p).await?;
        let v: serde_json::Value = serde_json::from_str(&body)?;
        if v["status"] == "error" {
            bail!("idconv: {}", v["errors"][0]["message"].as_str().unwrap_or("error"));
        }
        out.extend(records(&v));
    }
    Ok(out)
}

pub async fn pmc_of(ctx: &Ctx, pmids: &[u32]) -> Result<HashMap<u32, String>> {
    let ids: Vec<String> = pmids.iter().map(u32::to_string).collect();
    Ok(idconv(ctx, "pmid", &ids)
        .await?
        .into_iter()
        .filter_map(|(k, r)| Some((k.parse().ok()?, r.pmc)).filter(|(_, p): &(u32, String)| !p.is_empty()))
        .collect())
}

fn cell(r: &IdRec) -> Vec<String> {
    vec![r.pmid.map(|p| p.to_string()).unwrap_or_default(), r.doi.clone(), r.pmc.clone()]
}

fn build_rows(
    toks: &[Tok],
    loaded: &[Article],
    pmc_map: &HashMap<String, IdRec>,
    mid_map: &HashMap<String, IdRec>,
    doi_map: &HashMap<&str, Option<u32>>,
) -> (Vec<Vec<String>>, Vec<String>) {
    let arts: HashMap<u32, &Article> = loaded.iter().map(|x| (x.pmid, x)).collect();
    let from_art = |p: u32, doi: &str| match arts.get(&p) {
        Some(x) => IdRec { pmid: Some(p), doi: x.doi.clone(), pmc: x.pmc.clone() },
        None => IdRec { pmid: Some(p), doi: doi.to_string(), pmc: String::new() },
    };
    let mut missing: Vec<String> = Vec::new();
    let rows = toks
        .iter()
        .map(|t| {
            cell(&match t {
                Tok::Pmid(p) if arts.contains_key(p) => from_art(*p, ""),
                Tok::Pmid(p) => {
                    missing.push(p.to_string());
                    IdRec { pmid: Some(*p), ..Default::default() }
                }
                Tok::Doi(d) => match doi_map[d.as_str()] {
                    Some(p) => from_art(p, d),
                    None => {
                        missing.push(d.clone());
                        IdRec { doi: d.clone(), ..Default::default() }
                    }
                },
                Tok::Pmc(s) => pmc_map.get(s).cloned().unwrap_or_else(|| {
                    missing.push(s.clone());
                    IdRec { pmc: s.clone(), ..Default::default() }
                }),
                Tok::Mid(s) => mid_map.get(s).cloned().unwrap_or_else(|| {
                    missing.push(s.clone());
                    IdRec::default()
                }),
            })
        })
        .collect();
    (rows, missing)
}

pub async fn run(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Set> {
    let toks: Vec<Tok> = if a.pos.is_empty() {
        match input {
            Some(s) if !s.ids.is_empty() => s.ids.iter().map(|&i| Tok::Pmid(i)).collect(),
            Some(s) if s.table.is_none() && s.text.is_none() => return Ok(Set::table(&["pmid", "doi", "pmc"], Vec::new())),
            _ => bail!("no ids"),
        }
    } else {
        a.pos.iter().map(|t| parse_tok(t)).collect::<Result<_>>()?
    };

    let pick = |f: fn(&Tok) -> Option<&String>| toks.iter().filter_map(f).cloned().collect::<Vec<String>>();
    let pmcs = pick(|t| if let Tok::Pmc(s) = t { Some(s) } else { None });
    let mids = pick(|t| if let Tok::Mid(s) = t { Some(s) } else { None });
    let dois = pick(|t| if let Tok::Doi(s) = t { Some(s) } else { None });

    let (pmc_map, mid_map, doi_ids) = futures::try_join!(
        async { if pmcs.is_empty() { Ok(HashMap::new()) } else { idconv(ctx, "pmcid", &pmcs).await } },
        async { if mids.is_empty() { Ok(HashMap::new()) } else { idconv(ctx, "mid", &mids).await } },
        futures::future::try_join_all(dois.iter().map(|d| {
            let t = [format!("doi:{d}")];
            async move {
                match eutils::resolve(ctx, &t).await {
                    Ok(v) => Ok(v.first().copied()),
                    Err(e) if e.to_string().starts_with("not found:") => Ok(None),
                    Err(e) => Err(e),
                }
            }
        })),
    )?;
    let doi_map: HashMap<&str, Option<u32>> = dois.iter().map(String::as_str).zip(doi_ids).collect();

    let mut set = Set::from_ids(
        toks.iter()
            .filter_map(|t| match t {
                Tok::Pmid(p) => Some(*p),
                Tok::Doi(d) => doi_map[d.as_str()],
                _ => None,
            })
            .collect(),
    );
    if !set.ids.is_empty() {
        eutils::load(ctx, &mut set).await?;
    }
    let (rows, missing) = build_rows(&toks, &set.arts, &pmc_map, &mid_map, &doi_map);
    if !missing.is_empty() && missing.len() == rows.len() {
        bail!("id: not found: {}", missing.join(", "));
    }
    let mut set = Set::table(&["pmid", "doi", "pmc"], rows);
    if !missing.is_empty() {
        set.notes.push(format!("not found: {}", missing.join(", ")));
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_upstream_set_gives_empty_table() {
        let ctx = Ctx::new(crate::http::Config::default()).unwrap();
        let s = run(&ctx, &Args::default(), Some(Set::default())).await.unwrap();
        assert!(s.table.unwrap().rows.is_empty());
        assert!(run(&ctx, &Args::default(), None).await.is_err());
    }

    #[test]
    fn failed_records_are_not_mapped() {
        let v = serde_json::json!({"records": [
            {"requested-id": "PMC1", "pmid": 5, "pmcid": "PMC1", "doi": "10.1/x"},
            {"requested-id": "PMC2", "status": "error", "errmsg": "invalid article id"},
        ]});
        let m = records(&v);
        assert_eq!(m.len(), 1);
        assert_eq!(m["PMC1"].pmid, Some(5));
        assert!(!m.contains_key("PMC2"));
    }

    #[test]
    fn unreturned_pmids_are_reported_missing() {
        let art = Article { pmid: 5, doi: "10.1/x".into(), ..Default::default() };
        let toks = [Tok::Pmid(5), Tok::Pmid(999)];
        let (rows, missing) = build_rows(&toks, &[art], &HashMap::new(), &HashMap::new(), &HashMap::new());
        assert_eq!(missing, vec!["999".to_string()]);
        assert_eq!(rows[0], vec!["5", "10.1/x", ""]);
        assert_eq!(rows[1], vec!["999", "", ""]);
    }

    #[test]
    fn token_kinds() {
        assert_eq!(parse_tok("123").unwrap(), Tok::Pmid(123));
        assert_eq!(parse_tok("pmid:123").unwrap(), Tok::Pmid(123));
        assert_eq!(parse_tok("pmc:3531190").unwrap(), Tok::Pmc("PMC3531190".into()));
        assert_eq!(parse_tok("PMC3531190").unwrap(), Tok::Pmc("PMC3531190".into()));
        assert_eq!(parse_tok("doi:10.1/x").unwrap(), Tok::Doi("10.1/x".into()));
        assert_eq!(parse_tok("10.1038/nature14539").unwrap(), Tok::Doi("10.1038/nature14539".into()));
        assert_eq!(parse_tok("nihms12345").unwrap(), Tok::Mid("NIHMS12345".into()));
        assert!(parse_tok("foo").is_err());
    }
}
