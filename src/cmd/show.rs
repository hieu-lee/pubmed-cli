use crate::args::Args;
use crate::eutils;
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, bail};

const SHOW_FIELDS: &str = "pmid,year,journal,authors,title,abs";
const METRIC_FIELDS: &str = "pmid,year,journal,cites,rcr,pct,apt,clin,title";

pub fn cmd_name(a: &Args) -> Option<&str> {
    a.flags.iter().find_map(|f| f.strip_prefix("cmd:"))
}

pub fn fields(s: &str) -> Vec<String> {
    s.split(',').map(String::from).collect()
}

pub async fn ids(ctx: &Ctx, a: &Args, input: Option<&Set>) -> Result<Vec<u32>> {
    let mut ids = if a.pos.is_empty() {
        match input {
            Some(s) if s.ids.is_empty() && (s.table.is_some() || s.text.is_some()) => bail!("input has no PMIDs"),
            Some(s) => s.ids.clone(),
            None => bail!("no ids"),
        }
    } else {
        eutils::resolve(ctx, &a.pos).await?
    };
    let mut seen = std::collections::HashSet::new();
    ids.retain(|i| seen.insert(*i));
    Ok(ids)
}

fn reset_for_load(mut set: Set, ids: Vec<u32>) -> Set {
    set.ids = ids;
    set.arts.clear();
    set.table = None;
    set.text = None;
    set
}

pub async fn loaded(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Set> {
    if a.pos.is_empty()
        && let Some(s) = input {
            if s.arts.len() == s.ids.len() && s.table.is_none() && s.text.is_none() {
                return Ok(s);
            }
            let new_ids = ids(ctx, a, Some(&s)).await?;
            let mut set = reset_for_load(s, new_ids);
            eutils::load(ctx, &mut set).await?;
            return Ok(set);
        }
    let mut set = Set::from_ids(ids(ctx, a, None).await?);
    eutils::load(ctx, &mut set).await?;
    Ok(set)
}

pub async fn run(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Set> {
    let mut set = loaded(ctx, a, input).await?;
    set.fields = Some(fields(if cmd_name(a) == Some("metrics") { METRIC_FIELDS } else { SHOW_FIELDS }));
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{Config, Ctx};

    fn args(pos: &[&str]) -> Args {
        Args { pos: pos.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn lazy_load_keeps_scores_notes_and_total() {
        let mut s = Set::from_ids(vec![7, 8]);
        s.scores.insert(7, 2.5);
        s.notes.push("src=1".into());
        s.total = Some(9);
        s.text = Some("x".into());
        let out = reset_for_load(s, vec![7]);
        assert_eq!((out.total, out.notes.clone(), out.scores.get(&7).copied()), (Some(9), vec!["src=1".to_string()], Some(2.5)));
        assert_eq!(out.ids, [7]);
        assert!(out.text.is_none() && out.table.is_none() && out.arts.is_empty());
    }

    #[tokio::test]
    async fn empty_upstream_set_is_ok_but_missing_input_is_an_error() {
        let ctx = Ctx::new(Config::default()).unwrap();
        let empty = Set::default();
        assert!(ids(&ctx, &args(&[]), Some(&empty)).await.unwrap().is_empty());
        assert_eq!(ids(&ctx, &args(&[]), None).await.unwrap_err().to_string(), "no ids");
        let set = run(&ctx, &args(&[]), Some(empty)).await.unwrap();
        assert!(set.ids.is_empty());
    }
}
