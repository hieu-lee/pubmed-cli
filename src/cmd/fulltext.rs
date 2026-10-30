use super::id::pmc_of;
use super::show::ids;
use crate::args::Args;
use crate::eutils;
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, bail};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const TTL: Duration = Duration::from_secs(30 * 86400);
const NOT_OA: &str = "no PMC full text (not OA)";
const SKIP: &[&str] = &[
    "fig", "fig-group", "table-wrap", "table-wrap-group", "disp-formula", "supplementary-material", "graphic",
    "inline-graphic", "media", "tex-math", "math", "label", "object-id", "permissions", "alt-text",
];

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Sec {
    path: String,
    keys: String,
    paras: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    review: bool,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Doc {
    title: String,
    pmid: String,
    secs: Vec<Sec>,
}

const BLOCKS: &[&str] = &[
    "p", "preformat", "code", "disp-quote", "list-item", "def-item", "verse-group", "statement", "speech",
];

fn in_block(stack: &[String]) -> bool {
    let from = stack.iter().rposition(|s| s == "sec").map_or(0, |i| i + 1);
    stack[from..].iter().any(|s| BLOCKS.contains(&s.as_str()))
}

fn tidy(s: &str) -> String {
    let mut out: Vec<char> = Vec::with_capacity(s.len());
    for c in s.chars() {
        if c.is_whitespace() {
            if !out.is_empty() && out.last() != Some(&' ') {
                out.push(' ');
            }
            continue;
        }
        if c == ')' || c == ']' {
            let open = if c == ')' { '(' } else { '[' };
            if let Some(i) = out.iter().rposition(|&x| x == open)
                && out[i + 1..].iter().all(|x| matches!(x, ' ' | ',' | ';')) {
                    out.truncate(i);
                    continue;
                }
        }
        out.push(c);
    }
    let s: String = out.into_iter().collect();
    s.trim_end().replace(" .", ".").replace(" ,", ",").replace("  ", " ")
}

fn decode_ref(r: &str) -> Option<char> {
    match r {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let n = r.strip_prefix('#')?;
            let v = match n.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(v)
        }
    }
}

fn sentence_case(t: String) -> String {
    if t.chars().any(char::is_lowercase) {
        return t;
    }
    let mut c = t.chars();
    match c.next() {
        Some(f) => f.to_string() + &c.as_str().to_lowercase(),
        None => t,
    }
}

fn attr(e: &BytesStart, name: &str) -> String {
    e.try_get_attribute(name)
        .ok()
        .flatten()
        .map(|a| a.value.into_owned())
        .unwrap_or_default()
}

struct Parser {
    doc: Doc,
    open: Vec<usize>,
    buf: String,
    review: bool,
}

impl Parser {
    fn flush_para(&mut self) {
        let text = tidy(&self.buf);
        self.buf.clear();
        if text.is_empty() {
            return;
        }
        let i = match self.open.last() {
            Some(&i) => i,
            None => {
                if self.doc.secs.last().is_none_or(|s| !s.path.is_empty() || !s.keys.is_empty()) {
                    self.doc.secs.push(Sec { review: self.review, ..Default::default() });
                }
                self.doc.secs.len() - 1
            }
        };
        self.doc.secs[i].paras.push(text);
    }

    fn start_sec(&mut self, ty: &str) {
        let parent = self.open.last().map(|&i| self.doc.secs[i].keys.as_str()).unwrap_or("");
        let keys = format!("{parent}|{}", ty.to_lowercase());
        self.doc.secs.push(Sec { keys, review: self.review, ..Default::default() });
        self.open.push(self.doc.secs.len() - 1);
    }

    fn end_sec(&mut self) {
        self.open.pop();
        if let Some(&pi) = self.open.last() {
            let (path, keys) = (self.doc.secs[pi].path.clone(), self.doc.secs[pi].keys.clone());
            self.doc.secs.push(Sec { path, keys, paras: Vec::new(), review: self.review });
            let last = self.doc.secs.len() - 1;
            if let Some(slot) = self.open.last_mut() {
                *slot = last;
            }
        }
    }

    fn set_title(&mut self) {
        let t = sentence_case(tidy(&self.buf));
        self.buf.clear();
        let Some(&i) = self.open.last() else { return };
        let parent = self.open.iter().rev().nth(1).map(|&p| self.doc.secs[p].path.clone()).unwrap_or_default();
        let s = &mut self.doc.secs[i];
        s.keys.push('|');
        s.keys.push_str(&t.to_lowercase());
        s.path = if parent.is_empty() { t } else { format!("{parent} / {t}") };
    }
}

pub fn parse_doc(xml: &str) -> Result<Doc> {
    let mut r = Reader::from_str(xml);
    let mut p = Parser { doc: Doc::default(), open: Vec::new(), buf: String::new(), review: false };
    let mut stack: Vec<String> = Vec::new();
    let mut skip_at: Option<usize> = None;
    let mut in_body = false;
    let mut front_title = false;
    let mut front_pmid = false;
    let mut sub_title = false;
    let mut review_title = String::new();
    let mut review_wrap = false;
    let mut embedded = 0usize;
    let mut sups: Vec<usize> = Vec::new();
    loop {
        match r.read_event()? {
            Event::Eof => {
                if !stack.is_empty() {
                    bail!("truncated XML");
                }
                break;
            }
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_string();
                if skip_at.is_none() {
                    if name == "sub-article" || name == "response" {
                        embedded += 1;
                        p.review = true;
                        review_title.clear();
                    } else if name == "body" {
                        in_body = true;
                        if p.review {
                            p.start_sec("review");
                            let i = p.doc.secs.len() - 1;
                            p.doc.secs[i].path = format!("[review] {}", review_title);
                            review_wrap = true;
                        }
                    } else if !in_body {
                        if p.review && name == "article-title" && review_title.is_empty() {
                            sub_title = true;
                            p.buf.clear();
                        } else if name == "article-title"
                            && embedded == 0
                            && p.doc.title.is_empty()
                            && stack.iter().any(|s| s == "title-group")
                            && !stack.iter().any(|s| matches!(s.as_str(), "ref-list" | "related-article"))
                        {
                            front_title = true;
                            p.buf.clear();
                        } else if name == "article-id"
                            && embedded == 0
                            && p.doc.pmid.is_empty()
                            && attr(&e, "pub-id-type") == "pmid"
                            && !stack.iter().any(|s| matches!(s.as_str(), "ref-list" | "related-article"))
                        {
                            front_pmid = true;
                            p.buf.clear();
                        }
                    } else if SKIP.contains(&name.as_str()) || (name == "xref" && attr(&e, "ref-type") == "bibr") {
                        skip_at = Some(stack.len());
                    } else {
                        if BLOCKS.contains(&name.as_str()) && !in_block(&stack) {
                            p.flush_para();
                        }
                        match name.as_str() {
                            "sec" => {
                                if stack.iter().any(|s| s == "p") {
                                    p.flush_para();
                                }
                                p.start_sec(&attr(&e, "sec-type"))
                            }
                            "title" if stack.last().is_some_and(|s| s == "sec") => p.buf.clear(),
                            "sup" => sups.push(p.buf.len()),
                            _ => {}
                        }
                    }
                }
                stack.push(name);
            }
            Event::End(_) => {
                let Some(name) = stack.pop() else { continue };
                if skip_at == Some(stack.len()) {
                    skip_at = None;
                    continue;
                }
                if skip_at.is_some() {
                    continue;
                }
                match name.as_str() {
                    "sub-article" | "response" => {
                        embedded = embedded.saturating_sub(1);
                        p.review = embedded > 0;
                    }
                    "article-title" if sub_title => {
                        review_title = tidy(&p.buf);
                        p.buf.clear();
                        sub_title = false;
                    }
                    "body" => {
                        if review_wrap {
                            p.flush_para();
                            p.open.pop();
                            review_wrap = false;
                        }
                        in_body = false;
                    }
                    "article-title" if front_title => {
                        p.doc.title = tidy(&p.buf);
                        p.buf.clear();
                        front_title = false;
                    }
                    "article-id" if front_pmid => {
                        p.doc.pmid = p.buf.trim().to_string();
                        p.buf.clear();
                        front_pmid = false;
                    }
                    _ if !in_body => {}
                    "sec" => p.end_sec(),
                    "title" if stack.last().is_some_and(|s| s == "sec") => p.set_title(),
                    n if BLOCKS.contains(&n) => {
                        if in_block(&stack) {
                            p.buf.push(' ');
                        } else {
                            p.flush_para();
                        }
                    }
                    "sup" => {
                        if let Some(m) = sups.pop()
                            && p.buf.len() > m {
                                p.buf.insert(m, '^');
                            }
                    }
                    _ => {}
                }
            }
            Event::Text(t) if skip_at.is_none() && (in_body || front_title || front_pmid || sub_title) => {
                p.buf.push_str(&t.xml10_content());
            }
            Event::CData(t) if skip_at.is_none() && (in_body || front_title || front_pmid || sub_title) => {
                p.buf.push_str(&t.xml10_content());
            }
            Event::GeneralRef(g) if skip_at.is_none() && (in_body || front_title || front_pmid || sub_title) => {
                if let Some(c) = decode_ref(&g) {
                    p.buf.push(c);
                }
            }
            _ => {}
        }
    }
    p.doc.secs.retain(|s| !s.paras.is_empty());
    Ok(p.doc)
}

fn sec_match(keys: &str, q: &str) -> bool {
    let q = q.trim().to_lowercase();
    let stem = q.trim_end_matches('s');
    let alias: &[&str] = match stem {
        "method" => &["material", "procedure", "experimental", "design"],
        "intro" | "introduction" => &["background"],
        "result" => &["finding"],
        "conclusion" => &["summary"],
        _ => &[],
    };
    keys.contains(stem) || alias.iter().any(|a| keys.contains(a))
}

pub fn render_doc(d: &Doc, secs: &[String], greps: &[String], reviews: bool) -> String {
    let mut out = String::new();
    let mut last_head = "";
    for s in &d.secs {
        if s.review && !reviews {
            continue;
        }
        if !secs.is_empty() && !secs.iter().any(|q| sec_match(&s.keys, q)) {
            continue;
        }
        let mut head = false;
        for para in &s.paras {
            if !greps.is_empty() {
                let low = para.to_lowercase();
                if !greps.iter().any(|g| low.contains(g.as_str())) {
                    continue;
                }
            }
            if !head {
                head = true;
                if !s.path.is_empty() && s.path != last_head {
                    last_head = &s.path;
                    out.push_str("## ");
                    out.push_str(&s.path);
                    out.push('\n');
                }
            }
            out.push_str(para);
            out.push('\n');
        }
    }
    out
}

async fn fetch_doc(ctx: &Ctx, pmc: &str) -> Result<Doc> {
    if let Some(b) = ctx.http.cache_get("pmc2", pmc, TTL).await
        && let Ok(d) = serde_json::from_slice(&b) {
            return Ok(d);
        }
    let num = pmc.trim_start_matches("PMC");
    let xml = ctx.http.eutils("efetch.fcgi", &[("db", "pmc"), ("id", num), ("rettype", "xml")]).await?;
    if xml.contains("<error ") {
        bail!(NOT_OA);
    }
    let doc = parse_doc(&xml)?;
    if doc.secs.iter().all(|s| s.review) {
        bail!(NOT_OA);
    }
    ctx.http.cache_put("pmc2", pmc, &serde_json::to_vec(&doc)?).await;
    Ok(doc)
}

fn pmc_token(t: &str) -> Option<String> {
    let rest = t.trim();
    let rest = rest.split_once(':').filter(|(p, _)| p.eq_ignore_ascii_case("pmc") || p.eq_ignore_ascii_case("pmcid")).map_or(rest, |(_, r)| r);
    let digits = rest.get(..3).filter(|p| p.eq_ignore_ascii_case("pmc")).map_or(rest, |_| &rest[3..]);
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && (digits.len() != rest.len() || t.contains(':')))
        .then(|| format!("PMC{digits}"))
}

async fn targets(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Vec<(Option<u32>, String)>> {
    let mut out: Vec<(Option<u32>, String)> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    for t in &a.pos {
        match pmc_token(t) {
            Some(p) => out.push((None, p)),
            None => other.push(t.clone()),
        }
    }
    let pmids = if !other.is_empty() {
        eutils::resolve(ctx, &other).await?
    } else if a.pos.is_empty() {
        ids(ctx, a, input.as_ref()).await?
    } else {
        Vec::new()
    };
    if !pmids.is_empty() {
        let mut set = Set::from_ids(pmids.clone());
        eutils::load(ctx, &mut set).await?;
        let mut known: std::collections::HashMap<u32, String> =
            set.arts.iter().filter(|x| !x.pmc.is_empty()).map(|x| (x.pmid, x.pmc.clone())).collect();
        let missing: Vec<u32> = pmids.iter().copied().filter(|p| !known.contains_key(p)).collect();
        if !missing.is_empty() {
            known.extend(pmc_of(ctx, &missing).await?);
        }
        out.extend(pmids.iter().map(|p| (Some(*p), known.remove(p).unwrap_or_default())));
    }
    if out.is_empty() {
        bail!("no ids");
    }
    Ok(out)
}

pub async fn run(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Set> {
    let secs = a.csv("sec");
    let greps: Vec<String> = a.all("grep").iter().map(|g| g.to_lowercase()).collect();
    let chars = a.num::<usize>("chars")?;
    let reviews = a.flag("reviews");
    if a.pos.is_empty() && input.as_ref().is_some_and(|s| s.ids.is_empty() && s.table.is_none() && s.text.is_none()) {
        return Ok(Set { total: Some(0), ..Default::default() });
    }
    let tg = targets(ctx, a, input).await?;
    let docs = futures::future::join_all(tg.iter().map(|(_, pmc)| async move {
        if pmc.is_empty() { Err(anyhow::anyhow!(NOT_OA)) } else { fetch_doc(ctx, pmc).await }
    }))
    .await;
    let mut out = String::new();
    let mut first_err = None;
    let mut ok = 0;
    for ((pmid, pmc), d) in tg.iter().zip(docs) {
        if !out.is_empty() {
            out.push('\n');
        }
        match d {
            Ok(d) => {
                ok += 1;
                let pmid = pmid.map(|p| p.to_string()).unwrap_or(d.pmid.clone());
                out.push_str(&format!("# {pmc} pmid={pmid} {}\n", d.title));
                let body = render_doc(&d, &secs, &greps, reviews);
                out.push_str(if body.is_empty() { "(no matching text)\n" } else { &body });
            }
            Err(e) => {
                let who = pmid.map(|p| p.to_string()).unwrap_or_else(|| pmc.clone());
                out.push_str(&format!("# {who} {}\n", crate::http::redact(&e.to_string())));
                first_err.get_or_insert(e);
            }
        }
    }
    if ok == 0
        && let Some(e) = first_err
    {
        return Err(e);
    }
    let mut set = Set::text(out.trim_end());
    if let Some(n) = chars {
        let t = set.text.as_mut().unwrap();
        if let Some((i, _)) = t.char_indices().nth(n) {
            let full = t.chars().count();
            t.truncate(i);
            t.push('\u{2026}');
            set.notes.push(format!("truncated chars={n}/{full}"));
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0"?><pmc-articleset><article><front><article-meta>
<article-id pub-id-type="pmid">42</article-id>
<title-group><article-title>A <italic>big</italic> study</article-title></title-group>
<abstract><p>Abstract text.</p></abstract></article-meta></front>
<body><sec sec-type="intro"><title>INTRODUCTION</title><p>GenBank (<xref ref-type="bibr" rid="B1">1</xref>) is big (<xref ref-type="bibr" rid="B2">2</xref>, <xref ref-type="bibr" rid="B3">3</xref>). It grew 10<sup>6</sup>-fold &amp; more.</p></sec>
<sec><title>Materials and Methods</title><p>Top.</p>
<sec><title>Patients</title><p>We enrolled <xref ref-type="fig" rid="F1">Figure 1</xref> 5 people.</p>
<fig id="F1"><label>Figure 1</label><caption><p>Cap text</p></caption></fig>
<table-wrap id="T1"><caption><p>Table cap</p></caption><table><tr><td>1</td></tr></table></table-wrap></sec></sec>
<sec sec-type="results"><title>Results</title><p>p&lt;0.05 observed.</p><p>Second para.</p></sec></body>
<back><ref-list><ref><label>1</label></ref></ref-list></back></article></pmc-articleset>"#;

    #[test]
    fn front_matter_ignores_embedded_articles() {
        let x = |parent_pmid: &str| {
            format!(
                "<pmc-articleset><article><front><article-meta>{parent_pmid}<title-group><article-title>Main paper</article-title></title-group></article-meta></front><body><sec><title>Methods</title><p>We did it.</p></sec></body>\
<sub-article article-type=\"reviewer-report\"><front-stub><article-id pub-id-type=\"pmid\">222</article-id><title-group><article-title>Decision letter</article-title></title-group></front-stub><body><p>No.</p></body></sub-article>\
<response><front-stub><article-id pub-id-type=\"pmid\">333</article-id><title-group><article-title>Reply</article-title></title-group></front-stub><body><p>Yes.</p></body></response>\
<back><ref-list><ref><element-citation><article-title>Cited work</article-title></element-citation></ref></ref-list><related-article><article-id pub-id-type=\"pmid\">444</article-id></related-article></back></article></pmc-articleset>"
            )
        };
        let d = parse_doc(&x("<article-id pub-id-type=\"pmid\">111</article-id>")).unwrap();
        assert_eq!((d.pmid.as_str(), d.title.as_str()), ("111", "Main paper"));
        let d = parse_doc(&x("")).unwrap();
        assert_eq!((d.pmid.as_str(), d.title.as_str()), ("", "Main paper"));
        assert_eq!(render_doc(&d, &[], &[], false), "## Methods\nWe did it.\n");
        assert!(render_doc(&d, &[], &[], true).contains("Yes."));
    }

    #[test]
    fn truncated_doc_is_an_error() {
        let cut = &XML[..XML.find("<sec sec-type=\"results\">").unwrap()];
        assert_eq!(parse_doc(cut).unwrap_err().to_string(), "truncated XML");
    }

    #[test]
    fn sub_articles_are_reviews_only_when_asked() {
        let x = "<pmc-articleset><article><front><article-meta><title-group><article-title>Main paper</article-title></title-group></article-meta></front><body><sec><title>Methods</title><p>We did it.</p></sec></body><sub-article article-type=\"reviewer-report\"><front-stub><title-group><article-title>Decision letter</article-title></title-group></front-stub><body><p>Reviewer says no.</p><sec><title>Concerns</title><p>Too small.</p></sec></body></sub-article></article></pmc-articleset>";
        let d = parse_doc(x).unwrap();
        assert_eq!(d.title, "Main paper");
        assert_eq!(render_doc(&d, &[], &[], false), "## Methods\nWe did it.\n");
        assert_eq!(
            render_doc(&d, &[], &[], true),
            "## Methods\nWe did it.\n## [review] Decision letter\nReviewer says no.\n## [review] Decision letter / Concerns\nToo small.\n"
        );
    }

    #[test]
    fn negative_markers_survive_tidy() {
        assert_eq!(tidy("ER(-) breast and CD4(-) CD8(+) cells ( ) here (, )."), "ER(-) breast and CD4(-) CD8(+) cells here.");
    }

    #[test]
    fn boxed_section_inside_paragraph_keeps_surrounding_text() {
        let x = "<pmc-articleset><article><body><sec><title>Intro</title><p>Important lead-in.<boxed-text><sec><title>Key messages</title><p>Box contents.</p></sec></boxed-text>Tail.</p></sec></body></article></pmc-articleset>";
        let d = parse_doc(x).unwrap();
        let all: Vec<&str> = d.secs.iter().flat_map(|s| s.paras.iter().map(String::as_str)).collect();
        assert_eq!(all, ["Important lead-in.", "Box contents.", "Tail."]);
        assert_eq!(render_doc(&d, &[], &[], false), "## Intro\nImportant lead-in.\n## Intro / Key messages\nBox contents.\n## Intro\nTail.\n");
    }

    #[test]
    fn non_paragraph_blocks_are_kept() {
        let x = "<pmc-articleset><article><body><sec><title>Methods</title><preformat>dose = 5</preformat><p>Done.</p><disp-quote><p>Quoted.</p></disp-quote><list><list-item><p>One</p><p>Two</p></list-item></list></sec></body></article></pmc-articleset>";
        assert_eq!(parse_doc(x).unwrap().secs[0].paras, ["dose = 5", "Done.", "Quoted.", "One Two"]);
        let only = "<pmc-articleset><article><body><sec><title>M</title><preformat>x=1</preformat></sec></body></article></pmc-articleset>";
        assert_eq!(parse_doc(only).unwrap().secs[0].paras, ["x=1"]);
    }

    #[test]
    fn cdata_paragraph_kept() {
        let x = "<pmc-articleset><article><body><sec><title>Results</title><p><![CDATA[p<0.05 here]]></p></sec></body></article></pmc-articleset>";
        assert_eq!(parse_doc(x).unwrap().secs[0].paras, ["p<0.05 here"]);
    }

    #[test]
    fn parses_sections() {
        let d = parse_doc(XML).unwrap();
        assert_eq!(d.title, "A big study");
        assert_eq!(d.pmid, "42");
        let paths: Vec<&str> = d.secs.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["Introduction", "Materials and Methods", "Materials and Methods / Patients", "Results"]);
        assert_eq!(d.secs[0].paras[0], "GenBank is big. It grew 10^6-fold & more.");
        assert_eq!(d.secs[2].paras, ["We enrolled Figure 1 5 people."]);
    }

    #[test]
    fn filters() {
        let d = parse_doc(XML).unwrap();
        let m = render_doc(&d, &["methods".into()], &[], false);
        assert_eq!(m, "## Materials and Methods\nTop.\n## Materials and Methods / Patients\nWe enrolled Figure 1 5 people.\n");
        let g = render_doc(&d, &[], &["p<0.05".into()], false);
        assert_eq!(g, "## Results\np<0.05 observed.\n");
        assert!(render_doc(&d, &["intro".into()], &[], false).starts_with("## Introduction\n"));
    }

    #[test]
    fn pmc_tokens() {
        assert_eq!(pmc_token("PMC123").as_deref(), Some("PMC123"));
        assert_eq!(pmc_token("pmc:123").as_deref(), Some("PMC123"));
        assert_eq!(pmc_token("123"), None);
        assert_eq!(pmc_token("doi:10.1/x"), None);
    }
}
