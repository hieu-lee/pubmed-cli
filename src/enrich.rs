pub mod icite;

use crate::eutils;
use crate::http::Ctx;
use crate::journals;
use crate::model::*;
use anyhow::Result;
use std::collections::{HashMap, HashSet};

const TITLE_MAX: usize = 120;

pub async fn apply(ctx: &Ctx, arts: &mut [Article], need: Need) -> Result<()> {
    if need.rank {
        for a in arts.iter_mut() {
            a.jr = journals::rank(a);
        }
    }
    let metric_ids: Vec<u32> = if need.metrics { arts.iter().map(|a| a.pmid).collect() } else { Vec::new() };
    let corr_ids = if need.expand { missing_corr_ids(arts) } else { Vec::new() };
    let (metrics, linked) = futures::try_join!(
        async {
            if metric_ids.is_empty() { Ok(HashMap::new()) } else { icite::metrics(ctx, &metric_ids).await }
        },
        async {
            if corr_ids.is_empty() { Ok(Vec::new()) } else { eutils::fetch(ctx, &corr_ids).await }
        }
    )?;
    if need.metrics {
        for a in arts.iter_mut() {
            a.m = Some(metrics.get(&a.pmid).cloned().unwrap_or_default());
        }
    }
    if need.expand {
        let mut titles: HashMap<u32, String> = HashMap::new();
        for a in arts.iter().chain(linked.iter()) {
            titles.entry(a.pmid).or_insert_with(|| corr_title(a));
        }
        for c in arts.iter_mut().flat_map(|a| a.corrs.iter_mut()) {
            if let Some(t) = c.pmid.and_then(|p| titles.get(&p)).filter(|_| c.title.is_empty()) {
                c.title = t.clone();
            }
        }
    }
    Ok(())
}

fn missing_corr_ids(arts: &[Article]) -> Vec<u32> {
    let have: HashSet<u32> = arts.iter().map(|a| a.pmid).collect();
    let mut seen = HashSet::new();
    arts.iter()
        .flat_map(|a| &a.corrs)
        .filter(|c| c.title.is_empty())
        .filter_map(|c| c.pmid)
        .filter(|p| !have.contains(p) && seen.insert(*p))
        .collect()
}

fn corr_title(a: &Article) -> String {
    let j = if a.journal.iso.is_empty() { &a.journal.title } else { &a.journal.iso };
    let mut s = String::new();
    if a.year != 0 {
        s.push_str(&a.year.to_string());
        s.push(' ');
    }
    if !j.is_empty() {
        s.push_str(j);
        s.push_str(": ");
    }
    s.push_str(a.title.trim_end_matches('.'));
    if s.chars().count() > TITLE_MAX {
        s = s.chars().take(TITLE_MAX - 1).collect::<String>().trim_end().to_string();
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn art(pmid: u32, year: u16, iso: &str, title: &str) -> Article {
        Article {
            pmid,
            year,
            title: title.into(),
            journal: JournalInfo { iso: iso.into(), ..Default::default() },
            ..Default::default()
        }
    }

    #[test]
    fn title_format() {
        assert_eq!(corr_title(&art(1, 2020, "Lancet", "Hello world.")), "2020 Lancet: Hello world");
        assert_eq!(corr_title(&art(1, 0, "", "Bare")), "Bare");
    }

    #[test]
    fn title_truncates() {
        let t = corr_title(&art(1, 2020, "Lancet", &"x".repeat(300)));
        assert_eq!(t.chars().count(), TITLE_MAX);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn missing_ids_dedup_and_skip_known() {
        let mut a = art(1, 2020, "J", "T");
        let corr = |p: u32, title: &str| Corr { kind: "CommentIn".into(), pmid: Some(p), note: String::new(), title: title.into() };
        a.corrs = vec![corr(2, ""), corr(2, ""), corr(3, "done"), corr(5, ""), Corr { pmid: None, ..Default::default() }];
        let b = art(5, 2019, "J", "T");
        assert_eq!(missing_corr_ids(&[a, b]), vec![2]);
    }
}
