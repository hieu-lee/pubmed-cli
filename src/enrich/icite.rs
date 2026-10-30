use crate::http::Ctx;
use crate::model::Metrics;
use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

const URL: &str = "https://icite.od.nih.gov/api/pubs";
const FIELDS: &str = "pmid,citation_count,relative_citation_ratio,nih_percentile,citations_per_year,apt,cited_by_clin";
const TTL: Duration = Duration::from_secs(86_400);
const CHUNK: usize = 200;
const CONCURRENCY: usize = 3;

#[derive(Deserialize)]
struct Resp {
    data: Vec<Rec>,
}

#[derive(Deserialize)]
struct Rec {
    pmid: u32,
    citation_count: Option<u32>,
    relative_citation_ratio: Option<f32>,
    nih_percentile: Option<f32>,
    citations_per_year: Option<f32>,
    apt: Option<f32>,
    cited_by_clin: Option<Vec<u32>>,
}

fn parse(body: &str) -> Result<Vec<(u32, Metrics)>> {
    let r: Resp = serde_json::from_str(body).context("bad iCite response")?;
    Ok(r.data
        .into_iter()
        .map(|r| {
            let m = Metrics {
                cites: r.citation_count.unwrap_or(0),
                rcr: r.relative_citation_ratio,
                pct: r.nih_percentile,
                apt: r.apt,
                cpy: r.citations_per_year,
                clin: r.cited_by_clin.is_some_and(|c| !c.is_empty()),
            };
            (r.pmid, m)
        })
        .collect())
}

async fn fetch_chunk(ctx: &Ctx, ids: &[u32]) -> Result<Vec<(u32, Metrics)>> {
    let joined = ids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let body = ctx.http.get(URL, &[("pmids", &joined), ("format", "json"), ("fl", FIELDS)]).await?;
    let mut found = parse(&body)?;
    for id in ids {
        if !found.iter().any(|(p, _)| p == id) {
            found.push((*id, Metrics::default()));
        }
    }
    for (id, m) in &found {
        if let Ok(v) = serde_json::to_vec(m) {
            ctx.http.cache_put("icite", &id.to_string(), &v).await;
        }
    }
    Ok(found)
}

pub async fn metrics(ctx: &Ctx, ids: &[u32]) -> Result<HashMap<u32, Metrics>> {
    let cached = futures::future::join_all(ids.iter().map(|id| async move {
        let v = ctx.http.cache_get("icite", &id.to_string(), TTL).await?;
        serde_json::from_slice::<Metrics>(&v).ok().map(|m| (*id, m))
    }))
    .await;
    let mut out: HashMap<u32, Metrics> = cached.into_iter().flatten().collect();
    let todo: Vec<u32> = ids.iter().copied().filter(|i| !out.contains_key(i)).collect();
    let chunks: Vec<&[u32]> = todo.chunks(CHUNK).collect();
    let mut res = stream::iter(chunks).map(|c| fetch_chunk(ctx, c)).buffer_unordered(CONCURRENCY);
    while let Some(r) = res.next().await {
        out.extend(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_shape() {
        let body = r#"{"data":[{"_id":"21081","pmid":21081,"apt":0.05,"citation_count":30,"citations_per_year":0.6122448979591837,"relative_citation_ratio":0.9019824812436485,"nih_percentile":null,"cited_by_clin":null},{"_id":"40000000","pmid":40000000,"apt":0.75,"citation_count":2,"citations_per_year":2.0,"relative_citation_ratio":null,"nih_percentile":null,"cited_by_clin":[42046511]}]}"#;
        let v = parse(body).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, 21081);
        assert_eq!(v[0].1.cites, 30);
        assert!(!v[0].1.clin);
        assert!(v[0].1.pct.is_none());
        assert!((v[0].1.rcr.unwrap() - 0.902).abs() < 1e-3);
        assert!(v[1].1.clin);
        assert_eq!(v[1].1.apt, Some(0.75));
    }

    #[test]
    fn empty_data_and_garbage() {
        assert!(parse(r#"{"data":[]}"#).unwrap().is_empty());
        assert!(parse("<html>").is_err());
    }
}
