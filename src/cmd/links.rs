use super::show::{cmd_name, fields, ids};
use crate::args::Args;
use crate::eutils::{self, Link};
use crate::filter::Filters;
use crate::ops;
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, bail};
use std::collections::{HashMap, HashSet};

const DEFAULT_LIMIT: usize = 10;

struct Hit {
    n: u32,
    score: f32,
}

fn union(links: &eutils::Links, sources: &HashSet<u32>, exclude_sources: bool) -> Vec<(u32, Hit)> {
    let mut pos: HashMap<u32, usize> = HashMap::new();
    let mut out: Vec<(u32, Hit)> = Vec::new();
    for (src, ls) in links {
        for &(id, sc) in ls {
            if id == *src || (exclude_sources && sources.contains(&id)) {
                continue;
            }
            let h = match pos.get(&id) {
                Some(&p) => &mut out[p].1,
                None => {
                    pos.insert(id, out.len());
                    out.push((id, Hit { n: 0, score: 0.0 }));
                    &mut out.last_mut().unwrap().1
                }
            };
            h.n += 1;
            h.score += sc.unwrap_or(0.0);
        }
    }
    out.sort_by(|a, b| b.1.n.cmp(&a.1.n).then(b.1.score.total_cmp(&a.1.score)));
    out
}

fn summary(src: usize, scanned: Option<(usize, usize)>, total: usize) -> (Option<u64>, String) {
    match scanned {
        Some((n, m)) => (None, format!("src={src} scanned={n}/{m}")),
        None => (Some(total as u64), format!("src={src} links={total}")),
    }
}

pub async fn run(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Set> {
    let kind = match cmd_name(a) {
        Some("related" | "rel") => Link::Related,
        Some("citedby" | "cb") => Link::CitedBy,
        Some("refs") => Link::Refs,
        _ => bail!("links: unknown command"),
    };
    if a.has("sort") {
        bail!("links: --sort unsupported; use -n 200 :: sort cites :: top 10");
    }
    let f = Filters::from_args(a)?;
    f.reject_server_only()?;
    let src = ids(ctx, a, input.as_ref()).await?;
    if src.is_empty() {
        return Ok(Set { total: Some(0), ..Default::default() });
    }
    let links = eutils::elink(ctx, kind, &src).await?;
    let sources: HashSet<u32> = src.iter().copied().collect();
    let mut hits = union(&links, &sources, kind == Link::Related);
    if let (Link::Related, Some(min)) = (kind, a.num::<f32>("min-score")?) {
        hits.retain(|(_, h)| h.score / h.n as f32 >= min);
    }
    let mut loaded: Option<Set> = None;
    let mut scanned: Option<(usize, usize)> = None;
    if !f.is_empty() {
        let scan: Vec<u32> = hits.iter().take(f.scan).map(|(id, _)| *id).collect();
        if scan.len() < hits.len() {
            scanned = Some((scan.len(), hits.len()));
        }
        let kept = ops::apply_filters(ctx, Set::from_ids(scan), &f).await?;
        let keep: HashSet<u32> = kept.ids.iter().copied().collect();
        hits.retain(|(id, _)| keep.contains(id));
        loaded = Some(kept);
    }
    let total = hits.len();
    let skip = a.num::<usize>("skip")?.or(a.num("offset")?).unwrap_or(0);
    let limit = match a.num::<usize>("limit")? {
        Some(0) => usize::MAX,
        Some(n) => n,
        None => DEFAULT_LIMIT,
    };
    let page: Vec<(u32, Hit)> = hits.into_iter().skip(skip).take(limit).collect();

    let multi = src.len() > 1;
    let mut set = Set::from_ids(page.iter().map(|p| p.0).collect());
    if kind == Link::Related || multi {
        let scored = kind == Link::Related;
        set.scores = page.iter().map(|(id, h)| (*id, if scored { h.score } else { h.n as f32 })).collect();
    }
    if let Some(kept) = loaded {
        let want: HashSet<u32> = set.ids.iter().copied().collect();
        set.arts = kept.arts.into_iter().filter(|x| want.contains(&x.pmid)).collect();
        let pos: HashMap<u32, usize> = set.ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        set.arts.sort_by_key(|x| pos[&x.pmid]);
    }
    if set.arts.is_empty() && !(a.get("fmt") == Some("ids") || crate::fields::is_pmid_only(&a.csv("fields"))) {
        eutils::load(ctx, &mut set).await?;
    }
    let (t, note) = summary(src.len(), scanned, total);
    set.total = t;
    set.notes.push(note);
    set.fields = Some(fields(if set.scores.is_empty() {
        "pmid,year,journal,authors,title"
    } else {
        "pmid,score,year,journal,authors,title"
    }));
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_scan_is_not_reported_as_exhaustive() {
        assert_eq!(summary(1, Some((1, 5)), 1), (None, "src=1 scanned=1/5".to_string()));
        assert_eq!(summary(2, None, 7), (Some(7), "src=2 links=7".to_string()));
    }

    fn links() -> eutils::Links {
        vec![(1, vec![(2, Some(5.0)), (3, Some(1.0)), (1, None)]), (2, vec![(1, None), (3, None)])]
    }

    #[test]
    fn refs_and_citedby_keep_other_sources() {
        let src: HashSet<u32> = [1, 2].into();
        let ids = |v: Vec<(u32, Hit)>| v.into_iter().map(|h| h.0).collect::<Vec<_>>();
        let mut keep = ids(union(&links(), &src, false));
        keep.sort();
        assert_eq!(keep, [1, 2, 3]);
        assert_eq!(ids(union(&links(), &src, true)), [3]);
    }
}
