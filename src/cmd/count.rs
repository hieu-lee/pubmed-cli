use crate::args::Args;
use crate::eutils::{self, SearchOpts};
use crate::filter::{Filters, today};
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, bail};
use futures::future::try_join_all;

const TYPES: &[&str] = &[
    "Randomized Controlled Trial",
    "Meta-Analysis",
    "Systematic Review",
    "Review",
    "Clinical Trial",
    "Observational Study",
    "Case Reports",
    "Practice Guideline",
    "Editorial",
    "Letter",
];

fn prepare(a: &Args) -> Result<(Filters, String)> {
    let f = Filters::from_args(a)?;
    let term = f.query(&a.pos.join(" "));
    if term.is_empty() {
        bail!("{}", f.empty_query_err());
    }
    if f.needs_post() {
        bail!("count: local-only filters ({}) need a scan; use search", f.local.join(","));
    }
    Ok((f, term))
}

pub async fn run(ctx: &Ctx, a: &Args, _input: Option<Set>) -> Result<Set> {
    match a.get("by") {
        Some("year") => by_year(ctx, a).await,
        Some("type") => by_type(ctx, a).await,
        Some(b) => bail!("count --by {b}? year|type"),
        None if a.flag("by-year") => by_year(ctx, a).await,
        None => {
            let (f, term) = prepare(a)?;
            let n = eutils::count(ctx, &term, &f.search_opts()).await?;
            Ok(Set::text(n.to_string()))
        }
    }
}

pub async fn trend(ctx: &Ctx, a: &Args, _input: Option<Set>) -> Result<Set> {
    by_year(ctx, a).await
}

async fn by_year(ctx: &Ctx, a: &Args) -> Result<Set> {
    let (f, term) = prepare(a)?;
    let (from, to) = f.year_span().unwrap_or_else(|| (today().y - 9, today().y));
    if to < from || to - from > 80 {
        bail!("trend: year span {from}-{to} invalid (max 80)");
    }
    let counts = try_join_all((from..=to).map(|y| {
        let o = f.year_opts(y);
        let term = &term;
        async move { eutils::count(ctx, term, &o).await }
    }))
    .await?;
    let rows = (from..=to).zip(counts).map(|(y, c)| vec![y.to_string(), c.to_string()]).collect();
    Ok(Set::table(&["year", "count"], rows))
}

async fn by_type(ctx: &Ctx, a: &Args) -> Result<Set> {
    let (f, term) = prepare(a)?;
    let opts = f.search_opts();
    let counts = try_join_all(TYPES.iter().map(|t| {
        let q = format!("({term}) AND \"{t}\"[pt]");
        let o = opts.clone();
        async move { eutils::count(ctx, &q, &o).await }
    }))
    .await?;
    let rows = TYPES.iter().zip(counts).map(|(t, c)| vec![t.to_string(), c.to_string()]).collect();
    Ok(Set::table(&["type", "count"], rows))
}

pub async fn explain(ctx: &Ctx, a: &Args, _input: Option<Set>) -> Result<Set> {
    let f = Filters::from_args(a)?;
    let term = f.query(&a.pos.join(" "));
    if term.is_empty() {
        bail!("{}", f.empty_query_err());
    }
    let mut o: SearchOpts = f.search_opts();
    o.retmax = 0;
    let r = eutils::esearch(ctx, &term, &o).await?;
    let mut out = format!("query: {term}\n");
    if let Some(d) = f.describe_dates() {
        out.push_str(&format!("dates: {d}\n"));
    }
    if let Some(s) = &o.sort {
        out.push_str(&format!("sort: {s}{}\n", f.local_sort().map(|k| format!(" then local {k}")).unwrap_or_default()));
    }
    if f.needs_post() {
        out.push_str(&format!("local: {} (scan {})\n", f.local.join(","), f.scan));
        if let Some(d) = f.describe_local() {
            out.push_str(&format!("match: {d}\n"));
        }
    }
    out.push_str(&format!("pubmed: {}\ncount: {}", r.translation, r.count));
    Ok(Set::text(out))
}
