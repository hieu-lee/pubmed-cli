use super::show::{cmd_name, fields, loaded};
use crate::args::Args;
use crate::eutils;
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, bail};
use std::collections::{HashMap, HashSet};

const EXAM_FIELDS: &str =
    "pmid,title,authors,aff,journal,q,year,doi,pmc,types,abs,mesh,kw,grants,nct,comments,problems,cites,rcr,nrefs";
const EXPAND_CAP: usize = 60;

fn retain_arts(set: &mut Set, keep: impl Fn(&Article) -> bool) {
    set.arts.retain(|a| keep(a));
    let kept: HashSet<u32> = set.arts.iter().map(|a| a.pmid).collect();
    set.ids.retain(|i| kept.contains(i));
}

pub async fn expand_corrs(ctx: &Ctx, arts: &mut [Article], want: impl Fn(&Corr) -> bool) -> Result<()> {
    let mut seen = HashSet::new();
    let need: Vec<u32> = arts
        .iter()
        .flat_map(|a| &a.corrs)
        .filter(|c| want(c) && c.title.is_empty())
        .filter_map(|c| c.pmid)
        .filter(|p| seen.insert(*p))
        .take(EXPAND_CAP)
        .collect();
    if need.is_empty() {
        return Ok(());
    }
    let titles: HashMap<u32, String> = eutils::fetch(ctx, &need)
        .await?
        .into_iter()
        .map(|l| {
            let t = if l.year > 0 && !l.journal.iso.is_empty() {
                format!("{} ({} {})", l.title, l.journal.iso, l.year)
            } else {
                l.title
            };
            (l.pmid, t)
        })
        .collect();
    for c in arts.iter_mut().flat_map(|a| a.corrs.iter_mut()).filter(|c| want(c)) {
        if let Some(t) = c.pmid.and_then(|p| titles.get(&p)) {
            c.title.clone_from(t);
        }
    }
    Ok(())
}

pub async fn run(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Set> {
    let name = cmd_name(a).unwrap_or("");
    let mut set = loaded(ctx, a, input).await?;
    let total;
    match name {
        "comments" => {
            retain_arts(&mut set, |x| x.corrs.iter().any(Corr::is_comment));
            total = set.arts.len();
            page(&mut set, a)?;
            if a.flag("expand") {
                expand_corrs(ctx, &mut set.arts, Corr::is_comment).await?;
            }
            set.fields = Some(fields("pmid,title,comments"));
        }
        "problems" => {
            retain_arts(&mut set, Article::has_problems);
            total = set.arts.len();
            page(&mut set, a)?;
            if a.flag("expand") {
                expand_corrs(ctx, &mut set.arts, Corr::is_problem).await?;
            }
            let retracted = set.arts.iter().filter(|x| x.is_retracted()).count();
            if retracted > 0 {
                set.notes.push(format!("retracted={retracted}"));
            }
            set.fields = Some(fields("pmid,title,problems"));
        }
        "exam" => {
            total = set.arts.len();
            page(&mut set, a)?;
            expand_corrs(ctx, &mut set.arts, |_| true).await?;
            set.notes.push("full".into());
            set.fields = Some(fields(EXAM_FIELDS));
        }
        _ => bail!("integrity: unknown command"),
    }
    set.total = Some(total as u64);
    Ok(set)
}

fn page(set: &mut Set, a: &Args) -> Result<()> {
    let skip = a.num::<usize>("skip")?.or(a.num("offset")?).unwrap_or(0);
    let limit = a.num::<usize>("limit")?.filter(|&n| n > 0).unwrap_or(usize::MAX);
    set.arts = std::mem::take(&mut set.arts).into_iter().skip(skip).take(limit).collect();
    set.ids = set.arts.iter().map(|x| x.pmid).collect();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn art(pmid: u32, kinds: &[&str]) -> Article {
        Article {
            pmid,
            corrs: kinds.iter().map(|k| Corr { kind: (*k).into(), pmid: Some(1), ..Default::default() }).collect(),
            ..Default::default()
        }
    }

    fn args(s: &str) -> Args {
        Args::parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn limit_and_skip_apply_after_retaining_matches() {
        let mut set = Set::from_arts(vec![art(1, &[]), art(2, &["CommentIn"]), art(3, &["CommentIn"]), art(4, &["CommentIn"])]);
        retain_arts(&mut set, |x| x.corrs.iter().any(Corr::is_comment));
        assert_eq!(set.arts.len(), 3);
        page(&mut set, &args("-n 1 --skip 1")).unwrap();
        assert_eq!(set.ids, vec![3]);
        assert_eq!(set.arts.iter().map(|x| x.pmid).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn no_limit_keeps_everything_and_zero_means_all() {
        let mut set = Set::from_arts(vec![art(1, &[]), art(2, &[])]);
        page(&mut set, &args("-n 0")).unwrap();
        assert_eq!(set.ids, vec![1, 2]);
    }
}
