use crate::args::Args;
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, anyhow, bail};
use std::collections::HashSet;
use std::fmt::Write;

const SYN_CAP: usize = 12;
const SCOPE_CAP: usize = 300;
const TREE_CAP: usize = 4;
const NARROW_CAP: usize = 15;

#[derive(Debug, Default, PartialEq)]
struct Entry {
    name: String,
    supp: bool,
    ui: String,
    year: String,
    trees: Vec<String>,
    scope: String,
    syns: Vec<String>,
    pa: Vec<String>,
    maps: Vec<String>,
    up: Vec<String>,
    down: Vec<String>,
}

pub async fn run(ctx: &Ctx, a: &Args, _input: Option<Set>) -> Result<Set> {
    let term = a.first_pos_or_all();
    if term.trim().is_empty() {
        bail!("usage: mesh <term> [-n 5] [--full] [--tree]");
    }
    let n = a.num::<usize>("limit")?.unwrap_or(5).clamp(1, 50);
    let full = a.flag("full");
    let tree = a.flag("tree");

    let q = term.trim();
    let mut note = None;
    let mut ids = search(ctx, q, n).await?;
    if ids.is_empty()
        && let Some(c) = spell(ctx, q).await?.filter(|c| !c.eq_ignore_ascii_case(q)) {
            ids = search(ctx, &c, n).await?;
            note = Some(format!("~ '{q}' -> '{c}'"));
        }
    if ids.is_empty() {
        bail!("no MeSH match for '{q}'");
    }

    let idstr = ids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let body = ctx.http.eutils("efetch.fcgi", &[("db", "mesh"), ("id", &idstr), ("retmode", "text"), ("rettype", "full")]).await?;
    if let Some(m) = body.split_once("<ERROR>").and_then(|(_, r)| r.split_once("</ERROR>")) {
        bail!("mesh: {}", crate::http::redact(m.0.trim()));
    }
    let mut entries = parse_entries(&body);
    if entries.len() == ids.len() {
        let uis = match ctx.http.eutils("esummary.fcgi", &[("db", "mesh"), ("id", &idstr), ("retmode", "json")]).await {
            Ok(b) => parse_meshui(&b),
            Err(_) => Default::default(),
        };
        for (e, id) in entries.iter_mut().zip(&ids) {
            e.ui = uis.get(id).cloned().unwrap_or_else(|| ui_of(*id));
        }
    }

    let mut out = String::new();
    if let Some(n) = note {
        out.push_str(&n);
        out.push('\n');
    }
    for e in &entries {
        render(&mut out, e, full, tree);
    }
    Ok(Set::text(out.trim_end()))
}

async fn search(ctx: &Ctx, term: &str, n: usize) -> Result<Vec<u32>> {
    let retmax = n.to_string();
    let body = ctx
        .http
        .eutils("esearch.fcgi", &[("db", "mesh"), ("term", term), ("retmode", "json"), ("retmax", &retmax), ("sort", "relevance")])
        .await?;
    parse_ids(&body)
}

fn json_error(v: &serde_json::Value) -> Option<&str> {
    v.as_object()?.iter().find(|(k, _)| k.eq_ignore_ascii_case("error")).and_then(|(_, e)| e.as_str())
}

fn parse_ids(body: &str) -> Result<Vec<u32>> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| anyhow!("mesh: bad esearch response"))?;
    if let Some(e) = json_error(&v).or_else(|| json_error(&v["esearchresult"])) {
        bail!("mesh: {}", crate::http::redact(&e.chars().take(120).collect::<String>()));
    }
    Ok(v["esearchresult"]["idlist"]
        .as_array()
        .map(|l| l.iter().filter_map(|x| x.as_str()?.parse().ok()).collect())
        .unwrap_or_default())
}

async fn spell(ctx: &Ctx, term: &str) -> Result<Option<String>> {
    let body = ctx.http.eutils("espell.fcgi", &[("db", "pubmed"), ("term", term)]).await?;
    Ok(body
        .split_once("<CorrectedQuery>")
        .and_then(|(_, r)| r.split_once("</CorrectedQuery>"))
        .map(|(s, _)| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string))
}

fn parse_meshui(body: &str) -> std::collections::HashMap<u32, String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return Default::default() };
    let Some(r) = v["result"].as_object() else { return Default::default() };
    r.iter()
        .filter_map(|(k, d)| Some((k.parse().ok()?, d["ds_meshui"].as_str().filter(|s| !s.is_empty())?.to_string())))
        .collect()
}

fn ui_of(id: u32) -> String {
    match id {
        68_000_000..=68_999_999 => format!("D{:06}", id - 68_000_000),
        67_000_000..=67_999_999 => format!("C{:06}", id - 67_000_000),
        _ => String::new(),
    }
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn is_header(l: &str) -> bool {
    l.starts_with("Year introduced:")
        || l.starts_with("Tree Number(s):")
        || l.starts_with("Registry Number:")
        || (l.ends_with(':') && l.len() < 40 && !l.contains(". "))
}

fn parse_entries(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut section = "";
    let mut in_scope = false;
    let mut tree: Vec<(usize, &str)> = Vec::new();
    for l in text.lines() {
        let t = l.trim_end();
        if let Some((_, rest)) = t.split_once(": ").filter(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
            finish_tree(out.last_mut(), &mut tree);
            let (name, supp) = match rest.strip_suffix(" [Supplementary Concept]") {
                Some(s) => (s, true),
                None => (rest, false),
            };
            out.push(Entry { name: name.to_string(), supp, ..Default::default() });
            section = "";
            in_scope = true;
            continue;
        }
        let Some(e) = out.last_mut() else { continue };
        if t.is_empty() {
            in_scope = false;
            continue;
        }
        let ind = indent(t);
        let body = t.trim_start();
        if ind == 0 {
            if let Some(y) = body.strip_prefix("Year introduced:") {
                e.year = y.split_whitespace().next().unwrap_or("").to_string();
                in_scope = false;
                section = "";
            } else if let Some(tn) = body.strip_prefix("Tree Number(s):") {
                e.trees = tn.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                in_scope = false;
                section = "";
            } else if is_header(body) {
                section = match body {
                    "Entry Terms:" => "syn",
                    "Pharmacologic Action:" => "pa",
                    "Heading Mapped to:" => "map",
                    _ => "skip",
                };
                in_scope = false;
            } else if in_scope {
                if !e.scope.is_empty() {
                    e.scope.push(' ');
                }
                e.scope.push_str(body);
            }
            continue;
        }
        if body == "All MeSH Categories" {
            finish_tree(Some(e), &mut tree);
            section = "tree";
            continue;
        }
        match section {
            "syn" => e.syns.push(body.to_string()),
            "pa" => e.pa.push(body.to_string()),
            "map" => e.maps.push(body.split_whitespace().collect::<Vec<_>>().join(" ")),
            "tree" => tree.push((ind, body)),
            _ => {}
        }
    }
    finish_tree(out.last_mut(), &mut tree);
    out
}

fn finish_tree(e: Option<&mut Entry>, tree: &mut Vec<(usize, &str)>) {
    let lines = std::mem::take(tree);
    let Some(e) = e else { return };
    let Some(i) = lines.iter().position(|(_, t)| *t == e.name) else { return };
    let ind = lines[i].0;
    if i > 1 {
        let p = lines[i - 1].1;
        if !e.up.iter().any(|x| x == p) {
            e.up.push(p.to_string());
        }
    }
    for (d, t) in &lines[i + 1..] {
        if *d <= ind {
            break;
        }
        if *d == lines[i + 1].0 && !e.down.iter().any(|x| x == t) {
            e.down.push(t.to_string());
        }
    }
}

fn key(s: &str) -> Vec<String> {
    let mut w: Vec<String> = s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| {
            let w = w.to_lowercase();
            w.strip_suffix('s').filter(|_| w.len() > 3).map(str::to_string).unwrap_or(w)
        })
        .collect();
    w.sort();
    w
}

fn distinct_syns<'a>(name: &str, syns: &'a [String]) -> Vec<&'a str> {
    let mut seen: HashSet<Vec<String>> = HashSet::new();
    seen.insert(key(name));
    syns.iter().filter(|s| seen.insert(key(s))).map(String::as_str).collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    if let Some(i) = t.rfind(' ').filter(|i| *i > max / 2) {
        t.truncate(i);
    }
    t.push('…');
    t
}

fn render(out: &mut String, e: &Entry, full: bool, tree: bool) {
    out.push_str(&e.name);
    if e.supp {
        out.push_str(" [supp]");
    }
    if !e.ui.is_empty() {
        let _ = write!(out, " {}", e.ui);
    }
    if !e.year.is_empty() {
        let _ = write!(out, " {}", e.year);
    }
    if !e.trees.is_empty() {
        let shown = if full { e.trees.len() } else { e.trees.len().min(TREE_CAP) };
        let _ = write!(out, " tree={}", e.trees[..shown].join(","));
        if shown < e.trees.len() {
            let _ = write!(out, " +{}", e.trees.len() - shown);
        }
    }
    out.push('\n');
    let syns = distinct_syns(&e.name, &e.syns);
    if !syns.is_empty() {
        let shown = if full { syns.len() } else { syns.len().min(SYN_CAP) };
        let _ = write!(out, "syn: {}", syns[..shown].join("; "));
        if shown < syns.len() {
            let _ = write!(out, "; +{}", syns.len() - shown);
        }
        out.push('\n');
    }
    if !e.maps.is_empty() {
        let _ = writeln!(out, "maps to: {}", e.maps.join("; "));
    }
    if !e.pa.is_empty() {
        let _ = writeln!(out, "pa: {}", e.pa.join("; "));
    }
    if !e.scope.is_empty() {
        let s = if full { e.scope.clone() } else { truncate(&e.scope, SCOPE_CAP) };
        let _ = writeln!(out, "scope: {s}");
    }
    if tree {
        if !e.up.is_empty() {
            let _ = writeln!(out, "broader: {}", e.up.join("; "));
        }
        if !e.down.is_empty() {
            let shown = if full { e.down.len() } else { e.down.len().min(NARROW_CAP) };
            let _ = write!(out, "narrower: {}", e.down[..shown].join("; "));
            if shown < e.down.len() {
                let _ = write!(out, "; +{}", e.down.len() - shown);
            }
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\n1: Myocardial Infarction\nNECROSIS of the MYOCARDIUM caused by an obstruction of the blood supply to the\nheart (CORONARY CIRCULATION).\nYear introduced: 1979\n\nSubheadings:\n    blood\n    urine\n\nTree Number(s): C14.280.647.500, C23.550.513.355.750\nEntry Terms:\n    Infarction, Myocardial\n    Infarctions, Myocardial\n    Myocardial Infarctions\n    Heart Attack\n    Heart Attacks\n    Cardiovascular Stroke\n\nSee Also:\n    Heart Rupture, Post-Infarction\n\n    All MeSH Categories\n        Diseases Category\n            Cardiovascular Diseases\n                Heart Diseases\n                    Myocardial Ischemia\n                        Myocardial Infarction\n                            Anterior Wall Myocardial Infarction\n                            MINOCA\n                                Deep Child\n\n    All MeSH Categories\n        Diseases Category\n            Pathologic Processes\n                Ischemia\n                    Infarction\n                        Myocardial Infarction\n                            Anterior Wall Myocardial Infarction\n                            Shock, Cardiogenic\n\n\n2: Jentadueto [Supplementary Concept]\nfixed combination of linagliptin plus metformin for the treatment of type 2\ndiabetes\nHeading Mapped to:\n    Purines\n    Quinazolines\n\n    All MeSH Categories\n\n3: Metformin\nA biguanide hypoglycemic agent.\n\nPharmacologic Action:\n    Hypoglycemic Agents\n";

    #[test]
    fn esearch_errors_are_not_empty_results() {
        assert_eq!(parse_ids(r#"{"esearchresult":{"idlist":["68009203"]}}"#).unwrap(), vec![68009203]);
        assert_eq!(parse_ids(r#"{"esearchresult":{"idlist":[]}}"#).unwrap(), Vec::<u32>::new());
        let e = parse_ids(r#"{"esearchresult":{"ERROR":"Search Backend failed"}}"#).unwrap_err().to_string();
        assert_eq!(e, "mesh: Search Backend failed");
        let e = parse_ids(r#"{"error":"API rate limit exceeded"}"#).unwrap_err().to_string();
        assert_eq!(e, "mesh: API rate limit exceeded");
        assert!(parse_ids("not json").is_err());
    }

    #[test]
    fn parses_descriptor_supp_and_drug() {
        let e = parse_entries(SAMPLE);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].name, "Myocardial Infarction");
        assert_eq!(e[0].year, "1979");
        assert_eq!(e[0].trees, vec!["C14.280.647.500", "C23.550.513.355.750"]);
        assert_eq!(e[0].scope, "NECROSIS of the MYOCARDIUM caused by an obstruction of the blood supply to the heart (CORONARY CIRCULATION).");
        assert_eq!(e[0].syns.len(), 6);
        assert_eq!(e[0].up, vec!["Myocardial Ischemia", "Infarction"]);
        assert_eq!(e[0].down, vec!["Anterior Wall Myocardial Infarction", "MINOCA", "Shock, Cardiogenic"]);
        assert!(e[1].supp);
        assert_eq!(e[1].maps, vec!["Purines", "Quinazolines"]);
        assert_eq!(e[1].scope, "fixed combination of linagliptin plus metformin for the treatment of type 2 diabetes");
        assert_eq!(e[2].pa, vec!["Hypoglycemic Agents"]);
        assert_eq!(e[2].scope, "A biguanide hypoglycemic agent.");
    }

    #[test]
    fn renders_compact() {
        let mut e = parse_entries(SAMPLE);
        e[0].ui = ui_of(68009203);
        let mut s = String::new();
        render(&mut s, &e[0], false, true);
        assert_eq!(
            s,
            "Myocardial Infarction D009203 1979 tree=C14.280.647.500,C23.550.513.355.750\nsyn: Heart Attack; Cardiovascular Stroke\nscope: NECROSIS of the MYOCARDIUM caused by an obstruction of the blood supply to the heart (CORONARY CIRCULATION).\nbroader: Myocardial Ischemia; Infarction\nnarrower: Anterior Wall Myocardial Infarction; MINOCA; Shock, Cardiogenic\n"
        );
    }

    #[test]
    fn meshui_from_esummary() {
        let body = r#"{"result":{"uids":["2052179","68009203","9"],"2052179":{"uid":"2052179","ds_meshui":"D000086382"},"68009203":{"ds_meshui":"D009203"},"9":{"ds_meshui":""}}}"#;
        let m = parse_meshui(body);
        assert_eq!(m.get(&2052179).map(String::as_str), Some("D000086382"));
        assert_eq!(m.get(&68009203).map(String::as_str), Some("D009203"));
        assert!(!m.contains_key(&9));
        assert!(parse_meshui("not json").is_empty());
    }

    #[test]
    fn ui_mapping() {
        assert_eq!(ui_of(68008687), "D008687");
        assert_eq!(ui_of(67585596), "C585596");
        assert_eq!(ui_of(2009902), "");
    }

    #[test]
    fn scope_truncates_on_word() {
        let t = truncate(&"word ".repeat(100), 300);
        assert!(t.chars().count() <= 301 && t.ends_with("word…"));
    }
}
