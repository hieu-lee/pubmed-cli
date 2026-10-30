//! Shared modules get the invoked command as a flag `cmd:<name>` (canonical, never an alias):
//! links: cmd:related|citedby|refs; integrity: cmd:comments|problems|exam; filter.rs: cmd:filter|sort|top.
//! count.rs also reads flags `explain` / `by-year`; metrics = show with default fields.
pub mod author;
pub mod batch;
pub mod cache;
pub mod cite;
pub mod count;
pub mod facet;
pub mod filter;
pub mod fulltext;
pub mod id;
pub mod integrity;
pub mod journal;
pub mod links;
pub mod mesh;
pub mod search;
pub mod show;

use crate::args::Args;
use crate::help;
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, anyhow, bail};

pub const NAMES: &[&str] = &[
    "search", "s", "count", "c", "explain", "trend", "show", "get", "id", "related", "rel", "citedby", "cb",
    "refs", "comments", "problems", "exam", "fulltext", "ft", "cite", "mesh", "author", "journal", "j",
    "filter", "sort", "top", "head", "facet", "metrics", "cache", "batch", "run", "help",
];

pub fn is_command(name: &str) -> bool {
    NAMES.contains(&name)
}

pub(crate) fn lev(a: &str, b: &str) -> usize {
    let b = b.as_bytes();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.bytes().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let up = row[j + 1];
            row[j + 1] = (diag + usize::from(ca != cb)).min(up + 1).min(row[j] + 1);
            diag = up;
        }
    }
    row[b.len()]
}

pub fn suggest(name: &str) -> Option<&'static str> {
    let max = if name.len() >= 8 { 2 } else { 1 };
    NAMES
        .iter()
        .filter(|n| n.len() >= 3)
        .map(|n| (lev(name, n), *n))
        .filter(|(d, _)| *d <= max)
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| n)
}

pub fn unknown(name: &str) -> anyhow::Error {
    if name.starts_with('-') {
        return anyhow!("unknown command (pm help cmds)");
    }
    match suggest(name) {
        Some(s) => anyhow!("unknown command '{name}' (did you mean '{s}'?)"),
        None => anyhow!("unknown command '{name}' (pm help cmds)"),
    }
}

fn tagged(a: &Args, flag: &str) -> Args {
    let mut b = a.clone();
    b.flags.push(flag.to_string());
    b
}

fn via(a: &Args, name: &str) -> Args {
    tagged(a, &format!("cmd:{name}"))
}

pub async fn dispatch(ctx: &Ctx, name: &str, a: &Args, input: Option<Set>) -> Result<Set> {
    if name != "help" && a.flag("help") {
        return Ok(Set::text(help::text(Some(name))));
    }
    match name {
        "search" | "s" => search::run(ctx, a, input).await,
        "count" | "c" => count::run(ctx, a, input).await,
        "explain" => count::explain(ctx, a, input).await,
        "trend" => count::trend(ctx, a, input).await,
        "show" | "get" => show::run(ctx, a, input).await,
        "metrics" => show::run(ctx, &via(a, "metrics"), input).await,
        "id" => id::run(ctx, a, input).await,
        "related" | "rel" => links::run(ctx, &via(a, "related"), input).await,
        "citedby" | "cb" => links::run(ctx, &via(a, "citedby"), input).await,
        "refs" => links::run(ctx, &via(a, "refs"), input).await,
        "comments" | "problems" | "exam" => integrity::run(ctx, &via(a, name), input).await,
        "fulltext" | "ft" => fulltext::run(ctx, a, input).await,
        "cite" => cite::run(ctx, a, input).await,
        "mesh" => mesh::run(ctx, a, input).await,
        "author" => author::run(ctx, a, input).await,
        "journal" | "j" => journal::run(ctx, a, input).await,
        "facet" => facet::run(ctx, a, input).await,
        "filter" => filter::run(ctx, a, input).await,
        "sort" => filter::sort(ctx, a, input).await,
        "top" | "head" => filter::top(ctx, a, input).await,
        "cache" => cache::run(ctx, a).await,
        "batch" | "run" => batch::run(ctx, a).await,
        "help" => Ok(Set::text(help::text(a.pos.first().map(String::as_str)))),
        _ => bail!(unknown(name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance() {
        assert_eq!(lev("kitten", "sitting"), 3);
        assert_eq!(lev("", "abc"), 3);
        assert_eq!(lev("abc", "abc"), 0);
    }

    #[test]
    fn suggestions() {
        assert_eq!(suggest("serch"), Some("search"));
        assert_eq!(suggest("citedbi"), Some("citedby"));
        assert_eq!(suggest("crispr"), None);
        assert_eq!(suggest("cancer"), None);
        assert_eq!(suggest("gene"), None);
    }

    #[test]
    fn unknown_message() {
        assert_eq!(unknown("serch").to_string(), "unknown command 'serch' (did you mean 'search'?)");
        assert_eq!(unknown("zzzzzz").to_string(), "unknown command 'zzzzzz' (pm help cmds)");
    }

    #[test]
    fn every_name_is_known() {
        for n in ["search", "ft", "cb", "head", "metrics", "batch"] {
            assert!(is_command(n));
        }
        assert!(!is_command("crispr"));
    }
}
