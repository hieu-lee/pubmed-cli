use crate::args::Args;
use crate::filter::Filters;
use crate::http::Ctx;
use crate::model::*;
use crate::ops;
use anyhow::{Result, anyhow, bail};

pub async fn run(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Set> {
    let mut plain = a.clone();
    plain.opts.retain(|(k, _)| k != "sort");
    plain.flags.retain(|k| !matches!(k.as_str(), "asc" | "desc" | "reverse"));
    let f = Filters::from_args(&plain)?;
    let sort = a.get("sort").map(ops::norm_key).transpose()?;
    let set = ops::input_set(ctx, &a.pos, input).await?;
    let mut set = ops::apply_filters(ctx, set, &f).await?;
    if let Some(key) = sort {
        let asc = if a.flag("asc") {
            Some(true)
        } else if a.flag("desc") || a.flag("reverse") {
            Some(false)
        } else {
            None
        };
        ops::sort_loaded(ctx, &mut set, key, asc).await?;
    }
    Ok(set)
}

pub async fn sort(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Set> {
    let Some(key) = a.pos.first() else {
        bail!("sort: need a key ({})", ops::SORT_KEYS);
    };
    let key = ops::norm_key(key)?;
    let asc = if a.flag("asc") {
        Some(true)
    } else if a.flag("desc") || a.flag("reverse") {
        Some(false)
    } else {
        None
    };
    let mut set = ops::input_set(ctx, &a.pos[1..], input).await?;
    ops::sort_loaded(ctx, &mut set, key, asc).await?;
    Ok(set)
}

pub async fn top(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Set> {
    let (limit, skip) = ops::limit_skip(a)?;
    let (n, rest) = match a.pos.split_first() {
        Some((c, rest)) if !a.has("limit") => (c.parse::<usize>().map_err(|_| anyhow!("top: bad count '{c}'"))?, rest),
        _ => (limit, &a.pos[..]),
    };
    if input.is_some() && !rest.is_empty() {
        bail!("top: ids cannot be combined with piped input");
    }
    let mut set = ops::input_set(ctx, rest, input).await?;
    let aligned = set.arts.len() == set.ids.len();
    let end = skip.saturating_add(n).min(set.ids.len());
    let start = skip.min(end);
    set.ids.truncate(end);
    set.ids.drain(..start);
    if aligned {
        set.arts.truncate(end);
        set.arts.drain(..start);
    } else {
        set.arts.clear();
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Config;

    async fn top_ids(argv: &[&str], input: Option<Set>) -> Result<Vec<u32>> {
        let ctx = Ctx::new(Config::default())?;
        let toks: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        Ok(top(&ctx, &Args::parse(&toks)?, input).await?.ids)
    }

    #[tokio::test]
    async fn filter_applies_its_sort_option() {
        let ctx = Ctx::new(Config::default()).unwrap();
        let art = |pmid: u32, year: u16| Article { pmid, year, date: year.to_string(), title: "t".into(), ..Default::default() };
        let up = || Some(Set::from_arts(vec![art(1, 2019), art(2, 2025), art(3, 2021)]));
        let run_ids = |argv: &[&str]| {
            let toks: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
            let a = Args::parse(&toks).unwrap();
            let ctx = &ctx;
            async move { run(ctx, &a, up()).await.unwrap().ids }
        };
        assert_eq!(run_ids(&["--sort", "year"]).await, [2, 3, 1]);
        assert_eq!(run_ids(&["--sort", "year", "--asc"]).await, [1, 3, 2]);
        assert_eq!(run_ids(&["--year", "2020-2030", "--sort", "year", "--asc"]).await, [3, 2]);
        assert_eq!(run_ids(&[]).await, [1, 2, 3]);
    }

    #[tokio::test]
    async fn count_with_piped_input_is_never_an_id() {
        let up = || Some(Set::from_ids(vec![11, 12, 13]));
        assert_eq!(top_ids(&["1000000"], up()).await.unwrap(), [11, 12, 13]);
        assert_eq!(top_ids(&["2"], up()).await.unwrap(), [11, 12]);
        assert_eq!(top_ids(&["-n", "1"], up()).await.unwrap(), [11]);
        assert!(top_ids(&["1", "99"], up()).await.is_err());
        assert!(top_ids(&["x"], up()).await.is_err());
    }

    #[tokio::test]
    async fn without_input_first_positional_is_count_and_rest_are_ids() {
        assert_eq!(top_ids(&["2", "11", "12", "13"], None).await.unwrap(), [11, 12]);
        assert_eq!(top_ids(&["-n", "1", "11", "12"], None).await.unwrap(), [11]);
        assert!(top_ids(&[], None).await.is_err());
    }
}
