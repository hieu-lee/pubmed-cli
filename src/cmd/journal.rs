use crate::args::Args;
use crate::http::Ctx;
use crate::journals::{self, Entry};
use crate::model::*;
use anyhow::{Result, bail};

const HEADER: [&str; 6] = ["issn", "title", "q", "sjr", "h", "area"];

fn row(e: &Entry, best: (&str, u8)) -> Vec<String> {
    vec![e.print_issn().to_string(), e.title.to_string(), format!("Q{}", best.1), format!("{:.3}", e.sjr), e.h.to_string(), best.0.to_string()]
}

pub async fn run(_ctx: &Ctx, a: &Args, _input: Option<Set>) -> Result<Set> {
    exec(a)
}

fn exec(a: &Args) -> Result<Set> {
    if a.flag("update") {
        return Ok(Set::text(
            "journal data: SCImago SJR 2025 embedded (data/sjr.tsv.gz). Refresh: python3 scripts/build_sjr.py --year <Y> --sjr <sjr_journals.parquet> --nlm <J_Entrez.txt>, then rebuild.",
        ));
    }
    if a.flag("areas") {
        let n = a.num::<usize>("limit")?.unwrap_or(40);
        return Ok(Set::text(journals::areas().into_iter().take(n).collect::<Vec<_>>().join("; ")));
    }
    let limit = a.num::<usize>("limit")?.unwrap_or(5);
    let quartiles: Vec<u8> = a
        .csv("quartile")
        .iter()
        .map(|q| q.trim_start_matches(['Q', 'q']).parse::<u8>().ok().filter(|q| (1..=4).contains(q)).ok_or_else(|| anyhow::anyhow!("--quartile: bad '{q}'")))
        .collect::<Result<_>>()?;
    let query = a.first_pos_or_all();
    let query = query.trim();
    let area = a.get("area");
    if query.is_empty() && area.is_none() {
        bail!("usage: pm journal <name|issn> [-n 5] [--area X] [-q 1,2] | --areas | --update");
    }
    let rows: Vec<_> = journals::search(query, area, &quartiles, limit).into_iter().map(|(e, best)| row(&e, best)).collect();
    if rows.is_empty() {
        bail!("no journal match");
    }
    Ok(Set::table(&HEADER, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(line: &str) -> Result<Set> {
        exec(&Args::parse(&crate::args::split_words(line)?)?)
    }

    fn rows(line: &str) -> Vec<Vec<String>> {
        run(line).unwrap().table.unwrap().rows
    }

    #[test]
    fn issn_gives_single_row() {
        let r = rows("0028-4793");
        assert_eq!(r.len(), 1);
        assert_eq!(&r[0][..3], ["00284793", "New England Journal of Medicine", "Q1"]);
    }

    #[test]
    fn name_search_respects_limit() {
        let r = rows("lancet -n 3");
        assert_eq!(r.len(), 3);
        assert_eq!(r[0][1], "The Lancet");
    }

    #[test]
    fn area_and_quartile_filters() {
        let r = rows("--area Oncology -q 1 -n 4");
        assert_eq!(r.len(), 4);
        assert!(r.iter().all(|x| x[2] == "Q1" && x[5] == "Oncology"));
        assert!(rows("lancet --area oncology -q 1").iter().all(|x| x[5] == "Oncology"));
    }

    #[test]
    fn areas_update_and_errors() {
        assert!(run("--areas -n 3").unwrap().text.unwrap().contains("; "));
        assert!(run("--update").unwrap().text.unwrap().contains("build_sjr.py"));
        assert!(run("").is_err());
        assert!(run("zzzzqqqq").is_err());
        assert!(run("lancet -q 9").is_err());
    }
}
